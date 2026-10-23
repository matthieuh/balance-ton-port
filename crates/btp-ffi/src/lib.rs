//! UniFFI bindings for the macOS app: one `Core` object that owns the watcher
//! for the app's lifetime. Every method may block on disk and process I/O, so
//! the app calls them off the main thread; `Core` is `Send + Sync`.

uniffi::setup_scaffolding!();

use btp_core::agents::AgentKind;
use btp_core::config::{data_dir, read_config, save_config, Config};
use btp_core::display::{
    card_reason, format_idle, free_is_low, server_state, session_ended_body, started_label,
    status_text, status_text_observed, usage_line, NeedsYou, SuggestedAction, ACTIVE_WINDOW_MS,
};
use btp_core::history::{health, EventKind, Health, LeakCause, ServerEvent};
use btp_core::labels::{framework as framework_of, location_label, tool_label};
use btp_core::model::{KillReason, KillTarget, ServerKey};
use btp_core::observed::ObservedServer;
use btp_core::platform::{MacOs, Platform};
use btp_core::probe::{probe_port, ProbeOutcome};
use btp_core::restart::RestartBlocked;
use btp_core::sessions::default_config_dirs;
use btp_core::state::{read_state_readonly, with_state, KillRecord, SessionEndedGroup};
use btp_core::watcher::{acquire_watch_lock, now_ms, ServerView, TickReport, WatchLock, Watcher};
use btp_core::worktree::worktree_root;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Owner written into `watch.lock` while the app holds it.
pub use btp_core::watcher::APP_OWNER;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum BtpError {
    #[error("{message}")]
    Io { message: String },
}

impl From<io::Error> for BtpError {
    fn from(e: io::Error) -> Self {
        BtpError::Io {
            message: e.to_string(),
        }
    }
}

/// The "Needs you" card of a server: why it needs a decision and the one
/// action it suggests.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CardFfi {
    /// `leak`, `about_to_stop`, `session_ended_open` or `idle_alert`.
    pub kind: String,
    /// The one-sentence reason (`Growing fast: +600 MB in 10 min`).
    pub reason: String,
    /// `restart`, `postpone` or `stop`.
    pub action: String,
}

impl From<&NeedsYou> for CardFfi {
    fn from(n: &NeedsYou) -> Self {
        let kind = match n {
            NeedsYou::Leak { .. } => "leak",
            NeedsYou::AboutToStop { .. } => "about_to_stop",
            NeedsYou::SessionEndedOpen => "session_ended_open",
            NeedsYou::IdleAlert { .. } => "idle_alert",
        };
        let action = match n.suggested_action() {
            SuggestedAction::Restart => "restart",
            SuggestedAction::Postpone => "postpone",
            SuggestedAction::Stop => "stop",
        };
        CardFfi {
            kind: kind.to_string(),
            reason: card_reason(n),
            action: action.to_string(),
        }
    }
}

/// One server as the menu shows it: tracked (an agent started it) or
/// observed (`managed` false: no agent, never stopped automatically).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ServerViewFfi {
    pub pid: i32,
    pub start_time: u64,
    pub port: u16,
    /// Every port the server's process tree listens on, sorted.
    pub ports: Vec<u16>,
    pub command: String,
    /// A readable name for the server (`Expo (Metro)`, `Vite`, `Node x.js`).
    pub tool_label: String,
    /// Full worktree path (empty for an observed server whose folder
    /// cannot be read).
    pub worktree: String,
    pub worktree_name: String,
    /// `<repo> › <worktree>`, the repo dir name, or a `~`-shortened path.
    pub location_label: String,
    /// The session's display name (empty for an observed server).
    pub session_name: String,
    /// `claude_code`, `codex`, or empty for an observed server.
    pub agent: String,
    /// `Claude` or `Codex`, the word shown before `session`; `no agent` for
    /// an observed server.
    pub agent_label: String,
    /// When the server process started, Unix milliseconds.
    pub started_ms: u64,
    /// `started 4 min ago`, as of the pass that produced this view.
    pub started_label: String,
    /// Always `false` for an observed server, which has no session.
    pub session_alive: bool,
    pub idle_ms: u64,
    pub kill_in_ms: Option<u64>,
    /// `active`, `idle` or `session dead`, as `btp list` prints it.
    pub state: String,
    /// An agent started it: the automatic rules apply.
    pub managed: bool,
    /// Matches an exclusion: shown, never stoppable (observed only).
    pub excluded: bool,
    /// `Vite`, `Next.js`, from the command and `package.json`.
    pub framework: Option<String>,
    /// The page title, else the package name, else `tool_label`.
    pub title: String,
    /// `icon` returns the page's favicon.
    pub has_icon: bool,
    /// The git branch of its worktree.
    pub branch: Option<String>,
    /// Memory of the whole process tree at the last pass.
    pub rss_bytes: u64,
    /// CPU of the whole tree; 100 = one core.
    pub cpu_pct: f32,
    /// Memory growth over the last 10 minutes (negative when it shrank).
    pub growth_bytes: i64,
    /// `green`, `amber` or `red`.
    pub health: String,
    /// A connection is open on it (the app's own probes excluded).
    pub in_use: bool,
    /// `in use`, `kept by Claude: "<reason>"`, `kept by you`, `stops in N
    /// min`, `excluded`, else `idle N min`.
    pub status_text: String,
    pub card: Option<CardFfi>,
    /// The leak its card shows: `size` (above the memory threshold) or
    /// `growth` (grew past the growth threshold in 10 min); `None` without
    /// a leak card.
    pub leak_kind: Option<String>,
    /// Restart is possible: still running, folder exists, environment
    /// readable, not excluded.
    pub can_restart: bool,
    /// "Postpone 1 h" holds its stop now.
    pub postponed: bool,
    pub uptime_ms: u64,
    /// Processes in its tree (`processes` lists them).
    pub process_count: u32,
}

/// What a pass reads about a server beyond its row: memory, page, git and
/// restart facts, its card.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub rss_bytes: u64,
    pub cpu_pct: f32,
    pub growth_bytes: i64,
    pub health: Health,
    pub framework: Option<String>,
    pub package_name: Option<String>,
    pub branch: Option<String>,
    pub page_title: Option<String>,
    pub has_icon: bool,
    pub can_restart: bool,
    pub process_count: u32,
    pub card: Option<CardFfi>,
    pub leak_kind: Option<String>,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            rss_bytes: 0,
            cpu_pct: 0.0,
            growth_bytes: 0,
            health: Health::Green,
            framework: None,
            package_name: None,
            branch: None,
            page_title: None,
            has_icon: false,
            can_restart: false,
            process_count: 0,
            card: None,
            leak_kind: None,
        }
    }
}

fn health_id(h: Health) -> &'static str {
    match h {
        Health::Green => "green",
        Health::Amber => "amber",
        Health::Red => "red",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct KillRecordFfi {
    pub port: u16,
    pub command: String,
    /// A readable name for the command (`Vite`, `Node x.js`).
    pub tool_label: String,
    /// Full worktree path.
    pub worktree: String,
    /// Last path component of `worktree`.
    pub worktree_name: String,
    /// `worktree deleted`, `dead session + idle 23 min` or `manual`.
    pub reason: String,
    pub at_ms: u64,
}

/// The machine's memory split for the usage bar.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MemoryBarFfi {
    /// Every server shown, tracked and observed.
    pub dev_bytes: u64,
    /// Everything else in use.
    pub apps_bytes: u64,
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// `4.8 GB dev servers (13%) · apps 26.8 · free 4.4 of 36 GB`.
    pub line: String,
    /// Free memory is under 10 % of the machine's.
    pub free_low: bool,
}

impl MemoryBarFfi {
    /// `dev` bytes of servers on a machine with `total` bytes, `used` in use.
    pub fn new(dev: u64, total: u64, used: u64) -> Self {
        MemoryBarFfi {
            dev_bytes: dev,
            apps_bytes: used.saturating_sub(dev),
            free_bytes: total.saturating_sub(used),
            total_bytes: total,
            line: usage_line(dev, total, used),
            free_low: free_is_low(total, used),
        }
    }
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct TickReportFfi {
    /// Tracked servers, then observed ones (`managed` false).
    pub servers: Vec<ServerViewFfi>,
    /// Servers due an alert, reported on every pass until `mark_alerted`.
    pub alerts: Vec<ServerViewFfi>,
    pub killed: Vec<KillRecordFfi>,
    /// Why `config.toml` could not be used, if it could not (the pass ran on
    /// the last valid config, or the defaults).
    pub config_error: Option<String>,
    pub memory: MemoryBarFfi,
    /// Servers (tracked or observed) whose leak is due a notification,
    /// reported on every pass until `mark_leak_notified`.
    pub leaks: Vec<ServerViewFfi>,
}

/// One memory and CPU sample of a server's tree.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SampleFfi {
    pub at_ms: u64,
    pub rss_bytes: u64,
    pub cpu_pct: f32,
}

/// One event of a server's life, for its chart.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct EventFfi {
    pub at_ms: u64,
    /// `started`, `browser_connected`, `browser_closed`, `session_ended`,
    /// `alert`, `restarted` or `postponed`.
    pub kind: String,
    /// `started by Claude`, `browser connected`, `postponed 1 h`...
    pub label: String,
}

impl From<&ServerEvent> for EventFfi {
    fn from(e: &ServerEvent) -> Self {
        let (kind, label) = match &e.kind {
            EventKind::Started { agent: Some(a) } => {
                ("started", format!("started by {}", a.label()))
            }
            EventKind::Started { agent: None } => ("started", "started".to_string()),
            EventKind::BrowserConnected => ("browser_connected", "browser connected".to_string()),
            EventKind::BrowserClosed => ("browser_closed", "browser closed".to_string()),
            EventKind::SessionEnded => ("session_ended", "session ended".to_string()),
            EventKind::Alert { .. } => ("alert", "alert".to_string()),
            EventKind::Restarted => ("restarted", "restarted".to_string()),
            EventKind::Postponed => ("postponed", "postponed 1 h".to_string()),
        };
        EventFfi {
            at_ms: e.at_ms,
            kind: kind.to_string(),
            label,
        }
    }
}

/// One process of a server's tree.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProcRowFfi {
    pub pid: i32,
    pub command: String,
    pub rss_bytes: u64,
}

/// A blocked restart as the row shows it.
pub fn restart_blocked_label(b: RestartBlocked) -> &'static str {
    match b {
        RestartBlocked::Gone | RestartBlocked::SpawnFailed => "Could not start it again",
        RestartBlocked::CwdGone => "The folder no longer exists",
        RestartBlocked::EnvUnreadable | RestartBlocked::ArgvUnreadable => {
            "Cannot read its environment"
        }
        RestartBlocked::PortStillTaken => "The port is still taken",
        RestartBlocked::Excluded => "It matches an exclusion now",
        RestartBlocked::Busy => "Already restarting",
    }
}

/// `cd '<folder>' && claude --resume <id>` (Codex: `codex resume <id>`),
/// single quotes in the folder escaped. `None` for an empty id, or one with
/// characters a shell would read (it is pasted into a terminal).
pub fn resume_command(agent: AgentKind, folder: &Path, id: &str) -> Option<String> {
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
    if id.is_empty() || !id.chars().all(safe) {
        return None;
    }
    let folder = folder.display().to_string().replace('\'', r"'\''");
    let resume = match agent {
        AgentKind::ClaudeCode => format!("claude --resume {id}"),
        AgentKind::Codex => format!("codex resume {id}"),
    };
    Some(format!("cd '{folder}' && {resume}"))
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConfigFfi {
    pub dead_session_idle_s: u64,
    pub alert_after_s: u64,
    pub kill_grace_s: u64,
    pub scan_interval_s: u64,
    pub claude_config_dirs: Vec<String>,
    pub exclude_commands: Vec<String>,
    /// Ports never tracked or killed.
    pub exclude_ports: Vec<u16>,
    /// A server above this much memory (MB) leaks.
    pub leak_memory_mb: u64,
    /// A server growing by more than this (MB) in 10 minutes leaks.
    pub leak_growth_mb: u64,
}

/// One server a session end left running.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct GroupServerFfi {
    pub pid: i32,
    pub start_time: u64,
    pub port: u16,
    pub tool_label: String,
    /// A connection kept it open (a browser tab).
    pub in_use: bool,
}

/// One ended Claude Code session whose kept servers are not notified yet.
/// `session_pid`, `session_proc_start` and `at_ms` identify it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SessionEndedFfi {
    pub session_pid: i32,
    pub session_proc_start: String,
    pub at_ms: u64,
    pub session_name: String,
    /// The kept servers still tracked.
    pub servers: Vec<GroupServerFfi>,
    /// The notification body, the same text the hook posts.
    pub body: String,
}

impl SessionEndedFfi {
    /// `g` as the app notifies it; `dead_idle_s` is the dead-session delay.
    pub fn at(g: &SessionEndedGroup, dead_idle_s: u64) -> Self {
        SessionEndedFfi {
            session_pid: g.session_pid,
            session_proc_start: g.session_proc_start.clone(),
            at_ms: g.at_ms,
            session_name: g.session_name.clone(),
            servers: g
                .still_open
                .iter()
                .map(|s| GroupServerFfi {
                    pid: s.key.pid,
                    start_time: s.key.start_time,
                    port: s.port,
                    tool_label: s.tool_label.clone(),
                    in_use: s.in_use,
                })
                .collect(),
            body: session_ended_body(g, dead_idle_s),
        }
    }
}

/// The reason as the app shows it.
pub fn reason_label(r: &KillReason) -> String {
    match r {
        KillReason::WorktreeDeleted => "worktree deleted".to_string(),
        KillReason::DeadSessionIdle { idle_ms } => {
            format!("dead session + idle {}", format_idle(*idle_ms))
        }
        KillReason::Manual => "manual".to_string(),
        KillReason::SessionEnded => "session ended".to_string(),
        KillReason::Other => "other".to_string(),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The serialized name of an agent kind (`claude_code`, `codex`).
fn agent_id(a: AgentKind) -> &'static str {
    match a {
        AgentKind::ClaudeCode => "claude_code",
        AgentKind::Codex => "codex",
    }
}

/// The page title, else the package name, else the tool label.
fn title_of(l: &Live, tool_label: &str) -> String {
    l.page_title
        .clone()
        .or_else(|| l.package_name.clone())
        .unwrap_or_else(|| tool_label.to_string())
}

impl ServerViewFfi {
    /// A tracked server `v`, with `l`, as the menu shows it at `now_ms`.
    pub fn tracked(v: &ServerView, l: &Live, now_ms: u64) -> Self {
        ServerViewFfi {
            pid: v.key.pid,
            start_time: v.key.start_time,
            port: v.port,
            ports: v.ports.clone(),
            command: v.command.clone(),
            tool_label: v.tool_label.clone(),
            worktree: v.worktree.display().to_string(),
            worktree_name: v.worktree_name.clone(),
            location_label: v.location_label.clone(),
            session_name: v.session_name.clone(),
            agent: agent_id(v.agent).to_string(),
            agent_label: v.agent.label().to_string(),
            started_ms: v.started_ms,
            started_label: started_label(v.started_ms, now_ms),
            session_alive: v.session_alive,
            idle_ms: v.idle_ms,
            kill_in_ms: v.kill_in_ms,
            state: server_state(v).to_string(),
            managed: true,
            excluded: false,
            framework: l.framework.clone(),
            title: title_of(l, &v.tool_label),
            has_icon: l.has_icon,
            branch: l.branch.clone(),
            rss_bytes: l.rss_bytes,
            cpu_pct: l.cpu_pct,
            growth_bytes: l.growth_bytes,
            health: health_id(l.health).to_string(),
            in_use: v.in_use,
            status_text: status_text(v),
            card: l.card.clone(),
            leak_kind: l.leak_kind.clone(),
            can_restart: l.can_restart,
            postponed: v.postponed_until_ms.is_some_and(|u| u > now_ms),
            uptime_ms: now_ms.saturating_sub(v.started_ms),
            process_count: l.process_count,
        }
    }

    /// An observed server `o` (no agent), with `l`, as the menu shows it
    /// at `now_ms`: a connection is open on it when `in_use`, and it has
    /// been unused for `idle_ms`.
    pub fn observed(o: &ObservedServer, l: &Live, in_use: bool, idle_ms: u64, now_ms: u64) -> Self {
        let worktree = o.cwd.as_deref().map(worktree_root);
        let tool = tool_label(&o.command);
        // Process start times are Unix microseconds.
        let started_ms = o.key.start_time / 1000;
        ServerViewFfi {
            pid: o.key.pid,
            start_time: o.key.start_time,
            port: o.port,
            ports: o.ports.clone(),
            command: o.command.clone(),
            tool_label: tool.clone(),
            worktree: worktree
                .as_deref()
                .map(|w| w.display().to_string())
                .unwrap_or_default(),
            worktree_name: worktree.as_deref().map(file_name).unwrap_or_default(),
            location_label: worktree.as_deref().map(location_label).unwrap_or_default(),
            session_name: String::new(),
            agent: String::new(),
            agent_label: "no agent".to_string(),
            started_ms,
            started_label: started_label(started_ms, now_ms),
            session_alive: false,
            idle_ms,
            kill_in_ms: None,
            state: if idle_ms < ACTIVE_WINDOW_MS {
                "active"
            } else {
                "idle"
            }
            .to_string(),
            managed: false,
            excluded: o.excluded,
            framework: l.framework.clone(),
            title: title_of(l, &tool),
            has_icon: l.has_icon,
            branch: l.branch.clone(),
            rss_bytes: l.rss_bytes,
            cpu_pct: l.cpu_pct,
            growth_bytes: l.growth_bytes,
            health: health_id(l.health).to_string(),
            in_use,
            status_text: status_text_observed(o, in_use, idle_ms),
            card: l.card.clone(),
            leak_kind: l.leak_kind.clone(),
            can_restart: l.can_restart && !o.excluded,
            postponed: false,
            uptime_ms: now_ms.saturating_sub(started_ms),
            process_count: l.process_count,
        }
    }
}

impl From<&KillRecord> for KillRecordFfi {
    fn from(r: &KillRecord) -> Self {
        KillRecordFfi {
            port: r.port,
            command: r.command.clone(),
            tool_label: tool_label(&r.command),
            worktree: r.worktree.display().to_string(),
            worktree_name: file_name(&r.worktree),
            reason: reason_label(&r.reason),
            at_ms: r.at_ms,
        }
    }
}

/// What the watcher knows about server `key` beyond its row. `cwd` is the
/// folder its `package.json` is looked for in, `worktree` its git worktree.
fn live_of<P: Platform>(
    w: &mut Watcher<P>,
    key: ServerKey,
    command: &str,
    worktree: Option<&Path>,
    card: Option<&NeedsYou>,
    now_ms: u64,
) -> Live {
    let last = w.history().samples(key).last().copied();
    let (rss_bytes, cpu_pct) = last.map_or((0, 0.0), |s| (s.rss_bytes, s.cpu_pct));
    let growth_bytes = w.history().growth_bytes(key, now_ms);
    let procs = w.server_procs(key);
    let cwd = procs.first().and_then(|p| p.cwd.clone());
    let (framework, package_name, branch) = match cwd.as_deref() {
        Some(cwd) => w.server_meta(&key, command, cwd, worktree.unwrap_or(cwd), now_ms),
        None => (framework_of(command, None), None, None),
    };
    let page = w.page_meta_of(key);
    Live {
        rss_bytes,
        cpu_pct,
        growth_bytes,
        health: health(rss_bytes, growth_bytes, &w.config()),
        framework,
        package_name,
        branch,
        page_title: page.and_then(|m| m.title.clone()),
        has_icon: page.is_some_and(|m| m.icon.is_some()),
        can_restart: !procs.is_empty() && w.can_restart(key),
        process_count: procs.len() as u32,
        card: card.map(Into::into),
        leak_kind: leak_kind(card),
    }
}

/// `size` or `growth` for a leak card, `None` for any other card or none.
fn leak_kind(card: Option<&NeedsYou>) -> Option<String> {
    match card {
        Some(NeedsYou::Leak { cause, .. }) => Some(
            match cause {
                LeakCause::Size => "size",
                LeakCause::Growth => "growth",
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// An observed server is in use while a connection is open on it, and
/// unused since the last browser closed, else since it started.
fn observed_use<P: Platform>(w: &Watcher<P>, o: &ObservedServer, now_ms: u64) -> (bool, u64) {
    let h = w.history();
    if h.in_use(o.key) {
        return (true, 0);
    }
    let since = h
        .events(o.key)
        .iter()
        .rev()
        .find(|e| e.kind == EventKind::BrowserClosed)
        .map_or(o.key.start_time / 1000, |e| e.at_ms);
    (false, now_ms.saturating_sub(since))
}

/// `t` as the app shows it, with what the watcher knows of each server.
fn report_ffi<P: Platform>(w: &mut Watcher<P>, t: &TickReport, now_ms: u64) -> TickReportFfi {
    let cards: HashMap<ServerKey, &NeedsYou> = t.needs_you.iter().map(|(k, n)| (*k, n)).collect();
    let mut servers: Vec<ServerViewFfi> = Vec::new();
    for v in &t.servers {
        let card = cards.get(&v.key).copied();
        let l = live_of(w, v.key, &v.command, Some(&v.worktree), card, now_ms);
        servers.push(ServerViewFfi::tracked(v, &l, now_ms));
    }
    for o in &t.observed {
        let worktree = o.cwd.as_deref().map(worktree_root);
        let l = live_of(
            w,
            o.key,
            &o.command,
            worktree.as_deref(),
            cards.get(&o.key).copied(),
            now_ms,
        );
        let (in_use, idle_ms) = observed_use(w, o, now_ms);
        servers.push(ServerViewFfi::observed(o, &l, in_use, idle_ms, now_ms));
    }
    let dev = servers.iter().map(|s| s.rss_bytes).sum();
    let machine = w.platform().machine_memory();
    let (total, used) = machine.map_or((0, 0), |m| (m.total_bytes, m.used_bytes));
    // Rows of this pass (a server stopped by the pass has none).
    let rows = |keys: &mut dyn Iterator<Item = ServerKey>| -> Vec<ServerViewFfi> {
        keys.filter_map(|k| {
            servers
                .iter()
                .find(|s| s.pid == k.pid && s.start_time == k.start_time)
                .cloned()
        })
        .collect()
    };
    let alerts = rows(&mut t.alerts.iter().map(|v| v.key));
    let leaks = rows(&mut t.leaks.iter().copied());
    TickReportFfi {
        servers,
        alerts,
        killed: t.killed.iter().map(Into::into).collect(),
        config_error: t.config_error.clone(),
        memory: MemoryBarFfi::new(dev, total, used),
        leaks,
    }
}

impl From<Config> for ConfigFfi {
    fn from(c: Config) -> Self {
        ConfigFfi {
            dead_session_idle_s: c.dead_session_idle_s,
            alert_after_s: c.alert_after_s,
            kill_grace_s: c.kill_grace_s,
            scan_interval_s: c.scan_interval_s,
            claude_config_dirs: c
                .claude_config_dirs
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            exclude_commands: c.exclude_commands,
            exclude_ports: c.exclude_ports,
            leak_memory_mb: c.leak_memory_mb,
            leak_growth_mb: c.leak_growth_mb,
        }
    }
}

impl From<ConfigFfi> for Config {
    fn from(c: ConfigFfi) -> Self {
        Config {
            dead_session_idle_s: c.dead_session_idle_s,
            alert_after_s: c.alert_after_s,
            kill_grace_s: c.kill_grace_s,
            scan_interval_s: c.scan_interval_s,
            claude_config_dirs: c
                .claude_config_dirs
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            exclude_commands: c.exclude_commands,
            exclude_ports: c.exclude_ports,
            leak_memory_mb: c.leak_memory_mb,
            leak_growth_mb: c.leak_growth_mb,
        }
    }
}

/// A duration as the menu shows it (`<1 min`, `12 min`, `2 h`, `3 d`), the
/// same label `btp list` prints.
#[uniffi::export]
pub fn duration_label(ms: u64) -> String {
    format_idle(ms)
}

#[derive(uniffi::Object)]
pub struct Core {
    dir: PathBuf,
    /// Finds the Claude config dirs on its own (`default_config_dirs`).
    discover: fn() -> Vec<PathBuf>,
    watcher: Mutex<Watcher<MacOs>>,
    /// Held from a successful `acquire_watch` until the object is dropped.
    watch_lock: Mutex<Option<WatchLock>>,
    /// Fetches a port's title and favicon (`probe_port`; tests never touch
    /// the network).
    probe: fn(u16) -> ProbeOutcome,
}

/// A poisoned mutex only means an earlier call panicked; the watcher holds no
/// in-memory state worth distrusting (everything lives in `state.json`).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Core {
    fn at(dir: PathBuf, discover: fn() -> Vec<PathBuf>) -> Self {
        let watcher = Watcher::new(MacOs, dir.clone())
            .with_discovery(discover)
            .with_ignored_peers(vec![std::process::id() as i32]);
        // Tests run on the real platform: a live Codex of the machine must not
        // show up in them.
        #[cfg(test)]
        let watcher = watcher.without_codex();
        // Tests run on the real platform: the machine's own servers are
        // never fetched.
        let probe: fn(u16) -> ProbeOutcome = if cfg!(test) {
            |_| ProbeOutcome::Nothing
        } else {
            probe_port
        };
        Core {
            watcher: Mutex::new(watcher),
            dir,
            discover,
            watch_lock: Mutex::new(None),
            probe,
        }
    }

    /// One pass (`tick` or `observe`), then the title and favicon probes it
    /// made due, fetched with the watcher unlocked: no network I/O ever
    /// happens while it is held.
    fn pass(
        &self,
        run: fn(&mut Watcher<MacOs>, u64) -> io::Result<TickReport>,
    ) -> Result<TickReportFfi, BtpError> {
        let now = now_ms();
        let (report, due) = {
            let mut w = lock(&self.watcher);
            let report = run(&mut w, now)?;
            (report, w.due_probes(now))
        };
        let results: Vec<_> = due
            .into_iter()
            .map(|(key, port)| ((key, port), (self.probe)(port)))
            .collect();
        let mut w = lock(&self.watcher);
        if !results.is_empty() {
            w.store_probes(results, now_ms());
        }
        Ok(report_ffi(&mut w, &report, now))
    }

    fn kill(&self, target: KillTarget) -> Result<Option<KillRecordFfi>, BtpError> {
        Ok(lock(&self.watcher).kill(target)?.as_ref().map(Into::into))
    }

    /// The kept servers recorded for this ended session, marking it
    /// delivered (its buttons were pressed, so it was seen).
    fn take_group(
        &self,
        session_pid: i32,
        session_proc_start: &str,
        at_ms: u64,
    ) -> Result<Vec<ServerKey>, BtpError> {
        Ok(with_state(&self.dir, |s| {
            s.mark_session_ended_delivered(session_pid, session_proc_start, at_ms);
            s.session_ended_group(session_pid, session_proc_start, at_ms)
                .map(|g| g.still_open.iter().map(|o| o.key).collect())
                .unwrap_or_default()
        })?)
    }
}

impl Default for Core {
    fn default() -> Self {
        Core::new()
    }
}

#[uniffi::export]
impl Core {
    /// A core on the default data dir (`BTP_DATA_DIR` or
    /// `~/Library/Application Support/balance-ton-port`).
    #[uniffi::constructor]
    pub fn new() -> Self {
        Core::at(data_dir(), default_config_dirs)
    }

    /// One watch pass (link, observe, kill, alert). Blocking; never call it
    /// on the main thread.
    pub fn tick(&self) -> Result<TickReportFfi, BtpError> {
        self.pass(Watcher::tick)
    }

    /// A read-only watch pass for when another process holds the watch lock:
    /// links new servers and records port activity, but kills nothing and
    /// marks no alert. `killed` lists the kills the watcher would make;
    /// ignore it. Blocking; never call it on the main thread.
    pub fn observe(&self) -> Result<TickReportFfi, BtpError> {
        self.pass(Watcher::observe)
    }

    /// Kills the tracked server on `port`. `nil` when nothing tracked matches.
    pub fn kill_port(&self, port: u16) -> Result<Option<KillRecordFfi>, BtpError> {
        self.kill(KillTarget::Port(port))
    }

    /// Kills exactly this tracked server (pid and start time, as a row or a
    /// notification identifies it). `nil` when it is no longer tracked or
    /// running: a process that reused the pid is never touched.
    pub fn kill_server(
        &self,
        pid: i32,
        start_time: u64,
    ) -> Result<Option<KillRecordFfi>, BtpError> {
        self.kill(KillTarget::Key(ServerKey { pid, start_time }))
    }

    /// Records that this server's alert was delivered (call it once the
    /// notification is added): no more alerts until it restarts. `tick`
    /// reports a due alert on every pass until then.
    pub fn mark_alerted(&self, pid: i32, start_time: u64) -> Result<(), BtpError> {
        Ok(lock(&self.watcher).mark_alerted(ServerKey { pid, start_time })?)
    }

    /// No more alerts for this server until it restarts.
    pub fn keep(&self, pid: i32, start_time: u64) -> Result<(), BtpError> {
        Ok(lock(&self.watcher).keep(ServerKey { pid, start_time })?)
    }

    /// Memory and CPU of the server's tree, one sample per pass, oldest
    /// first (the last 2 hours, in memory only).
    pub fn history(&self, pid: i32, start_time: u64) -> Vec<SampleFfi> {
        lock(&self.watcher)
            .history()
            .samples(ServerKey { pid, start_time })
            .iter()
            .map(|s| SampleFfi {
                at_ms: s.at_ms,
                rss_bytes: s.rss_bytes,
                cpu_pct: s.cpu_pct,
            })
            .collect()
    }

    /// The server's events of the last 2 hours, oldest first.
    pub fn events(&self, pid: i32, start_time: u64) -> Vec<EventFfi> {
        lock(&self.watcher)
            .history()
            .events(ServerKey { pid, start_time })
            .iter()
            .map(Into::into)
            .collect()
    }

    /// The page's favicon (PNG, ICO, SVG... as served) when `has_icon`.
    pub fn icon(&self, pid: i32, start_time: u64) -> Option<Vec<u8>> {
        lock(&self.watcher)
            .page_meta_of(ServerKey { pid, start_time })
            .and_then(|m| m.icon.as_ref())
            .map(|i| i.bytes.clone())
    }

    /// The processes of the server's tree, the server first.
    pub fn processes(&self, pid: i32, start_time: u64) -> Vec<ProcRowFfi> {
        let w = lock(&self.watcher);
        w.server_procs(ServerKey { pid, start_time })
            .into_iter()
            .map(|p| ProcRowFfi {
                pid: p.pid,
                rss_bytes: w
                    .platform()
                    .proc_resources(p.pid)
                    .map_or(0, |r| r.rss_bytes),
                command: p.command,
            })
            .collect()
    }

    /// The command that resumes the agent session that started this server,
    /// for the session chip to copy (never run): `cd '<folder>' && claude
    /// --resume <id>` or `cd '<folder>' && codex resume <id>`. `nil` for an
    /// observed server or a session without an id.
    pub fn resume_command(&self, pid: i32, start_time: u64) -> Result<Option<String>, BtpError> {
        let key = ServerKey { pid, start_time };
        let state = read_state_readonly(&self.dir)?;
        let Some(s) = state.servers.iter().find(|s| s.key == key) else {
            return Ok(None);
        };
        let folder = s.session_cwd.as_deref().unwrap_or(&s.worktree);
        Ok(resume_command(s.agent, folder, &s.session_id))
    }

    /// "Postpone 1 h": the server's stop waits one hour from now. `false`
    /// when it is not tracked.
    pub fn postpone(&self, pid: i32, start_time: u64) -> Result<bool, BtpError> {
        Ok(lock(&self.watcher).postpone(ServerKey { pid, start_time })?)
    }

    /// "Ignore this alert": dismisses the leak or idle alert card the last
    /// pass showed for this server. `false` when its card is not an alert.
    pub fn ignore_alert(&self, pid: i32, start_time: u64) -> bool {
        lock(&self.watcher).ignore_alert(ServerKey { pid, start_time })
    }

    /// Records that this server's leak notification was added: no more
    /// until its alert is ignored and re-arms. `tick` reports a due leak in
    /// `leaks` on every pass until then.
    pub fn mark_leak_notified(&self, pid: i32, start_time: u64) {
        lock(&self.watcher).mark_leak_notified(ServerKey { pid, start_time });
    }

    /// Stops the server and starts it again with the same command, folder
    /// and environment; a tracked server keeps its session, note, Keep and
    /// Postpone. `nil` once started, else why not, as the row shows it (`The
    /// folder no longer exists`, `Cannot read its environment`, `The port is
    /// still taken`, `It matches an exclusion now`, `Already restarting`,
    /// `Could not start it again`). Blocks for the kill grace plus up to 5 s
    /// while the port frees: never call it on the main thread.
    pub fn restart(&self, pid: i32, start_time: u64) -> Result<Option<String>, BtpError> {
        // Checked and planned under the lock; the stop, the port wait and
        // the start run without it, so passes go on meanwhile.
        let (job, platform) = {
            let mut w = lock(&self.watcher);
            let job = w.prepare_restart(ServerKey { pid, start_time }, now_ms())?;
            (job, *w.platform())
        };
        let r = match job {
            Ok(job) => job.run(&platform)?,
            Err(blocked) => Err(blocked),
        };
        Ok(r.err().map(|b| restart_blocked_label(b).to_string()))
    }

    /// Stops an observed server (no agent started it), once the user
    /// confirmed. `nil`, sending nothing, unless it is still that exact
    /// observed, non-excluded server. Blocking.
    pub fn stop_observed(
        &self,
        pid: i32,
        start_time: u64,
    ) -> Result<Option<KillRecordFfi>, BtpError> {
        // Checked under the lock, killed without it (the kill grace).
        let (job, platform) = {
            let mut w = lock(&self.watcher);
            let job = w.prepare_stop_observed(ServerKey { pid, start_time })?;
            (job, *w.platform())
        };
        let Some(job) = job else {
            return Ok(None);
        };
        Ok(job.run(&platform)?.as_ref().map(Into::into))
    }

    /// The Claude config dirs found automatically, read only. The config's
    /// `claude_config_dirs` are extras watched on top of these.
    pub fn discovered_claude_dirs(&self) -> Vec<String> {
        (self.discover)()
            .iter()
            .map(|p| p.display().to_string())
            .collect()
    }

    /// The saved config; `claude_config_dirs` holds only the extras.
    pub fn config(&self) -> Result<ConfigFfi, BtpError> {
        read_config(&self.dir)
            .map(Into::into)
            .map_err(|message| BtpError::Io { message })
    }

    /// Saves the config. Refuses a value below its minimum (delays in
    /// minutes >= 60 s, grace and scan >= 1 s, leak memory >= 100 MB, leak
    /// growth >= 50 MB).
    pub fn set_config(&self, c: ConfigFfi) -> Result<(), BtpError> {
        let c: Config = c.into();
        c.validate().map_err(|message| BtpError::Io { message })?;
        Ok(save_config(&self.dir, &c)?)
    }

    /// Claims the ended sessions whose kept servers are not notified yet
    /// (oldest first, each listing only its servers still tracked): they are
    /// marked delivered under the state lock before this returns, so neither
    /// the hook nor a later pass posts them too. Call it only when the
    /// notification can be posted; hand back any whose `center.add` failed
    /// with `unclaim_session_ended`. The body uses the dead-session delay of
    /// the saved config.
    pub fn claim_session_ended(&self) -> Result<Vec<SessionEndedFfi>, BtpError> {
        let dead_idle_s = read_config(&self.dir)
            .unwrap_or_else(|_| lock(&self.watcher).config())
            .dead_session_idle_s;
        let groups = with_state(&self.dir, |s| s.claim_session_ended())?;
        Ok(groups
            .iter()
            .map(|g| SessionEndedFfi::at(g, dead_idle_s))
            .collect())
    }

    /// Hands back a claimed group whose notification could not be added:
    /// the next pass claims it again.
    pub fn unclaim_session_ended(
        &self,
        session_pid: i32,
        session_proc_start: String,
        at_ms: u64,
    ) -> Result<(), BtpError> {
        Ok(with_state(&self.dir, |s| {
            s.unmark_session_ended_delivered(session_pid, &session_proc_start, at_ms)
        })?)
    }

    /// "Stop now": stops every kept server of this ended session through
    /// `kill_server` (exact pid and start time; a server gone or restarted
    /// is skipped). Returns the kills made. Blocking.
    pub fn stop_session_ended(
        &self,
        session_pid: i32,
        session_proc_start: String,
        at_ms: u64,
    ) -> Result<Vec<KillRecordFfi>, BtpError> {
        let keys = self.take_group(session_pid, &session_proc_start, at_ms)?;
        let mut kills = Vec::new();
        for key in keys {
            if let Some(r) = self.kill_server(key.pid, key.start_time)? {
                kills.push(r);
            }
        }
        Ok(kills)
    }

    /// "Keep": Keep on every kept server of this ended session.
    pub fn keep_session_ended(
        &self,
        session_pid: i32,
        session_proc_start: String,
        at_ms: u64,
    ) -> Result<(), BtpError> {
        for key in self.take_group(session_pid, &session_proc_start, at_ms)? {
            self.keep(key.pid, key.start_time)?;
        }
        Ok(())
    }

    /// The last kills, newest first.
    pub fn recent_kills(&self) -> Result<Vec<KillRecordFfi>, BtpError> {
        let kills = with_state(&self.dir, |s| s.recent_kills.clone())?;
        Ok(kills.iter().map(Into::into).collect())
    }

    /// Takes the watch lock for this object's lifetime. `false` when another
    /// process (`btp watch`) already holds it; `true` again if this object
    /// already holds it.
    pub fn acquire_watch(&self) -> Result<bool, BtpError> {
        let mut held = lock(&self.watch_lock);
        if held.is_some() {
            return Ok(true);
        }
        match acquire_watch_lock(&self.dir, APP_OWNER) {
            Ok(l) => {
                *held = Some(l);
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_reason_strings() {
        assert_eq!(
            reason_label(&KillReason::WorktreeDeleted),
            "worktree deleted"
        );
        assert_eq!(
            reason_label(&KillReason::DeadSessionIdle {
                idle_ms: 23 * 60_000
            }),
            "dead session + idle 23 min"
        );
        assert_eq!(reason_label(&KillReason::Manual), "manual");
    }

    #[test]
    fn duration_label_matches_the_cli() {
        assert_eq!(duration_label(30_000), "<1 min");
        assert_eq!(duration_label(12 * 60_000), "12 min");
        assert_eq!(duration_label(2 * 3_600_000), "2 h");
    }

    /// Held by the tests that spawn a child and by those that check the
    /// watch lock. Without it `acquire_watch_is_exclusive_and_held_until_drop`
    /// fails now and then (a dropped lock still looks taken) while another
    /// test spawns a child; serializing them removes it. The cause is not
    /// established.
    static SPAWNS: Mutex<()> = Mutex::new(());

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn core_is_send_and_sync() {
        assert_send_sync::<Core>();
    }

    #[test]
    fn server_view_carries_agent() {
        let v = ServerView {
            key: ServerKey {
                pid: 42,
                start_time: 7,
            },
            port: 5173,
            ports: vec![5173],
            command: "vite".into(),
            tool_label: "Vite".into(),
            worktree: "/repo/app".into(),
            worktree_name: "app".into(),
            location_label: "app".into(),
            session_name: "app-1d".into(),
            agent: btp_core::agents::AgentKind::ClaudeCode,
            started_ms: 0,
            session_alive: true,
            idle_ms: 0,
            kill_in_ms: None,
            in_use: false,
            postponed_until_ms: None,
            note_reason: None,
            kept_by_user: false,
        };
        let f = ServerViewFfi::tracked(&v, &Live::default(), 0);
        assert_eq!(f.agent, "claude_code");
        assert_eq!(f.agent_label, "Claude");
    }

    #[test]
    fn server_view_carries_state() {
        let v = ServerView {
            key: ServerKey {
                pid: 42,
                start_time: 7,
            },
            port: 5173,
            ports: vec![5173],
            command: "vite".into(),
            tool_label: "Vite".into(),
            worktree: "/repo/app".into(),
            worktree_name: "app".into(),
            location_label: "app".into(),
            session_name: "app-1d".into(),
            agent: btp_core::agents::AgentKind::ClaudeCode,
            started_ms: 0,
            session_alive: false,
            idle_ms: 0,
            kill_in_ms: Some(1_000),
            in_use: false,
            postponed_until_ms: None,
            note_reason: None,
            kept_by_user: false,
        };
        let f = ServerViewFfi::tracked(&v, &Live::default(), 4 * 60_000);
        assert_eq!((f.pid, f.start_time, f.port), (42, 7, 5173));
        assert_eq!(f.kill_in_ms, Some(1_000));
        assert_eq!(f.state, "session dead");
        assert_eq!(f.tool_label, "Vite");
        assert_eq!(f.worktree, "/repo/app");
        assert_eq!(f.location_label, "app");
        assert_eq!(f.session_name, "app-1d");
        assert_eq!(f.started_label, "started 4 min ago");
    }

    fn view() -> ServerView {
        ServerView {
            key: ServerKey {
                pid: 42,
                start_time: 7_000,
            },
            port: 3010,
            ports: vec![3010, 9229],
            command: "vite".into(),
            tool_label: "Vite".into(),
            worktree: "/repo/app".into(),
            worktree_name: "app".into(),
            location_label: "app".into(),
            session_name: "app-1d".into(),
            agent: AgentKind::ClaudeCode,
            started_ms: 1_000,
            session_alive: false,
            idle_ms: 12 * 60_000,
            kill_in_ms: Some(8 * 60_000),
            in_use: false,
            postponed_until_ms: Some(10_000),
            note_reason: None,
            kept_by_user: false,
        }
    }

    #[test]
    fn card_maps_kind_reason_action() {
        use btp_core::history::MB;
        let leak = NeedsYou::Leak {
            rss_bytes: 900 * MB,
            growth_bytes: 600 * MB as i64,
            cause: LeakCause::Growth,
        };
        let card = |n: &NeedsYou| {
            let c = CardFfi::from(n);
            (c.kind, c.reason, c.action)
        };
        assert_eq!(
            card(&leak),
            (
                "leak".into(),
                "Growing fast: +600 MB in 10 min".into(),
                "restart".into()
            )
        );
        assert_eq!(
            card(&NeedsYou::AboutToStop { in_ms: 8 * 60_000 }),
            (
                "about_to_stop".into(),
                "Session ended, unused · stops in 8 min".into(),
                "postpone".into()
            )
        );
        assert_eq!(
            card(&NeedsYou::SessionEndedOpen),
            (
                "session_ended_open".into(),
                "Session ended, still open in a browser".into(),
                "stop".into()
            )
        );
        let idle = NeedsYou::IdleAlert {
            idle_ms: 40 * 60_000,
            agent: AgentKind::Codex,
        };
        assert_eq!(
            card(&idle),
            (
                "idle_alert".into(),
                "Codex session idle 40 min".into(),
                "stop".into()
            )
        );
    }

    #[test]
    fn leak_kind_follows_the_card_cause() {
        use btp_core::history::MB;
        let leak = |cause| NeedsYou::Leak {
            rss_bytes: 3000 * MB,
            growth_bytes: 600 * MB as i64,
            cause,
        };
        assert_eq!(
            leak_kind(Some(&leak(LeakCause::Size))).as_deref(),
            Some("size")
        );
        assert_eq!(
            leak_kind(Some(&leak(LeakCause::Growth))).as_deref(),
            Some("growth")
        );
        assert_eq!(leak_kind(Some(&NeedsYou::SessionEndedOpen)), None);
        assert_eq!(leak_kind(None), None);
    }

    #[test]
    fn tracked_view_carries_live_facts() {
        let l = Live {
            rss_bytes: 600 * 1_048_576,
            cpu_pct: 12.5,
            growth_bytes: -3,
            health: Health::Amber,
            framework: Some("Vite".into()),
            package_name: Some("web".into()),
            branch: Some("main".into()),
            page_title: None,
            has_icon: true,
            can_restart: true,
            process_count: 3,
            card: Some(CardFfi::from(&NeedsYou::AboutToStop { in_ms: 1 })),
            leak_kind: None,
        };
        let f = ServerViewFfi::tracked(&view(), &l, 5_000);
        assert!(f.managed && !f.excluded);
        assert_eq!(f.ports, vec![3010, 9229]);
        assert_eq!(f.title, "web", "the package name without a page title");
        assert_eq!(f.health, "amber");
        assert_eq!(f.status_text, "stops in 8 min");
        assert_eq!(f.card.unwrap().action, "postpone");
        assert!(f.postponed, "postponed until 10 s, now 5 s");
        assert_eq!((f.uptime_ms, f.process_count), (4_000, 3));
        assert!(f.has_icon && f.can_restart);
        let titled = Live {
            page_title: Some("Roadbook".into()),
            ..l.clone()
        };
        assert_eq!(
            ServerViewFfi::tracked(&view(), &titled, 0).title,
            "Roadbook"
        );
        let bare = ServerViewFfi::tracked(&view(), &Live::default(), 20_000);
        assert_eq!(bare.title, "Vite", "the tool label last");
        assert!(!bare.postponed);
    }

    #[test]
    fn observed_maps_to_no_agent() {
        let o = ObservedServer {
            key: ServerKey {
                pid: 77,
                start_time: 2_000_000,
            },
            port: 5432,
            ports: vec![5432],
            command: "postgres -D /usr/local/var/postgres".into(),
            cwd: Some("/tmp/no-repo-here".into()),
            excluded: false,
        };
        let l = Live {
            can_restart: true,
            ..Live::default()
        };
        let f = ServerViewFfi::observed(&o, &l, false, 5 * 60_000, 62_000);
        assert!(!f.managed);
        assert_eq!((f.agent.as_str(), f.agent_label.as_str()), ("", "no agent"));
        assert_eq!(f.session_name, "");
        assert!(!f.session_alive);
        assert_eq!(f.kill_in_ms, None);
        assert_eq!(f.status_text, "idle 5 min");
        assert_eq!(f.state, "idle");
        assert_eq!(f.started_ms, 2_000);
        assert_eq!(f.uptime_ms, 60_000);
        assert!(f.can_restart);
        let in_use = ServerViewFfi::observed(&o, &l, true, 0, 62_000);
        assert_eq!(
            (in_use.status_text.as_str(), in_use.in_use),
            ("in use", true)
        );
        let excluded = ObservedServer {
            excluded: true,
            ..o
        };
        let f = ServerViewFfi::observed(&excluded, &l, false, 0, 62_000);
        assert!(f.excluded);
        assert_eq!(f.status_text, "excluded");
        assert!(!f.can_restart, "an excluded server never restarts");
    }

    #[test]
    fn resume_command_quotes_the_folder() {
        assert_eq!(
            resume_command(AgentKind::ClaudeCode, Path::new("/tmp/it's"), "abc").as_deref(),
            Some(r"cd '/tmp/it'\''s' && claude --resume abc")
        );
        assert_eq!(
            resume_command(AgentKind::ClaudeCode, Path::new("/tmp"), ""),
            None
        );
        assert_eq!(
            resume_command(AgentKind::ClaudeCode, Path::new("/tmp"), "a; rm -rf ~"),
            None,
            "an id a shell would read is never offered"
        );
    }

    #[test]
    fn codex_resume_command() {
        assert_eq!(
            resume_command(AgentKind::Codex, Path::new("/repo/wt"), "0199-ab_c").as_deref(),
            Some("cd '/repo/wt' && codex resume 0199-ab_c")
        );
    }

    #[test]
    fn restart_blocked_copy() {
        use RestartBlocked::*;
        let copy = |b| restart_blocked_label(b);
        assert_eq!(copy(CwdGone), "The folder no longer exists");
        assert_eq!(copy(EnvUnreadable), "Cannot read its environment");
        assert_eq!(copy(ArgvUnreadable), "Cannot read its environment");
        assert_eq!(copy(PortStillTaken), "The port is still taken");
        assert_eq!(copy(Excluded), "It matches an exclusion now");
        assert_eq!(copy(Busy), "Already restarting");
        assert_eq!(copy(Gone), "Could not start it again");
        assert_eq!(copy(SpawnFailed), "Could not start it again");
    }

    #[test]
    fn event_labels() {
        use btp_core::history::AlertKind;
        let label = |kind| EventFfi::from(&ServerEvent { at_ms: 3, kind });
        let e = label(EventKind::Started {
            agent: Some(AgentKind::ClaudeCode),
        });
        assert_eq!(
            (e.at_ms, e.kind.as_str(), e.label.as_str()),
            (3, "started", "started by Claude")
        );
        let all = [
            (
                EventKind::Started {
                    agent: Some(AgentKind::Codex),
                },
                "started by Codex",
            ),
            (EventKind::Started { agent: None }, "started"),
            (EventKind::BrowserConnected, "browser connected"),
            (EventKind::BrowserClosed, "browser closed"),
            (EventKind::SessionEnded, "session ended"),
            (
                EventKind::Alert {
                    alert: AlertKind::Leak,
                },
                "alert",
            ),
            (EventKind::Restarted, "restarted"),
            (EventKind::Postponed, "postponed 1 h"),
        ];
        for (kind, text) in all {
            assert_eq!(label(kind).label, text);
        }
        assert_eq!(label(EventKind::BrowserConnected).kind, "browser_connected");
        assert_eq!(label(EventKind::Postponed).kind, "postponed");
    }

    #[test]
    fn memory_bar_line() {
        const GB: u64 = 1024 * 1_048_576;
        let total = 36 * GB;
        let used = total - (44 * GB) / 10;
        let dev = (48 * GB) / 10;
        let m = MemoryBarFfi::new(dev, total, used);
        assert_eq!(
            m.line,
            "4.8 GB dev servers (13%) · apps 26.8 · free 4.4 of 36 GB"
        );
        assert_eq!((m.dev_bytes, m.total_bytes), (dev, total));
        assert_eq!(m.apps_bytes, used - dev);
        assert_eq!(m.free_bytes, total - used);
        assert!(!m.free_low);
        assert!(MemoryBarFfi::new(0, 10 * GB, 10 * GB - GB / 2).free_low);
        // Dev servers above what the machine reports used never underflow.
        assert_eq!(MemoryBarFfi::new(2 * GB, 10 * GB, GB).apps_bytes, 0);
        assert!(!MemoryBarFfi::new(0, 0, 0).free_low);
    }

    #[test]
    fn core_ignores_its_own_connections() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        assert_eq!(
            lock(&core.watcher).ignored_peers(),
            &[std::process::id() as i32]
        );
    }

    #[test]
    fn detail_calls_on_an_unknown_server_are_empty() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let me = std::process::id() as i32;
        assert!(core.history(me, 1).is_empty());
        assert!(core.events(me, 1).is_empty());
        assert_eq!(core.icon(me, 1), None);
        assert!(core.processes(me, 1).is_empty(), "wrong start time");
        assert_eq!(core.resume_command(me, 1).unwrap(), None);
        assert!(!core.postpone(me, 1).unwrap());
        assert!(!core.ignore_alert(me, 1));
        core.mark_leak_notified(me, 1);
        assert_eq!(core.stop_observed(me, 1).unwrap(), None);
        assert_eq!(
            core.restart(me, 1).unwrap().as_deref(),
            Some("Could not start it again")
        );
    }

    #[test]
    fn resume_command_reads_the_tracked_session() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        seed_group(d.path());
        with_state(d.path(), |s| {
            s.servers[0].session_cwd = Some("/tmp/it's".into());
            s.servers[0].session_id = "abc".into();
        })
        .unwrap();
        assert_eq!(
            core.resume_command(999_999, 1).unwrap().as_deref(),
            Some(r"cd '/tmp/it'\''s' && claude --resume abc")
        );
        // A row tracked before the session folder was recorded: its worktree.
        with_state(d.path(), |s| s.servers[0].session_cwd = None).unwrap();
        assert_eq!(
            core.resume_command(999_999, 1).unwrap().as_deref(),
            Some("cd '/tmp/w' && claude --resume abc")
        );
    }

    #[test]
    fn processes_lists_a_running_server_first() {
        let _spawns = lock(&SPAWNS);
        use btp_core::platform::Platform;
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let start = MacOs.proc_info(pid).unwrap().start_time;
        let rows = core.processes(pid, start);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, pid);
        assert!(rows[0].command.contains("sleep"));
        assert!(rows[0].rss_bytes > 0);
    }

    #[test]
    fn report_carries_a_memory_bar() {
        let d = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = core.config().unwrap();
        c.claude_config_dirs = vec![sessions.path().display().to_string()];
        core.set_config(c).unwrap();
        let r = core.observe().unwrap();
        assert!(r.memory.total_bytes > 0);
        assert!(r.memory.line.contains("dev servers"), "{}", r.memory.line);
        let dev: u64 = r.servers.iter().map(|s| s.rss_bytes).sum();
        assert_eq!(r.memory.dev_bytes, dev);
        // Nothing tracked in a temp dir: every row is an observed one.
        assert!(r
            .servers
            .iter()
            .all(|s| !s.managed && s.agent_label == "no agent"));
    }

    /// A child `nc` listening on a free loopback port (an observed server
    /// of the test's own), once the platform sees it listen.
    fn nc_listener() -> (std::process::Child, u16, ServerKey) {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // An absolute argv[0]: `nc`'s environment reads back empty here (no
        // `PATH` to look it up in), and a restart must still plan it.
        let child = std::process::Command::new("/usr/bin/nc")
            .args(["-l", "127.0.0.1", &port.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !MacOs
            .listeners()
            .iter()
            .any(|l| l.pid == pid && l.port == port)
        {
            assert!(std::time::Instant::now() < deadline, "nc never listened");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let start_time = MacOs.proc_info(pid).unwrap().start_time;
        (child, port, ServerKey { pid, start_time })
    }

    /// A temp core whose sessions dir is empty: nothing is ever tracked.
    fn isolated_core() -> (Core, tempfile::TempDir, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = core.config().unwrap();
        c.claude_config_dirs = vec![sessions.path().display().to_string()];
        c.kill_grace_s = 2;
        core.set_config(c).unwrap();
        (core, d, sessions)
    }

    /// The ports `recording_probe` was asked, in order.
    static PROBED: Mutex<Vec<u16>> = Mutex::new(Vec::new());

    fn recording_probe(port: u16) -> ProbeOutcome {
        lock(&PROBED).push(port);
        ProbeOutcome::Meta(btp_core::probe::PageMeta {
            title: Some("Fetched title".into()),
            icon: None,
        })
    }

    #[test]
    fn a_pass_shows_the_title_its_probe_fetched() {
        let _spawns = lock(&SPAWNS);
        let (mut core, _d, _s) = isolated_core();
        core.probe = recording_probe;
        let (mut child, port, key) = nc_listener();
        let r = core.observe().unwrap();
        let probed = std::mem::take(&mut *lock(&PROBED));
        let _ = child.kill();
        let _ = child.wait();
        let row = r
            .servers
            .iter()
            .find(|s| s.pid == key.pid)
            .expect("observed");
        assert!(row.ports.contains(&port) && !row.managed);
        assert!(row.process_count >= 1);
        // Whatever the machine listens on, a pass probes something (`nc`
        // at least is due), after the pass with the watcher unlocked, and
        // that very report shows each answer.
        assert!(!probed.is_empty() && probed.len() <= 2, "{probed:?}");
        for port in probed {
            let shown = r.servers.iter().find(|s| s.port == port).expect("a row");
            assert_eq!(shown.title, "Fetched title");
            assert!(!shown.has_icon);
        }
    }

    /// Starts nothing: tells the test it was reached, then waits for its
    /// go before failing, or before handing back `started` (a process the
    /// test runs) when set.
    struct BlockingSpawner {
        entered: Mutex<std::sync::mpsc::Sender<()>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        started: Option<(i32, u64)>,
    }

    impl btp_core::restart::Spawner for BlockingSpawner {
        fn spawn(
            &self,
            _plan: &btp_core::restart::RestartPlan,
            _log: &Path,
            _p: &dyn Platform,
        ) -> io::Result<(i32, u64)> {
            let _ = lock(&self.entered).send(());
            let _ = lock(&self.release).recv();
            self.started.ok_or_else(|| io::Error::other("test spawner"))
        }
    }

    #[test]
    fn passes_run_while_a_restart_waits() {
        let _spawns = lock(&SPAWNS);
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let (core, d, _s) = isolated_core();
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        {
            let mut w = lock(&core.watcher);
            let old = std::mem::replace(&mut *w, Watcher::new(MacOs, d.path().to_path_buf()));
            *w = old
                .with_spawner(Box::new(BlockingSpawner {
                    entered: Mutex::new(entered_tx),
                    release: Mutex::new(release_rx),
                    started: None,
                }))
                .with_restart_wait(Duration::from_secs(5), Duration::from_millis(20));
        }
        let (mut child, _port, key) = nc_listener();
        let (blocked, observed) = std::thread::scope(|s| {
            let restart = s.spawn(|| core.restart(key.pid, key.start_time));
            // The restart stopped `nc` and reached the start: it is waiting.
            let reached = entered_rx.recv_timeout(Duration::from_secs(15)).is_ok();
            let (done_tx, done_rx) = channel();
            let core = &core;
            s.spawn(move || {
                let _ = done_tx.send(core.observe().is_ok());
            });
            let observed = reached && done_rx.recv_timeout(Duration::from_secs(10)) == Ok(true);
            let _ = release_tx.send(());
            (restart.join().unwrap(), observed)
        });
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            blocked.unwrap().as_deref(),
            Some("Could not start it again")
        );
        assert!(observed, "observe() returned while the restart was waiting");
    }

    #[test]
    fn two_restarts_of_one_server_spawn_once() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let _spawns = lock(&SPAWNS);
        let (core, d, _s) = isolated_core();
        // What the fake start hands back: a process of the test's own.
        let mut new = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let new_pid = new.id() as i32;
        let new_start = MacOs.proc_info(new_pid).unwrap().start_time;
        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        {
            let mut w = lock(&core.watcher);
            let old = std::mem::replace(&mut *w, Watcher::new(MacOs, d.path().to_path_buf()));
            *w = old
                .with_spawner(Box::new(BlockingSpawner {
                    entered: Mutex::new(entered_tx),
                    release: Mutex::new(release_rx),
                    started: Some((new_pid, new_start)),
                }))
                .with_restart_wait(Duration::from_secs(5), Duration::from_millis(20));
        }
        let (mut child, _port, key) = nc_listener();
        let (first, second, spawns) = std::thread::scope(|s| {
            let first = s.spawn(|| core.restart(key.pid, key.start_time));
            let reached = entered_rx.recv_timeout(Duration::from_secs(15)).is_ok();
            // A second click while the first restart waits at the start.
            let second = s.spawn(|| core.restart(key.pid, key.start_time));
            let second = second.join().unwrap();
            let _ = release_tx.send(());
            let first = first.join().unwrap();
            // A start the second restart would have made is reached by now.
            let more = entered_rx.recv_timeout(Duration::from_millis(200)).is_ok();
            (first, second, usize::from(reached) + usize::from(more))
        });
        let pending = with_state(d.path(), |s| s.pending_restarts.clone()).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        let _ = new.kill();
        let _ = new.wait();
        assert_eq!(first.unwrap(), None, "started");
        assert_eq!(second.unwrap().as_deref(), Some("Already restarting"));
        assert_eq!(spawns, 1);
        assert_eq!(pending.len(), 1);
        assert_eq!((pending[0].child_pid, pending[0].from), (new_pid, key));
        // Released: a later restart is judged on its own (`nc` is gone).
        assert_eq!(
            core.restart(key.pid, key.start_time).unwrap().as_deref(),
            Some("Could not start it again")
        );
    }

    #[test]
    fn kill_record_maps_worktree_and_reason() {
        let r = KillRecord {
            port: 3000,
            command: "next dev".into(),
            worktree: "/repo/.worktrees/feat-x".into(),
            reason: KillReason::WorktreeDeleted,
            at_ms: 9,
        };
        let f = KillRecordFfi::from(&r);
        assert_eq!(f.worktree, "/repo/.worktrees/feat-x");
        assert_eq!(f.worktree_name, "feat-x");
        assert_eq!(f.tool_label, "Next.js");
        assert_eq!(f.reason, "worktree deleted");
        assert_eq!(f.at_ms, 9);
    }

    #[test]
    fn config_roundtrips_through_disk() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = core.config().unwrap();
        assert_eq!(c.dead_session_idle_s, 1200);
        assert_eq!((c.leak_memory_mb, c.leak_growth_mb), (2048, 500));
        c.alert_after_s = 60;
        c.claude_config_dirs = vec!["/tmp/claude-x".into()];
        c.leak_memory_mb = 4096;
        c.leak_growth_mb = 300;
        core.set_config(c.clone()).unwrap();
        assert_eq!(core.config().unwrap(), c);
        let saved = read_config(d.path()).unwrap();
        assert_eq!(saved.alert_after_s, 60);
        // The app's save keeps the leak thresholds it was given.
        assert_eq!((saved.leak_memory_mb, saved.leak_growth_mb), (4096, 300));
    }

    #[test]
    fn saving_never_writes_discovered_dirs() {
        fn found() -> Vec<PathBuf> {
            vec![PathBuf::from("/tmp/found-claude")]
        }
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), found);
        assert_eq!(core.discovered_claude_dirs(), vec!["/tmp/found-claude"]);
        let mut c = core.config().unwrap();
        assert!(c.claude_config_dirs.is_empty());
        c.alert_after_s = 60;
        core.set_config(c).unwrap();
        let text = std::fs::read_to_string(d.path().join("config.toml")).unwrap();
        assert!(!text.contains("found-claude"), "{text}");
        assert!(text.contains("claude_config_dirs = []"), "{text}");
    }

    #[test]
    fn set_config_rejects_values_below_minimum() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = core.config().unwrap();
        c.kill_grace_s = 0;
        assert!(core.set_config(c).is_err());
        assert!(!d.path().join("config.toml").exists());
    }

    #[test]
    fn broken_config_is_an_error_and_reaches_the_report() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "alert_after_s = [").unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(core.config().is_err());
        let report = core.observe().unwrap();
        assert!(report.config_error.unwrap().contains("using defaults"));
    }

    #[test]
    fn kill_server_and_mark_alerted_ignore_an_unknown_server() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let me = std::process::id() as i32;
        assert_eq!(core.kill_server(me, 1).unwrap(), None);
        core.mark_alerted(me, 1).unwrap();
        // Nothing tracked: only the machine's observed servers are listed.
        assert!(core.observe().unwrap().servers.iter().all(|s| !s.managed));
    }

    /// A state with one tracked server (pid not running) and one undelivered
    /// group naming it and a server no longer tracked.
    fn seed_group(dir: &Path) -> (i32, String, u64) {
        use btp_core::model::TrackedServer;
        use btp_core::state::GroupServer;
        let dead_pid = 999_999;
        let start = "Thu Oct 22 10:00:00 2026".to_string();
        with_state(dir, |s| {
            s.servers.push(TrackedServer {
                key: ServerKey {
                    pid: dead_pid,
                    start_time: 1,
                },
                port: 5173,
                command: "node vite".into(),
                worktree: "/tmp/w".into(),
                session_id: "c".into(),
                session_pid: 50,
                session_proc_start: start.clone(),
                session_name: "web".into(),
                first_seen_ms: 0,
                last_active_ms: 0,
                kept: false,
                alerted: false,
                note: None,
                asked_stale: false,
                agent: btp_core::agents::AgentKind::ClaudeCode,
                postponed_until_ms: None,
                restarted_from: None,
                session_cwd: None,
            });
            let open = |pid, port| GroupServer {
                key: ServerKey { pid, start_time: 1 },
                port,
                tool_label: "Vite".into(),
                in_use: false,
            };
            s.record_session_ended(SessionEndedGroup {
                session_pid: 50,
                session_proc_start: start.clone(),
                conversation_id: String::new(),
                session_name: "web".into(),
                at_ms: 7,
                stopped: Vec::new(),
                still_open: vec![open(dead_pid, 5173), open(dead_pid + 1, 3000)],
                delivered: false,
                agent: btp_core::agents::AgentKind::ClaudeCode,
            });
        })
        .unwrap();
        (50, start, 7)
    }

    #[test]
    fn claim_session_ended_lists_tracked_servers_with_the_hook_text() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(core.claim_session_ended().unwrap().is_empty());
        let (pid, start, at) = seed_group(d.path());
        let mut c = core.config().unwrap();
        c.dead_session_idle_s = 1800;
        core.set_config(c).unwrap();
        let pending = core.claim_session_ended().unwrap();
        assert_eq!(pending.len(), 1);
        let g = &pending[0];
        assert_eq!(
            (g.session_pid, g.session_proc_start.as_str(), g.at_ms),
            (pid, start.as_str(), at)
        );
        assert_eq!(g.session_name, "web");
        assert_eq!(g.servers.len(), 1);
        assert_eq!((g.servers[0].pid, g.servers[0].port), (999_999, 5173));
        assert_eq!(
            g.body,
            "Session ended: 1 server kept running: Vite :5173. Stopped after 30 min unused, once no session works in that folder."
        );
        // Claimed: not handed out twice, until given back.
        assert!(core.claim_session_ended().unwrap().is_empty());
        core.unclaim_session_ended(pid, start, at).unwrap();
        assert_eq!(core.claim_session_ended().unwrap().len(), 1);
    }

    #[test]
    fn keep_session_ended_keeps_each_server_and_marks_delivered() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let (pid, start, at) = seed_group(d.path());
        core.keep_session_ended(pid, start, at).unwrap();
        let (kept, delivered) = with_state(d.path(), |s| {
            (s.servers[0].kept, s.session_ended[0].delivered)
        })
        .unwrap();
        assert!(kept && delivered);
    }

    #[test]
    fn stop_session_ended_never_touches_a_server_not_running() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let (pid, start, at) = seed_group(d.path());
        assert!(core
            .stop_session_ended(pid, start.clone(), at)
            .unwrap()
            .is_empty());
        assert!(core.claim_session_ended().unwrap().is_empty());
        // An unknown group stops nothing.
        assert!(core
            .stop_session_ended(pid, start, at + 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stop_session_ended_kills_only_the_exact_process() {
        let _spawns = lock(&SPAWNS);
        use btp_core::model::TrackedServer;
        use btp_core::platform::Platform;
        use btp_core::state::GroupServer;
        let d = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = core.config().unwrap();
        c.claude_config_dirs = vec![sessions.path().display().to_string()];
        c.kill_grace_s = 2;
        core.set_config(c).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let start_time = MacOs.proc_info(pid).unwrap().start_time;
        let me = std::process::id() as i32;
        let session = "Thu Oct 22 10:00:00 2026".to_string();
        with_state(d.path(), |s| {
            s.servers.push(TrackedServer {
                key: ServerKey { pid, start_time },
                port: 5173,
                command: "sleep 30".into(),
                worktree: "/tmp/w".into(),
                session_id: "c".into(),
                // The test process: no ancestor of the child between them.
                session_pid: me,
                session_proc_start: session.clone(),
                session_name: "web".into(),
                first_seen_ms: 0,
                last_active_ms: 0,
                kept: false,
                alerted: false,
                note: None,
                asked_stale: false,
                agent: btp_core::agents::AgentKind::ClaudeCode,
                postponed_until_ms: None,
                restarted_from: None,
                session_cwd: None,
            });
            for (at_ms, start) in [(7, start_time + 1), (8, start_time)] {
                s.record_session_ended(SessionEndedGroup {
                    session_pid: me,
                    session_proc_start: session.clone(),
                    conversation_id: String::new(),
                    session_name: "web".into(),
                    at_ms,
                    stopped: Vec::new(),
                    still_open: vec![GroupServer {
                        key: ServerKey {
                            pid,
                            start_time: start,
                        },
                        port: 5173,
                        tool_label: "sleep".into(),
                        in_use: false,
                    }],
                    delivered: false,
                    agent: btp_core::agents::AgentKind::ClaudeCode,
                });
            }
        })
        .unwrap();
        // A wrong start time: another process for all we know, spared.
        let none = core.stop_session_ended(me, session.clone(), 7).unwrap();
        assert!(none.is_empty());
        assert!(child.try_wait().unwrap().is_none(), "spared");
        // The exact process: stopped and reported.
        let kills = core.stop_session_ended(me, session, 8).unwrap();
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0].port, 5173);
        assert_eq!(kills[0].reason, "manual");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut gone = false;
        while std::time::Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !gone {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(gone, "the sleep child was stopped");
    }

    #[test]
    fn recent_kills_empty_on_fresh_dir() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(core.recent_kills().unwrap().is_empty());
    }

    #[test]
    fn observe_works_without_the_watch_lock() {
        let _spawns = lock(&SPAWNS);
        let d = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        let holder = Core::at(d.path().to_path_buf(), Vec::new);
        let mut c = holder.config().unwrap();
        c.claude_config_dirs = vec![sessions.path().display().to_string()];
        holder.set_config(c).unwrap();
        assert!(holder.acquire_watch().unwrap());
        let reader = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(!reader.acquire_watch().unwrap());
        let report = reader.observe().unwrap();
        assert!(report.servers.iter().all(|s| !s.managed));
        assert!(report.alerts.is_empty());
        assert!(report.killed.is_empty());
    }

    #[test]
    fn acquire_watch_is_exclusive_and_held_until_drop() {
        let _spawns = lock(&SPAWNS);
        let d = tempfile::tempdir().unwrap();
        let a = Core::at(d.path().to_path_buf(), Vec::new);
        let b = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(a.acquire_watch().unwrap());
        assert!(a.acquire_watch().unwrap(), "idempotent for the holder");
        assert!(!b.acquire_watch().unwrap());
        assert_eq!(
            std::fs::read_to_string(d.path().join("watch.lock")).unwrap(),
            APP_OWNER
        );
        drop(a);
        assert!(b.acquire_watch().unwrap());
    }
}
