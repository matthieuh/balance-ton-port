//! The watcher ties everything together: it links new listeners to the
//! agent session (Claude Code or Codex) that started them, observes tracked servers, asks
//! `decide` what to do, and executes it (kill, alert, forget).
//!
//! Safety: only a server linked at first sight to a live agent session
//! is ever tracked, and only tracked servers are ever signalled, each signal
//! guarded by a fresh start-time check (a pid can be reused at any moment).

use crate::agents::claude::ClaudeAgent;
use crate::agents::codex::{classify_codex, env_thread, CodexAgent, CodexRole};
use crate::agents::{is_agent_cli, session_idle_ms, Agent, AgentKind, AgentSession};
use crate::config::{effective_claude_dirs_with, read_config, Config};
use crate::decide::{decide, touch};
use crate::history::{AlertKind, EventKind, History, ProcId, ServerEvent};
use crate::labels::{location_label, session_display_name, tool_label};
use crate::model::{Action, KillReason, KillTarget, Note, Observation, ServerKey, TrackedServer};
use crate::observed::{observed_servers_cached, ObservedServer, SystemCache};
use crate::platform::{ancestry, format_proc_start, Listener, Platform, ProcInfo, ProcResources};
use crate::probe::{PageMeta, ProbeCache, ProbeOutcome};
use crate::sessions::default_config_dirs;
use crate::state::{append_kill_log, with_state, KillRecord, State};
use crate::worktree::{worktree_root, MetaCache};
use fs4::{FileExt, TryLockError};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod session;
pub use session::{
    NoteCmd, NoteOutcome, OwnKillOutcome, SessionEndReport, StaleServer, SweepOutcome, WaiterEnd,
};

/// One tracked server as the UI and the CLI show it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ServerView {
    pub key: ServerKey,
    pub port: u16,
    /// Every port the server's process tree listens on now, sorted.
    pub ports: Vec<u16>,
    pub command: String,
    /// A readable name for the server (`Expo (Metro)`, `Vite`, `Node x.js`).
    pub tool_label: String,
    /// Full worktree path.
    pub worktree: PathBuf,
    pub worktree_name: String,
    /// `<repo> › <worktree>`, the repo dir name, or a `~`-shortened path.
    pub location_label: String,
    /// The session's display name.
    pub session_name: String,
    /// The agent whose session started the server.
    pub agent: AgentKind,
    /// When the server process started, Unix milliseconds.
    pub started_ms: u64,
    pub session_alive: bool,
    /// Port idle time, or the larger of port and session idle time while the
    /// session is alive (what the alert compares against).
    pub idle_ms: u64,
    /// Time left before the dead-session kill, `Some(0)` when the worktree is
    /// gone; `None` while nothing is scheduled.
    pub kill_in_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TickReport {
    /// Servers still tracked after this pass.
    pub servers: Vec<ServerView>,
    /// Servers due an alert (`tick` and `observe`), reported on every pass
    /// until `mark_alerted` records the delivery. Always empty for `sweep`.
    pub alerts: Vec<ServerView>,
    /// Kills done (`tick`, `sweep`) or planned (`observe`).
    pub killed: Vec<KillRecord>,
    /// Listeners no agent started (`tick` and `observe`; always empty for
    /// `sweep`). Never stopped by a pass.
    pub observed: Vec<ObservedServer>,
    /// Set while `config.toml` cannot be read or parsed (the pass then ran
    /// on the last valid config, or the defaults if there never was one).
    pub config_error: Option<String>,
}

/// Two notes of rows folded into one server: the cautious side wins (keep
/// if either keeps, done only if both are done).
fn merge_notes(a: Option<Note>, b: Option<Note>) -> Option<Note> {
    match (a, b) {
        (Some(a), Some(b)) => Some(Note {
            keep: a.keep || b.keep,
            done: a.done && b.done,
            reason: if b.keep && !a.keep {
                b.reason
            } else {
                a.reason.or(b.reason)
            },
            set_at_ms: a.set_at_ms.max(b.set_at_ms),
        }),
        (a, b) => a.or(b),
    }
}

pub(crate) fn is_excluded(command: &str, cfg: &Config) -> bool {
    let command = command.to_lowercase();
    cfg.exclude_commands
        .iter()
        .map(|e| e.trim().to_lowercase())
        .any(|e| !e.is_empty() && command.contains(&e))
}

/// Links a new listener to the live agent session it belongs to.
///
/// `chain` is the listener's ancestry (`chain[0]` is the listener's own
/// process). The session is the nearest ancestor that matches a live session
/// by pid and by start time, so a stale session file whose pid now belongs
/// to another process never links. A Codex session picks the conversation
/// named by `CODEX_THREAD_ID` (read through `envs`, from the listener or the
/// nearest ancestor below the session) when that session holds it.
///
/// `None` when no ancestor is a live session (a server reparented to
/// launchd is never linked, whatever its environment), when the listener is
/// a session or an agent CLI itself, when a process between it and the
/// session is an agent CLI (a Codex `exec` or launcher excepted) or matches
/// an excluded command, when the port is excluded, or when the server's
/// working directory cannot be read. The environment is read only once a
/// Codex session matched.
pub fn link(
    listener: &Listener,
    chain: &[ProcInfo],
    envs: &dyn Fn(i32) -> Option<Vec<(String, String)>>,
    sessions: &[AgentSession],
    cfg: &Config,
    now_ms: u64,
) -> Option<TrackedServer> {
    let server = chain.first().filter(|p| p.pid == listener.pid)?;
    if sessions.iter().any(|s| s.pid == listener.pid)
        || cfg.exclude_ports.contains(&listener.port)
        || is_agent_cli(&server.command).is_some()
    {
        return None;
    }
    // Processes that stop a link: another agent CLI (a nested `codex exec`
    // and a launcher stay with the outer session) or an excluded command.
    let blocks = |p: &ProcInfo| {
        let agent = match (is_agent_cli(&p.command), classify_codex(&p.command)) {
            (Some(_), Some(CodexRole::NonInteractive | CodexRole::Wrapper)) => false,
            (agent, _) => agent.is_some(),
        };
        agent || is_excluded(&p.command, cfg)
    };
    // The thread id of the nearest process of `chain[..end]` that has one.
    let thread = |end: usize| {
        chain[..end]
            .iter()
            .find_map(|p| envs(p.pid).and_then(|e| env_thread(&e)))
    };
    let matched = chain.iter().enumerate().skip(1).find_map(|(i, p)| {
        let started = format_proc_start(p.start_time);
        sessions
            .iter()
            .find(|s| s.pid == p.pid && s.proc_start == started)
            .map(|s| (i, s))
    });
    let (session, conversation) = match matched {
        Some((depth, session)) => {
            if chain[..depth].iter().any(blocks) {
                return None;
            }
            let conversation = (session.agent == AgentKind::Codex)
                .then(|| thread(depth))
                .flatten()
                .filter(|t| holds_thread(session, t))
                .unwrap_or_else(|| session.conversation_id.clone());
            (session, conversation)
        }
        None => return None,
    };
    let cwd = server.cwd.as_deref()?;
    Some(TrackedServer {
        key: ServerKey {
            pid: server.pid,
            start_time: server.start_time,
        },
        port: listener.port,
        command: server.command.clone(),
        worktree: worktree_root(cwd),
        session_id: conversation,
        session_pid: session.pid,
        session_proc_start: session.proc_start.clone(),
        session_name: session_display_name(Some(&session.name), &session.cwd),
        first_seen_ms: now_ms,
        last_active_ms: now_ms,
        kept: false,
        alerted: false,
        note: None,
        asked_stale: false,
        agent: session.agent,
    })
}

/// `thread` is the session's conversation or one of its others.
fn holds_thread(s: &AgentSession, thread: &str) -> bool {
    s.conversation_id == thread || s.other_conversations.iter().any(|c| c == thread)
}

/// `true` when `pid` is the very process that started at `start_time`.
fn same_process(p: &dyn Platform, pid: i32, start_time: u64) -> bool {
    p.proc_info(pid).is_some_and(|i| i.start_time == start_time)
}

/// What one pass learned about a server, for the history.
struct Feed {
    key: ServerKey,
    /// `None` for an observed server.
    agent: Option<AgentKind>,
    procs: Vec<ProcId>,
    established: u32,
    /// Always `true` for an observed server, which has no session.
    session_alive: bool,
}

/// A tracked server's process tree as seen in one pass: the server and its
/// descendants, and every port any of them listens on. Empty when the
/// server's process is gone.
#[derive(Debug, Clone, Default)]
struct Tree {
    pids: Vec<i32>,
    /// `pids` with their start times (the server's own process first).
    procs: Vec<ProcId>,
    ports: Vec<u16>,
    /// Every (pid, port) whose established connections count as the
    /// server's use: every process of the tree and every port they listen
    /// on, a nested Claude Code subtree included (errs safe).
    activity: Vec<(i32, u16)>,
}

/// The descendants of `pid` that belong to its server, as (pid, start time):
/// a Claude Code process found among them is a session of its own, so it and
/// its whole subtree are left out (never signalled, never its ports); so is
/// any process for which `spare` holds, with its subtree.
fn own_descendants(p: &dyn Platform, pid: i32, spare: &dyn Fn(&str) -> bool) -> Vec<(i32, u64)> {
    let mut own = Vec::new();
    let mut spared = HashSet::new();
    for pid in p.descendants(pid) {
        if spared.contains(&pid) {
            continue;
        }
        let Some(info) = p.proc_info(pid) else {
            continue;
        };
        if is_agent_cli(&info.command).is_some() || spare(&info.command) {
            spared.insert(pid);
            spared.extend(p.descendants(pid));
            continue;
        }
        own.push((pid, info.start_time));
    }
    own
}

/// The server's tree leaves out any Claude Code process and its subtree:
/// a server started from such a nested session is not this server's, so
/// neither are its ports (`btp kill <port>` must never pick the outer one).
fn tree_of(p: &dyn Platform, s: &TrackedServer, listeners: &[Listener]) -> Tree {
    if !same_process(p, s.key.pid, s.key.start_time) {
        return Tree::default();
    }
    let mut procs = vec![(s.key.pid, s.key.start_time)];
    procs.extend(own_descendants(p, s.key.pid, &|_| false));
    let pids: Vec<i32> = procs.iter().map(|&(pid, _)| pid).collect();
    let mut ports: Vec<u16> = listeners
        .iter()
        .filter(|l| pids.contains(&l.pid))
        .map(|l| l.port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    let mut all = vec![s.key.pid];
    all.extend(p.descendants(s.key.pid));
    let mut all_ports: Vec<u16> = listeners
        .iter()
        .filter(|l| all.contains(&l.pid))
        .map(|l| l.port)
        .collect();
    all_ports.sort_unstable();
    all_ports.dedup();
    let activity = all
        .iter()
        .flat_map(|&pid| all_ports.iter().map(move |&port| (pid, port)))
        .collect();
    Tree {
        pids,
        procs,
        ports,
        activity,
    }
}

/// The tracked server (index into `servers`) that owns a process whose
/// ancestry is `chain`: the topmost tracked server among its ancestors below
/// the session `session_pid`. `chain[0]` (the process itself) is skipped.
fn owner_in_chain(
    chain: &[ProcInfo],
    session_pid: i32,
    servers: &[TrackedServer],
) -> Option<usize> {
    chain
        .iter()
        .skip(1)
        .take_while(|p| p.pid != session_pid)
        .filter_map(|p| {
            servers
                .iter()
                .position(|s| s.key.pid == p.pid && s.key.start_time == p.start_time)
        })
        .last()
}

/// Every process of the tracked trees.
fn tracked_pids(trees: &[Tree]) -> HashSet<i32> {
    trees.iter().flat_map(|t| t.pids.iter().copied()).collect()
}

/// Signals `pid` only if it is still the process we snapshotted, and never
/// pid <= 1 or ourselves.
fn signal_checked(p: &dyn Platform, pid: i32, start_time: u64, sig: i32) -> bool {
    pid > 1
        && pid as u32 != std::process::id()
        && same_process(p, pid, start_time)
        && p.signal(pid, sig)
}

/// SIGTERM to the server and its descendants (except a process of any agent
/// CLI or a command `cfg` excludes, each with its subtree), then SIGKILL to whatever
/// survives `grace` (polled every 100 ms). Every target is snapshotted as
/// (pid, start time) up front and re-checked right before each signal, so a
/// reused pid is never hit. Returns `false`, sending nothing, when `key` is
/// no longer the running process.
pub fn kill_tree(p: &dyn Platform, key: ServerKey, grace: Duration, cfg: &Config) -> bool {
    let Some(targets) = term_tree(p, key, cfg) else {
        return false;
    };
    finish_kill(p, &targets, Instant::now() + grace);
    true
}

/// The first stage of `kill_tree`: snapshots the tree as (pid, start time)
/// and sends SIGTERM to each target, start time re-checked right before.
/// `None`, sending nothing more, when `key` is no longer the running process
/// or refused the signal.
pub fn term_tree(p: &dyn Platform, key: ServerKey, cfg: &Config) -> Option<Vec<(i32, u64)>> {
    if !same_process(p, key.pid, key.start_time) {
        return None;
    }
    let mut targets = vec![(key.pid, key.start_time)];
    // A Claude Code process started from the server (and whatever it runs)
    // is a live session of its own, and an excluded command started by the
    // server (a database, say) is the user's to keep: neither is signalled,
    // nor anything they run.
    targets.extend(own_descendants(p, key.pid, &|c| is_excluded(c, cfg)));
    if !signal_checked(p, key.pid, key.start_time, libc::SIGTERM) {
        return None;
    }
    for &(pid, start) in &targets[1..] {
        signal_checked(p, pid, start, libc::SIGTERM);
    }
    Some(targets)
}

/// The targets that are still the very process snapshotted.
fn survivors(p: &dyn Platform, targets: &[(i32, u64)]) -> Vec<(i32, u64)> {
    targets
        .iter()
        .copied()
        .filter(|&(pid, start)| same_process(p, pid, start))
        .collect()
}

/// Polls every 100 ms until every target is gone or `deadline` passed;
/// returns the survivors.
pub fn wait_gone(p: &dyn Platform, targets: &[(i32, u64)], deadline: Instant) -> Vec<(i32, u64)> {
    let mut left = survivors(p, targets);
    loop {
        let now = Instant::now();
        if left.is_empty() || now >= deadline {
            return left;
        }
        std::thread::sleep(Duration::from_millis(100).min(deadline - now));
        left = survivors(p, &left);
    }
}

/// The last stage of `kill_tree`: waits for the targets until `deadline`,
/// then SIGKILL to each survivor whose start time still matches. Never
/// pid <= 1 or ourselves.
pub fn finish_kill(p: &dyn Platform, targets: &[(i32, u64)], deadline: Instant) {
    for (pid, start) in wait_gone(p, targets, deadline) {
        signal_checked(p, pid, start, libc::SIGKILL);
    }
}

/// Exclusive ownership of the watch loop (the app or `btp watch`). Released
/// on drop.
#[derive(Debug)]
pub struct WatchLock {
    _file: File,
}

/// Takes `watch.lock` in `dir` and writes `owner` into it. When another
/// process holds it, fails with `WouldBlock` and the message
/// `Already watched by <owner>`.
pub fn acquire_watch_lock(dir: &Path, owner: &str) -> io::Result<WatchLock> {
    std::fs::create_dir_all(dir)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("watch.lock"))?;
    match FileExt::try_lock(&file) {
        Ok(()) => {
            file.set_len(0)?;
            file.rewind()?;
            file.write_all(owner.as_bytes())?;
            file.flush()?;
            Ok(WatchLock { _file: file })
        }
        Err(TryLockError::WouldBlock) => {
            let mut held_by = String::new();
            let _ = file.read_to_string(&mut held_by);
            let held_by = held_by.trim();
            let held_by = if held_by.is_empty() {
                "another process"
            } else {
                held_by
            };
            Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("Already watched by {held_by}"),
            ))
        }
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// `true` while another process (the app or `btp watch`) holds
/// `watch.lock`. Probes with a try-lock released at once, never writes the
/// owner name. `false` when the file cannot be opened.
pub fn watch_lock_held(dir: &Path) -> bool {
    let Ok(file) = File::open(dir.join("watch.lock")) else {
        return false;
    };
    match FileExt::try_lock(&file) {
        Ok(()) => {
            let _ = FileExt::unlock(&file);
            false
        }
        Err(TryLockError::WouldBlock) => true,
        Err(TryLockError::Error(_)) => false,
    }
}

/// Owner written into `watch.lock` while the macOS app holds it.
pub const APP_OWNER: &str = "Balance ton port.app";

/// The owner name in `watch.lock` while some process holds it, `None` when
/// nobody does (or the file cannot be read). Empty while the holder has not
/// written it yet.
pub fn watch_lock_owner(dir: &Path) -> Option<String> {
    if !watch_lock_held(dir) {
        return None;
    }
    std::fs::read_to_string(dir.join("watch.lock"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// The lock owner is the app, which notifies session-ended groups itself.
/// `btp watch`, an unknown or not yet written owner, or no holder at all:
/// the hook posts.
pub fn app_owns_watch(owner: Option<&str>) -> bool {
    owner == Some(APP_OWNER)
}

/// Milliseconds since the Unix epoch, from the system clock.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Executes kills and forgets, reports alerts (marked only through
    /// `mark_alerted`, once delivered).
    Tick,
    /// Like `Tick` but alerts are ignored, not consumed.
    Sweep,
    /// Executes nothing but links, activity and forgets; reports the plan.
    Observe,
}

/// A live agent session (its process verified) and its worktree.
#[derive(Clone)]
struct LiveSession {
    session: AgentSession,
    root: PathBuf,
}

/// What one pass reads before touching the state.
struct Scan {
    cfg: Config,
    live: Vec<LiveSession>,
}

impl Scan {
    fn sessions(&self) -> Vec<AgentSession> {
        self.live.iter().map(|l| l.session.clone()).collect()
    }
}

pub struct Watcher<P: Platform> {
    platform: P,
    data_dir: PathBuf,
    /// Finds the Claude config dirs on its own; the config's dirs are extras.
    discover: fn() -> Vec<PathBuf>,
    /// Codex's home (`~/.codex`) when the process env names none.
    codex_home: Option<PathBuf>,
    /// Whether Codex sessions are followed (tests turn it off: a live Codex
    /// names its own `CODEX_HOME`, so a temp home cannot hide it).
    codex: bool,
    /// The last config read successfully: kept while `config.toml` is broken.
    last_good: Option<Config>,
    /// Why the last read of `config.toml` failed, cleared once it parses.
    config_error: Option<String>,
    history: History,
    meta: MetaCache,
    probes: ProbeCache,
    /// The servers of the last pass, as (key, port): the probe candidates.
    probe_targets: Vec<(ServerKey, u16)>,
    /// Pids whose connections to a server never count as use (the app's
    /// own process, which probes and previews).
    ignored_peers: Vec<i32>,
    /// Which listening processes are system ones, read once per process.
    system: SystemCache,
    /// The observed servers of the last `tick` or `observe`: a `sweep`
    /// keeps their history and probes.
    last_observed: Vec<ObservedServer>,
}

impl<P: Platform> Watcher<P> {
    pub fn new(platform: P, data_dir: PathBuf) -> Self {
        Watcher {
            platform,
            data_dir,
            discover: default_config_dirs,
            codex_home: dirs::home_dir().map(|h| h.join(".codex")),
            codex: true,
            last_good: None,
            config_error: None,
            history: History::default(),
            meta: MetaCache::default(),
            probes: ProbeCache::default(),
            probe_targets: Vec::new(),
            ignored_peers: Vec::new(),
            system: SystemCache::default(),
            last_observed: Vec::new(),
        }
    }

    /// Memory, CPU and events of every server seen by the passes so far.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// `(framework, package name, git branch)` of a server for its menu row
    /// and detail chips. Framework and package name are read once per server
    /// key, the branch again after 30 s. No subprocess.
    pub fn server_meta(
        &mut self,
        key: &ServerKey,
        command: &str,
        cwd: &Path,
        worktree: &Path,
        now_ms: u64,
    ) -> (Option<String>, Option<String>, Option<String>) {
        self.meta.get(key, command, cwd, worktree, now_ms)
    }

    /// Connections from these pids' sockets (loopback) are not use: set by
    /// the app to its own pid so its probes and previews never keep a server
    /// alive.
    pub fn with_ignored_peers(mut self, peers: Vec<i32>) -> Self {
        self.ignored_peers = peers;
        self
    }

    /// The `(server, port)` pairs of the last pass that need a title and
    /// favicon probe (two at most). Take them under the lock, release it,
    /// fetch with `probe::probe_port`, then hand the answers to
    /// `store_probes`: no network I/O happens while the watcher is borrowed.
    pub fn due_probes(&self, now_ms: u64) -> Vec<(ServerKey, u16)> {
        self.probes.due(&self.probe_targets, now_ms)
    }

    /// Records the answers of probes taken with `due_probes`.
    pub fn store_probes(&mut self, results: Vec<((ServerKey, u16), ProbeOutcome)>, now_ms: u64) {
        self.probes.store(results, now_ms);
        self.probes.retain(&self.probe_targets);
    }

    /// The cached page title and favicon of a server's port.
    pub fn page_meta(&self, key: ServerKey, port: u16) -> Option<&PageMeta> {
        self.probes.get(key, port)
    }

    /// Replaces the automatic discovery of Claude config dirs (tests pass
    /// `Vec::new` so only the configured extras are read).
    pub fn with_discovery(mut self, discover: fn() -> Vec<PathBuf>) -> Self {
        self.discover = discover;
        self
    }

    /// Replaces the default Codex home (tests pass `None` or a temp dir so the
    /// real `~/.codex` is never read).
    pub fn with_codex_home(mut self, home: Option<PathBuf>) -> Self {
        self.codex_home = home;
        self
    }

    /// Stops following Codex sessions, so tests on the real platform never
    /// see a live Codex session of the machine.
    pub fn without_codex(mut self) -> Self {
        self.codex = false;
        self
    }

    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// Why `config.toml` could not be used on the last read, if it could not.
    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// The config the watcher runs on: the last valid one read, else the
    /// defaults.
    pub fn config(&self) -> Config {
        self.last_good.clone().unwrap_or_default()
    }

    /// Reads `config.toml`. When it cannot be read or parsed, keeps running
    /// on the last valid config (defaults on a fresh start) and records why.
    fn load_config(&mut self) -> Config {
        match read_config(&self.data_dir) {
            Ok(c) => {
                self.last_good = Some(c.clone());
                self.config_error = None;
                c
            }
            Err(e) => {
                let fallback = match self.last_good {
                    Some(_) => "using the last valid config",
                    None => "using defaults",
                };
                self.config_error = Some(format!("{e}; {fallback}"));
                self.config()
            }
        }
    }

    /// One watch pass: link, observe, decide, then kill and forget. Alerts
    /// are reported, not consumed: call `mark_alerted` once delivered.
    pub fn tick(&mut self, now_ms: u64) -> io::Result<TickReport> {
        self.run(now_ms, Mode::Tick)
    }

    /// `tick` without the alert trigger: alerts are neither reported nor
    /// marked, so the app still sends them later.
    pub fn sweep(&mut self, now_ms: u64) -> io::Result<TickReport> {
        self.run(now_ms, Mode::Sweep)
    }

    /// `tick` that kills nothing and marks no alert. New links, port activity
    /// and dead servers are still recorded. `killed` holds the planned kills.
    pub fn observe(&mut self, now_ms: u64) -> io::Result<TickReport> {
        self.run(now_ms, Mode::Observe)
    }

    /// Kills a tracked server by hand. A port matches any port the server's
    /// tree listens on now, else its recorded port; a pid matches the
    /// server's own pid; a key matches only that exact process. Untracked
    /// processes are never killed: `Ok(None)`.
    pub fn kill(&mut self, target: KillTarget) -> io::Result<Option<KillRecord>> {
        let now = now_ms();
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let found = with_state(&self.data_dir, |state| {
            let trees = self.refresh(state, &scan, &listeners, now);
            let live = || {
                state
                    .servers
                    .iter()
                    .zip(&trees)
                    .filter(|(_, t)| !t.pids.is_empty())
            };
            let hit = match target {
                KillTarget::Pid(pid) => live().find(|(s, _)| s.key.pid == pid),
                KillTarget::Key(key) => live().find(|(s, _)| s.key == key),
                // A port the tree listens on now wins over a recorded port
                // that another process may have taken since.
                KillTarget::Port(port) => live()
                    .find(|(_, t)| t.ports.contains(&port))
                    .or_else(|| live().find(|(s, _)| s.port == port)),
            };
            hit.map(|(s, _)| s.clone())
        })?;
        let Some(server) = found else {
            return Ok(None);
        };
        Ok(self
            .execute(vec![(server, KillReason::Manual)], &scan.cfg, now)?
            .0
            .pop())
    }

    /// Stops an observed server by hand (the app only, after a
    /// confirmation). Re-scans first: `Ok(None)`, sending nothing, unless
    /// `key` is still an observed, non-excluded server with that start time.
    /// The kill re-checks every start time like any other.
    pub fn stop_observed(&mut self, key: ServerKey) -> io::Result<Option<KillRecord>> {
        let now = now_ms();
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let tracked = with_state(&self.data_dir, |state| {
            tracked_pids(&self.refresh(state, &scan, &listeners, now))
        })?;
        let Some(o) = self
            .observed(&listeners, &tracked, &scan.cfg)
            .into_iter()
            .find(|o| o.key == key && !o.excluded)
        else {
            return Ok(None);
        };
        let grace = Duration::from_secs(scan.cfg.kill_grace_s);
        if !kill_tree(&self.platform, key, grace, &scan.cfg) {
            return Ok(None);
        }
        let r = KillRecord {
            port: o.port,
            command: o.command,
            worktree: o.cwd.as_deref().map(worktree_root).unwrap_or_default(),
            reason: KillReason::Manual,
            at_ms: now,
        };
        with_state(&self.data_dir, |state| state.record_kill(r.clone()))?;
        append_kill_log(&self.data_dir, &r)?;
        Ok(Some(r))
    }

    /// The observed servers among `listeners`, outside the `tracked` trees.
    fn observed(
        &mut self,
        listeners: &[Listener],
        tracked: &HashSet<i32>,
        cfg: &Config,
    ) -> Vec<ObservedServer> {
        let found =
            observed_servers_cached(&self.platform, listeners, tracked, cfg, &mut self.system);
        self.system
            .retain(&listeners.iter().map(|l| l.pid).collect());
        found
    }

    /// What this pass learned about an observed server: its whole tree (an
    /// agent CLI subtree left out) and the connections on its ports.
    fn observed_feed(&self, o: &ObservedServer) -> Feed {
        let mut procs = vec![(o.key.pid, o.key.start_time)];
        procs.extend(own_descendants(&self.platform, o.key.pid, &|_| false));
        let established = procs
            .iter()
            .flat_map(|&(pid, _)| o.ports.iter().map(move |&port| (pid, port)))
            .map(|(pid, port)| {
                self.platform
                    .established_excluding(pid, port, &self.ignored_peers)
            })
            .sum();
        Feed {
            key: o.key,
            agent: None,
            procs,
            established,
            session_alive: true,
        }
    }

    /// Records that the alert for this exact server was delivered (the app
    /// calls it once the notification is posted): no more alerts until it
    /// restarts. A pass never marks alerts itself.
    pub fn mark_alerted(&mut self, key: ServerKey) -> io::Result<()> {
        with_state(&self.data_dir, |state| {
            for s in state.servers.iter_mut().filter(|s| s.key == key) {
                s.alerted = true;
            }
        })
    }

    /// No more alerts for this server until it restarts.
    pub fn keep(&mut self, key: ServerKey) -> io::Result<()> {
        with_state(&self.data_dir, |state| {
            for s in state.servers.iter_mut().filter(|s| s.key == key) {
                s.kept = true;
            }
        })
    }

    fn scan(&mut self) -> Scan {
        let cfg = self.load_config();
        let mut found: Vec<AgentSession> = Vec::new();
        for agent in self.agents(&cfg) {
            let mine: Vec<AgentSession> = agent
                .live_sessions_after(&self.platform, &found)
                .into_iter()
                .filter(|s| self.process_matches(s.pid, &s.proc_start))
                .collect();
            found.extend(mine);
        }
        let live = found
            .into_iter()
            .map(|s| LiveSession {
                root: canonical(&worktree_root(&s.cwd)),
                session: s,
            })
            .collect();
        Scan { cfg, live }
    }

    /// The agents whose sessions are followed, built from the current config.
    fn agents(&self, cfg: &Config) -> Vec<Box<dyn Agent>> {
        let mut agents: Vec<Box<dyn Agent>> = vec![Box::new(ClaudeAgent {
            dirs: effective_claude_dirs_with((self.discover)(), cfg),
        })];
        if self.codex {
            agents.push(Box::new(CodexAgent {
                default_home: self.codex_home.clone(),
            }));
        }
        agents
    }

    /// `pid` is running and started at `proc_start` (Claude's format).
    fn process_matches(&self, pid: i32, proc_start: &str) -> bool {
        !proc_start.is_empty()
            && self.platform.proc_start_string(pid).as_deref() == Some(proc_start)
    }

    /// Brings the tracked list up to date and returns each server's tree
    /// (same order as `state.servers`):
    /// - drops entries from before `session_proc_start` existed (they are
    ///   relinked below if their session still lives);
    /// - forgets servers that now match an exclusion (command, port, or an
    ///   ancestor below the session), so a later exclusion still protects;
    /// - links new listeners, except those inside a tracked server's tree
    ///   (a server is a process tree: its children's ports are its ports);
    /// - folds a tracked server into a tracked ancestor (a child that started
    ///   listening before its parent did), on every pass.
    fn refresh(
        &self,
        state: &mut State,
        scan: &Scan,
        listeners: &[Listener],
        now_ms: u64,
    ) -> Vec<Tree> {
        state.servers.retain(|s| !s.session_proc_start.is_empty());
        let sessions = scan.sessions();
        for l in listeners {
            // A tracked pid (same process, or a reused pid that this pass
            // forgets) is not linked again; extra ports of one process too.
            if state.servers.iter().any(|s| s.key.pid == l.pid) {
                continue;
            }
            let chain = ancestry(&self.platform, l.pid);
            let Some(new) = link(
                l,
                &chain,
                &|pid| self.platform.proc_env(pid),
                &sessions,
                &scan.cfg,
                now_ms,
            ) else {
                continue;
            };
            if owner_in_chain(&chain, new.session_pid, &state.servers).is_some() {
                continue;
            }
            state.servers.push(new);
        }
        // Every pass, not only when something new links: a state written
        // before servers were trees may still hold a separate child row.
        self.fold(state);
        let mut trees: Vec<Tree> = state
            .servers
            .iter()
            .map(|s| tree_of(&self.platform, s, listeners))
            .collect();
        let mut i = 0;
        while i < state.servers.len() {
            if self.excluded_now(&state.servers[i], &trees[i], &scan.cfg) {
                state.servers.remove(i);
                trees.remove(i);
            } else {
                i += 1;
            }
        }
        trees
    }

    /// Merges every tracked server that descends from another tracked server
    /// (below their session) into the topmost one.
    fn fold(&self, state: &mut State) {
        loop {
            let found = state.servers.iter().enumerate().find_map(|(i, s)| {
                let chain = ancestry(&self.platform, s.key.pid);
                let own = chain
                    .first()
                    .is_some_and(|p| p.pid == s.key.pid && p.start_time == s.key.start_time);
                if !own {
                    return None;
                }
                owner_in_chain(&chain, s.session_pid, &state.servers).map(|j| (i, j))
            });
            let Some((child, owner)) = found else {
                return;
            };
            let c = state.servers.remove(child);
            let owner = if owner > child { owner - 1 } else { owner };
            let o = &mut state.servers[owner];
            o.last_active_ms = o.last_active_ms.max(c.last_active_ms);
            o.kept |= c.kept;
            o.asked_stale |= c.asked_stale;
            o.note = merge_notes(o.note.take(), c.note);
        }
    }

    /// The server, any port of its tree, or a process between it and its
    /// session matches the current exclusions.
    fn excluded_now(&self, s: &TrackedServer, tree: &Tree, cfg: &Config) -> bool {
        if is_excluded(&s.command, cfg)
            || cfg.exclude_ports.contains(&s.port)
            || tree.ports.iter().any(|p| cfg.exclude_ports.contains(p))
        {
            return true;
        }
        if tree.pids.is_empty() {
            return false;
        }
        ancestry(&self.platform, s.key.pid)
            .iter()
            .skip(1)
            .take_while(|p| p.pid != s.session_pid)
            .any(|p| is_excluded(&p.command, cfg))
    }

    fn observe_one(&self, s: &TrackedServer, tree: &Tree, scan: &Scan, now_ms: u64) -> Observation {
        let alive = !tree.pids.is_empty();
        let own = |l: &&LiveSession| {
            l.session.pid == s.session_pid && l.session.proc_start == s.session_proc_start
        };
        let session_idle_ms = scan
            .live
            .iter()
            .find(own)
            .and_then(|l| session_idle_ms(&l.session, now_ms));
        let worktree = canonical(&s.worktree);
        let shared_with_live_session = scan.live.iter().any(|l| !own(&l) && l.root == worktree);
        Observation {
            key: s.key,
            alive,
            // Activity of the whole tree: any established connection on any
            // port the server or one of its descendants listens on, a
            // nested Claude Code subtree included.
            established: tree
                .activity
                .iter()
                .map(|&(pid, port)| {
                    self.platform
                        .established_excluding(pid, port, &self.ignored_peers)
                })
                .sum(),
            // Only a definite "does not exist" counts; an unreadable path
            // (permissions, unmounted volume) must not trigger a kill.
            worktree_exists: !matches!(s.worktree.try_exists(), Ok(false)),
            session_alive: self.process_matches(s.session_pid, &s.session_proc_start),
            session_idle_ms,
            shared_with_live_session,
        }
    }

    fn run(&mut self, now_ms: u64, mode: Mode) -> io::Result<TickReport> {
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let (mut report, to_kill, feeds, tracked) = with_state(&self.data_dir, |state| {
            let trees = self.refresh(state, &scan, &listeners, now_ms);
            let obs: Vec<Observation> = state
                .servers
                .iter()
                .zip(&trees)
                .map(|(s, t)| self.observe_one(s, t, &scan, now_ms))
                .collect();
            for (s, o) in state.servers.iter_mut().zip(&obs) {
                if o.alive {
                    touch(s, o, now_ms);
                }
            }
            let mut report = TickReport {
                config_error: self.config_error.clone(),
                ..TickReport::default()
            };
            let mut to_kill = Vec::new();
            let mut alerted = HashSet::new();
            let mut forget = HashSet::new();
            for action in decide(&state.servers, &obs, now_ms, &scan.cfg) {
                match action {
                    Action::Forget { key } => {
                        forget.insert(key);
                    }
                    Action::Alert { key, .. } => {
                        if mode != Mode::Sweep {
                            alerted.insert(key);
                        }
                    }
                    Action::Kill { key, reason } => {
                        if let Some(s) = state.servers.iter().find(|s| s.key == key) {
                            to_kill.push((s.clone(), reason));
                        }
                    }
                }
            }
            for ((s, o), t) in state.servers.iter_mut().zip(&obs).zip(&trees) {
                if forget.contains(&s.key) {
                    continue;
                }
                let session = scan
                    .live
                    .iter()
                    .find(|l| {
                        l.session.pid == s.session_pid
                            && l.session.proc_start == s.session_proc_start
                    })
                    .map(|l| session_display_name(Some(&l.session.name), &l.session.cwd));
                let view = view(s, o, t, session.as_deref(), now_ms, &scan.cfg);
                if alerted.contains(&s.key) {
                    report.alerts.push(view.clone());
                }
                report.servers.push(view);
            }
            state.servers.retain(|s| !forget.contains(&s.key));
            let feeds: Vec<Feed> = state
                .servers
                .iter()
                .zip(&obs)
                .zip(&trees)
                .map(|((s, o), t)| Feed {
                    key: s.key,
                    agent: Some(s.agent),
                    procs: t.procs.clone(),
                    established: o.established,
                    session_alive: o.session_alive,
                })
                .collect();
            (report, to_kill, feeds, tracked_pids(&trees))
        })?;
        let mut feeds = feeds;
        if mode != Mode::Sweep {
            report.observed = self.observed(&listeners, &tracked, &scan.cfg);
            self.last_observed = report.observed.clone();
            feeds.extend(report.observed.iter().map(|o| self.observed_feed(o)));
        }
        self.feed_history(&feeds, &report, now_ms);
        let mut live: HashSet<ServerKey> = report.servers.iter().map(|v| v.key).collect();
        live.extend(self.last_observed.iter().map(|o| o.key));
        self.history.retain(&live, now_ms);
        // An excluded server (a database, say) is never probed.
        self.probe_targets = report.servers.iter().map(|v| (v.key, v.port)).collect();
        self.probe_targets.extend(
            self.last_observed
                .iter()
                .filter(|o| !o.excluded)
                .map(|o| (o.key, o.port)),
        );

        if mode == Mode::Observe {
            report.killed = to_kill
                .into_iter()
                .map(|(s, reason)| record(&s, reason, now_ms))
                .collect();
            return Ok(report);
        }
        let (killed, gone) = self.execute(to_kill, &scan.cfg, now_ms)?;
        report.killed = killed;
        report.servers.retain(|v| !gone.contains(&v.key));
        Ok(report)
    }

    /// Feeds the history from one pass: a sample and the connection count of
    /// every live server, plus its lifecycle events.
    fn feed_history(&mut self, feeds: &[Feed], report: &TickReport, now_ms: u64) {
        for f in feeds {
            if self.history.track(f.key) {
                self.history.push_event(
                    f.key,
                    ServerEvent {
                        at_ms: now_ms,
                        kind: EventKind::Started { agent: f.agent },
                    },
                );
            }
            if !f.procs.is_empty() {
                let tree: Vec<(ProcId, ProcResources)> = f
                    .procs
                    .iter()
                    .filter_map(|&id| Some((id, self.platform.proc_resources(id.0)?)))
                    .collect();
                self.history.record(f.key, now_ms, &tree);
            }
            self.history.note_connections(f.key, now_ms, f.established);
            if !f.session_alive {
                self.history.note_session_ended(f.key, now_ms);
            }
        }
        for a in &report.alerts {
            self.history.note_alert(a.key, now_ms, AlertKind::Idle);
        }
    }

    /// Kills each server's tree outside the state lock (the grace period can
    /// last seconds), then records what was killed. Stops tracking every
    /// target except one that is still our process but refused the signal.
    /// Returns the kills and the keys no longer tracked.
    fn execute(
        &self,
        targets: Vec<(TrackedServer, KillReason)>,
        cfg: &Config,
        now_ms: u64,
    ) -> io::Result<(Vec<KillRecord>, HashSet<ServerKey>)> {
        if targets.is_empty() {
            return Ok((Vec::new(), HashSet::new()));
        }
        let grace = Duration::from_secs(cfg.kill_grace_s);
        let mut killed = Vec::new();
        let mut dropped = HashSet::new();
        for (s, reason) in &targets {
            // Kills can wait seconds each: an exclusion saved meanwhile
            // still protects the next target, with the same check as a pass
            // (command, any port of the tree, ancestors below the session).
            let fresh = read_config(&self.data_dir).unwrap_or_else(|_| cfg.clone());
            let tree = tree_of(&self.platform, s, &self.platform.listeners());
            if self.excluded_now(s, &tree, &fresh) {
                dropped.insert(s.key);
                continue;
            }
            if kill_tree(&self.platform, s.key, grace, &fresh) {
                killed.push(record(s, reason.clone(), now_ms));
            } else if same_process(&self.platform, s.key.pid, s.key.start_time) {
                // Still our process but the signal was refused (EPERM):
                // keep tracking it so a later pass can retry.
                eprintln!(
                    "btp: could not signal pid {} (port {}), still tracked",
                    s.key.pid, s.port
                );
                continue;
            }
            dropped.insert(s.key);
        }
        with_state(&self.data_dir, |state| {
            state.servers.retain(|s| !dropped.contains(&s.key));
            for r in &killed {
                state.record_kill(r.clone());
            }
        })?;
        for r in &killed {
            append_kill_log(&self.data_dir, r)?;
        }
        Ok((killed, dropped))
    }
}

/// Resolved path, so `/tmp/x` and `/private/tmp/x` compare equal; the path
/// as given when it cannot be resolved.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn record(s: &TrackedServer, reason: KillReason, now_ms: u64) -> KillRecord {
    KillRecord {
        port: s.port,
        command: s.command.clone(),
        worktree: s.worktree.clone(),
        reason,
        at_ms: now_ms,
    }
}

/// `live_session` is the display name of the server's session when it is
/// alive; it names servers tracked before session names were recorded.
fn view(
    s: &TrackedServer,
    o: &Observation,
    tree: &Tree,
    live_session: Option<&str>,
    now_ms: u64,
    cfg: &Config,
) -> ServerView {
    let port_idle = now_ms.saturating_sub(s.last_active_ms);
    let idle_ms = if o.session_alive {
        port_idle.max(o.session_idle_ms.unwrap_or(0))
    } else {
        port_idle
    };
    let kill_in_ms = if !o.worktree_exists {
        Some(0)
    } else if o.shared_with_live_session || o.session_alive {
        None
    } else {
        Some(
            cfg.dead_session_idle_s
                .saturating_mul(1000)
                .saturating_sub(port_idle),
        )
    };
    let worktree_name = s
        .worktree
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| s.worktree.display().to_string());
    ServerView {
        key: s.key,
        port: s.port,
        ports: tree.ports.clone(),
        command: s.command.clone(),
        tool_label: tool_label(&s.command),
        worktree: s.worktree.clone(),
        location_label: location_label(&s.worktree),
        session_name: if !s.session_name.is_empty() {
            s.session_name.clone()
        } else if let Some(name) = live_session.filter(|_| o.session_alive) {
            name.to_string()
        } else {
            format!("{} session", s.agent.label())
        },
        agent: s.agent,
        worktree_name,
        // Process start times are Unix microseconds.
        started_ms: s.key.start_time / 1000,
        session_alive: o.session_alive,
        idle_ms,
        kill_in_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::save_config;
    use crate::model::KillReason;
    use crate::platform::{ancestry, format_proc_start, FakePlatform};
    use crate::sessions::{read_sessions, SessionStatus};
    use crate::state::with_state;
    use std::fs;
    use tempfile::TempDir;

    pub(super) const MIN: u64 = 60_000;
    /// Fake wall clock, milliseconds.
    pub(super) const T0: u64 = 1_800_000_000_000;
    /// Base process start time, microseconds.
    pub(super) const START: u64 = 1_790_000_000_000_000;
    /// One second in microseconds: `procStart` has a one-second resolution.
    pub(super) const SEC: u64 = 1_000_000;
    pub(super) const TERM: i32 = libc::SIGTERM;
    pub(super) const KILL: i32 = libc::SIGKILL;

    /// launchd(1) > zsh(10) > claude(20, session "sess-a") > `bun run dev`
    /// (30, listens on 3000) > workerd (31). The server runs in
    /// `<repo>/wt/apps/web`, `<repo>/wt` being a linked worktree.
    pub(super) struct Fx {
        pub(super) data: TempDir,
        pub(super) claude: TempDir,
        pub(super) repo: TempDir,
        pub(super) wt: PathBuf,
        pub(super) w: Watcher<FakePlatform>,
    }

    impl Fx {
        pub(super) fn p(&self) -> &FakePlatform {
            self.w.platform()
        }

        pub(super) fn session(&self, pid: i32, id: &str, cwd: &Path, start: u64) {
            write_session(self.claude.path(), pid, id, cwd, start);
        }

        pub(super) fn cfg(&self) -> Config {
            crate::config::read_config(self.data.path()).unwrap()
        }

        pub(super) fn state_servers(&self) -> Vec<TrackedServer> {
            with_state(self.data.path(), |s| s.servers.clone()).unwrap()
        }

        pub(super) fn state_kills(&self) -> Vec<KillRecord> {
            with_state(self.data.path(), |s| s.recent_kills.clone()).unwrap()
        }

        pub(super) fn kill_log(&self) -> Vec<KillRecord> {
            let Ok(text) = fs::read_to_string(self.data.path().join("kills.log")) else {
                return Vec::new();
            };
            text.lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect()
        }
    }

    pub(super) fn write_session(claude: &Path, pid: i32, id: &str, cwd: &Path, start: u64) {
        let dir = claude.join("sessions");
        fs::create_dir_all(&dir).unwrap();
        let v = serde_json::json!({
            "pid": pid,
            "sessionId": id,
            "cwd": cwd,
            "status": "busy",
            "statusUpdatedAt": T0,
            "procStart": format_proc_start(start),
        });
        fs::write(dir.join(format!("{pid}.json")), v.to_string()).unwrap();
    }

    pub(super) fn fx() -> Fx {
        let data = tempfile::tempdir().unwrap();
        let claude = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let wt = repo.path().join("wt");
        fs::create_dir_all(wt.join("apps/web")).unwrap();
        fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        save_config(
            data.path(),
            &Config {
                claude_config_dirs: vec![claude.path().to_path_buf()],
                ..Config::default()
            },
        )
        .unwrap();
        let web = wt.join("apps/web");
        let p = FakePlatform::new();
        p.spawn(1, 0, START, "launchd", "/");
        p.spawn(10, 1, START + 10 * SEC, "zsh", "/");
        p.spawn(20, 10, START + 20 * SEC, "claude", wt.to_str().unwrap());
        p.spawn(
            30,
            20,
            START + 30 * SEC,
            "bun run dev",
            web.to_str().unwrap(),
        );
        p.spawn(
            31,
            30,
            START + 31 * SEC,
            "workerd serve",
            web.to_str().unwrap(),
        );
        p.add_listener(30, 3000);
        write_session(claude.path(), 20, "sess-a", &wt, START + 20 * SEC);
        let w = Watcher::new(p, data.path().to_path_buf())
            .with_discovery(Vec::new)
            .with_codex_home(None);
        Fx {
            data,
            claude,
            repo,
            wt,
            w,
        }
    }

    /// The fixture with a temp Codex home. A `codex` process (300, cwd
    /// `<wt>/apps/web`, so the session is named "web") holds the thread
    /// lock `T1` there: launchd > zsh(10) > codex(300).
    pub(super) fn codex_fx() -> Fx {
        let f = fx();
        let home = f.repo.path().join("codex-home");
        fs::create_dir_all(home.join("thread-writer-locks")).unwrap();
        let lock = home.join("thread-writer-locks/T1.lock");
        fs::write(&lock, "").unwrap();
        let web = f.wt.join("apps/web");
        f.p()
            .spawn(300, 10, START + 300 * SEC, "codex", web.to_str().unwrap());
        f.p()
            .set_open_paths(300, &[canonical(&lock).to_str().unwrap()]);
        let Fx {
            data,
            claude,
            repo,
            wt,
            w,
        } = f;
        Fx {
            data,
            claude,
            repo,
            wt,
            w: w.with_codex_home(Some(home)),
        }
    }

    /// codex(300) > sandbox-exec(301) > zsh(302) > `npm run dev`(303) >
    /// node(304, listens on 5173), the shape the brief describes.
    pub(super) fn codex_server_chain(f: &Fx) {
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p()
            .spawn(301, 300, START + 301 * SEC, "sandbox-exec -p x", web);
        f.p().spawn(302, 301, START + 302 * SEC, "zsh", web);
        f.p().spawn(303, 302, START + 303 * SEC, "npm run dev", web);
        f.p().spawn(304, 303, START + 304 * SEC, "node vite", web);
        f.p().add_listener(304, 5173);
    }

    fn server_of(f: &Fx, pid: i32) -> Option<TrackedServer> {
        f.state_servers().into_iter().find(|s| s.key.pid == pid)
    }

    #[test]
    fn links_server_under_codex_session_by_ancestry() {
        let mut f = codex_fx();
        codex_server_chain(&f);
        f.w.tick(T0).unwrap();
        let s = server_of(&f, 304).expect("linked");
        assert_eq!(s.agent, AgentKind::Codex);
        assert_eq!(s.session_id, "T1");
        assert_eq!(s.session_name, "web");
        assert_eq!(s.session_pid, 300);
    }

    #[test]
    fn links_server_that_is_a_direct_child_of_codex_with_its_thread_env() {
        let mut f = codex_fx();
        let web = f.wt.join("apps/web");
        f.p().spawn(
            310,
            300,
            START + 310 * SEC,
            "node vite",
            web.to_str().unwrap(),
        );
        f.p().add_listener(310, 5174);
        f.p().set_env(
            310,
            &[
                ("CODEX_THREAD_ID", "T1"),
                ("CODEX_SESSION_ID", "S"),
                ("X", "y"),
            ],
        );
        f.w.tick(T0).unwrap();
        let s = server_of(&f, 310).expect("linked");
        assert_eq!((s.agent, s.session_id.as_str()), (AgentKind::Codex, "T1"));
    }

    #[test]
    fn no_env_read_for_a_reparented_listener_without_codex_sessions() {
        let f = fx();
        let web = f.wt.join("apps/web");
        f.p().spawn(
            310,
            1,
            START + 310 * SEC,
            "node vite",
            web.to_str().unwrap(),
        );
        let chain = ancestry(f.p(), 310);
        let calls = std::cell::Cell::new(0);
        let envs = |_: i32| {
            calls.set(calls.get() + 1);
            None
        };
        let l = Listener {
            pid: 310,
            port: 5174,
        };
        assert_eq!(link(&l, &chain, &envs, &sessions(&f), &f.cfg(), T0), None);
        assert_eq!(calls.get(), 0);
    }

    /// The live Codex session of `codex_fx` (pid 300, thread T1).
    fn codex_session(f: &Fx) -> AgentSession {
        AgentSession {
            agent: AgentKind::Codex,
            pid: 300,
            proc_start: format_proc_start(START + 300 * SEC),
            conversation_id: "T1".into(),
            other_conversations: Vec::new(),
            cwd: f.wt.join("apps/web"),
            name: String::new(),
            status: SessionStatus::Unknown,
            status_updated_at_ms: T0,
        }
    }

    #[test]
    fn env_never_read_without_a_matched_codex_session() {
        let f = codex_fx();
        let web = f.wt.join("apps/web");
        f.p().spawn(
            310,
            1,
            START + 310 * SEC,
            "node vite",
            web.to_str().unwrap(),
        );
        let chain = ancestry(f.p(), 310);
        let calls = std::cell::Cell::new(0);
        let envs = |_: i32| {
            calls.set(calls.get() + 1);
            Some(vec![("CODEX_THREAD_ID".to_string(), "T1".to_string())])
        };
        let l = Listener {
            pid: 310,
            port: 5174,
        };
        let mut live = sessions(&f);
        live.push(codex_session(&f));
        assert_eq!(link(&l, &chain, &envs, &live, &f.cfg(), T0), None);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn reparented_server_with_a_live_codex_thread_env_is_not_linked() {
        let mut f = codex_fx();
        let web = f.wt.join("apps/web");
        f.p().spawn(
            310,
            1,
            START + 310 * SEC,
            "node vite",
            web.to_str().unwrap(),
        );
        f.p().add_listener(310, 5174);
        f.p().set_env(310, &[("CODEX_THREAD_ID", "T1")]);
        f.w.tick(T0).unwrap();
        assert_eq!(server_of(&f, 310), None);
    }

    /// A user-started server in a shell that inherited `CODEX_THREAD_ID`
    /// of a live Codex session, under `host` (not a session) > launchd.
    fn server_in_inherited_shell_is_not_linked(host: &str) {
        let mut f = codex_fx();
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p().spawn(400, 1, START + 400 * SEC, host, "/");
        f.p().spawn(401, 400, START + 401 * SEC, "zsh", web);
        f.p().spawn(402, 401, START + 402 * SEC, "node vite", web);
        f.p().add_listener(402, 5175);
        for pid in [401, 402] {
            f.p().set_env(pid, &[("CODEX_THREAD_ID", "T1")]);
        }
        f.w.tick(T0).unwrap();
        assert_eq!(server_of(&f, 402), None);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn server_in_tmux_shell_with_codex_thread_env_is_not_linked() {
        server_in_inherited_shell_is_not_linked("tmux");
    }

    #[test]
    fn server_in_editor_shell_with_codex_thread_env_is_not_linked() {
        server_in_inherited_shell_is_not_linked(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper",
        );
    }

    #[test]
    fn a_claude_server_never_takes_a_codex_thread_from_its_env() {
        let mut f = codex_fx();
        // The fixture's server (30) is under Claude (20); its env names T1.
        f.p().set_env(30, &[("CODEX_THREAD_ID", "T1")]);
        f.w.tick(T0).unwrap();
        let s = server_of(&f, 30).expect("linked");
        assert_eq!((s.agent, s.session_pid), (AgentKind::ClaudeCode, 20));
        assert_eq!(s.session_id, "sess-a");
    }

    #[test]
    fn claude_inside_codex_shell_links_to_claude() {
        let mut f = codex_fx();
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p().spawn(301, 300, START + 301 * SEC, "zsh", web);
        f.p().spawn(70, 301, START + 70 * SEC, "claude", web);
        f.session(70, "sess-in", &f.wt.join("apps/web"), START + 70 * SEC);
        f.p().spawn(71, 70, START + 71 * SEC, "bun dev", web);
        f.p().add_listener(71, 5180);
        f.p().set_env(71, &[("CODEX_THREAD_ID", "T1")]);
        f.w.tick(T0).unwrap();
        let s = server_of(&f, 71).expect("linked");
        assert_eq!(s.agent, AgentKind::ClaudeCode);
        assert_eq!(s.session_pid, 70);
        assert_eq!(s.session_id, "sess-in");
    }

    #[test]
    fn codex_exec_inside_claude_tree_server_belongs_to_claude() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p().spawn(60, 20, START + 60 * SEC, "zsh", web);
        f.p()
            .spawn(61, 60, START + 61 * SEC, "codex exec fix it", web);
        f.p().spawn(62, 61, START + 62 * SEC, "zsh", web);
        f.p().spawn(63, 62, START + 63 * SEC, "vite", web);
        f.p().add_listener(63, 5190);
        f.w.tick(T0).unwrap();
        let s = server_of(&f, 63).expect("linked");
        assert_eq!((s.agent, s.session_pid), (AgentKind::ClaudeCode, 20));
    }

    #[test]
    fn codex_inside_server_tree_is_spared_on_kill() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p()
            .spawn(60, 30, START + 60 * SEC, "codex exec fix it", web);
        f.p().spawn(61, 60, START + 61 * SEC, "vite", web);
        f.p().add_listener(61, 5191);
        f.w.tick(T0).unwrap();
        assert!(f.w.kill(KillTarget::Port(3000)).unwrap().is_some());
        assert!(f
            .p()
            .signals()
            .iter()
            .all(|&(pid, _)| pid != 60 && pid != 61));
        assert!(f.p().is_alive(60) && f.p().is_alive(61));
    }

    #[test]
    fn codex_app_server_listener_never_tracked() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        f.p().spawn(
            60,
            20,
            START + 60 * SEC,
            "codex app-server --listen ws://127.0.0.1:7000",
            web.to_str().unwrap(),
        );
        f.p().add_listener(60, 7000);
        f.w.tick(T0).unwrap();
        assert_eq!(server_of(&f, 60), None);
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn claude_and_codex_in_same_worktree_are_shared() {
        let mut f = codex_fx();
        // The Codex session's cwd is the worktree root.
        f.p().remove_proc(300);
        f.p()
            .spawn(300, 10, START + 300 * SEC, "codex", f.wt.to_str().unwrap());
        let lock = f.repo.path().join("codex-home/thread-writer-locks/T1.lock");
        f.p()
            .set_open_paths(300, &[canonical(&lock).to_str().unwrap()]);
        f.w.tick(T0).unwrap();
        assert!(server_of(&f, 30).is_some());
        f.p().remove_proc(20); // the Claude session exits
        let killed = tick_minutes(&mut f, T0, T0 + 30 * MIN);
        assert!(killed.is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn codex_dead_session_idle_kill_after_20_min() {
        let mut f = codex_fx();
        codex_server_chain(&f);
        f.w.tick(T0).unwrap();
        // Both sessions share the worktree: both must be gone.
        f.p().remove_proc(300);
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 19 * MIN).unwrap();
        assert!(r.killed.is_empty());
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        let rec = r
            .killed
            .iter()
            .find(|k| k.port == 5173)
            .expect("codex server killed");
        assert_eq!(
            rec.reason,
            KillReason::DeadSessionIdle { idle_ms: 20 * MIN }
        );
        assert!(!f.p().is_alive(304));
        assert!(f.kill_log().iter().any(|k| k.port == 5173));
    }

    pub(super) fn key30() -> ServerKey {
        ServerKey {
            pid: 30,
            start_time: START + 30 * SEC,
        }
    }

    fn sessions(f: &Fx) -> Vec<AgentSession> {
        read_sessions(&[f.claude.path().to_path_buf()])
            .into_iter()
            .map(AgentSession::from)
            .collect()
    }

    fn link_pid(f: &Fx, pid: i32, port: u16) -> Option<TrackedServer> {
        let chain = ancestry(f.p(), pid);
        link(
            &Listener { pid, port },
            &chain,
            &|_| None,
            &sessions(f),
            &f.cfg(),
            T0,
        )
    }

    #[test]
    fn links_carry_the_session_agent() {
        let f = fx();
        let s = link_pid(&f, 30, 3000).expect("linked");
        assert_eq!(s.agent, AgentKind::ClaudeCode);
        assert_eq!(s.session_id, "sess-a");
    }

    #[test]
    fn links_server_under_claude_session() {
        let mut f = fx();
        let s = link_pid(&f, 30, 3000).expect("linked");
        assert_eq!(
            s,
            TrackedServer {
                key: key30(),
                port: 3000,
                command: "bun run dev".into(),
                worktree: f.wt.clone(),
                session_id: "sess-a".into(),
                session_pid: 20,
                session_proc_start: format_proc_start(START + 20 * SEC),
                session_name: "wt".into(),
                first_seen_ms: T0,
                last_active_ms: T0,
                kept: false,
                alerted: false,
                note: None,
                asked_stale: false,
                agent: crate::agents::AgentKind::ClaudeCode,
            }
        );

        let r = f.w.tick(T0).unwrap();
        assert_eq!(
            r.servers,
            vec![ServerView {
                key: key30(),
                port: 3000,
                ports: vec![3000],
                command: "bun run dev".into(),
                tool_label: "Bun dev".into(),
                worktree: f.wt.clone(),
                worktree_name: "wt".into(),
                location_label: "main \u{203A} wt".into(),
                session_name: "wt".into(),
                agent: AgentKind::ClaudeCode,
                started_ms: (START + 30 * SEC) / 1000,
                session_alive: true,
                idle_ms: 0,
                kill_in_ms: None,
            }]
        );
        assert_eq!(f.state_servers(), vec![s]);
    }

    #[test]
    fn link_keeps_the_session_name() {
        let f = fx();
        let path = f.claude.path().join("sessions/20.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        v["name"] = "revva-b8".into();
        fs::write(&path, v.to_string()).unwrap();
        let s = link_pid(&f, 30, 3000).expect("linked");
        assert_eq!(s.session_name, "revva-b8");
    }

    #[test]
    fn legacy_server_is_named_after_its_live_session_else_generic() {
        let mut f = fx();
        let other = f.repo.path().join("other");
        fs::create_dir_all(&other).unwrap();
        f.w.tick(T0).unwrap();
        // Tracked before session names existed, in another directory than
        // the session's.
        with_state(f.data.path(), |s| {
            s.servers[0].session_name = String::new();
            s.servers[0].worktree = other.clone();
        })
        .unwrap();
        let r = f.w.tick(T0 + 1_000).unwrap();
        assert_eq!(r.servers[0].session_name, "wt");
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 2_000).unwrap();
        assert_eq!(r.servers[0].session_name, "Claude session");
    }

    #[test]
    fn ignores_server_without_claude_ancestor() {
        let mut f = fx();
        // A server started from a plain terminal.
        f.p()
            .spawn(40, 10, START + 40 * SEC, "python -m http.server", "/tmp");
        f.p().add_listener(40, 8000);
        assert_eq!(link_pid(&f, 40, 8000), None);
        // A session file whose pid now belongs to another process (stale
        // procStart): never a link.
        f.p().spawn(50, 10, START + 50 * SEC, "claude", "/tmp");
        f.p().spawn(51, 50, START + 51 * SEC, "vite", "/tmp");
        f.p().add_listener(51, 5173);
        f.session(50, "stale", Path::new("/tmp"), START + 7 * SEC);
        assert_eq!(link_pid(&f, 51, 5173), None);
        // A listener whose chain does not start with itself.
        let chain = ancestry(f.p(), 30);
        let l = Listener {
            pid: 31,
            port: 9999,
        };
        assert_eq!(
            link(&l, &chain, &|_| None, &sessions(&f), &f.cfg(), T0),
            None
        );

        let r = f.w.tick(T0).unwrap();
        let ports: Vec<u16> = r.servers.iter().map(|v| v.port).collect();
        assert_eq!(ports, vec![3000]);
    }

    #[test]
    fn claude_process_without_session_file_not_linked() {
        let mut f = fx();
        let wt = f.wt.to_str().unwrap().to_string();
        // A Claude CLI with no watched session file, listening (IDE, OAuth)
        // under a live watched session.
        f.p().spawn(70, 20, START + 70 * SEC, "claude", &wt);
        f.p().add_listener(70, 7300);
        assert_eq!(link_pid(&f, 70, 7300), None);
        f.p().spawn(
            71,
            20,
            START + 71 * SEC,
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
            &wt,
        );
        f.p().add_listener(71, 7301);
        assert_eq!(link_pid(&f, 71, 7301), None);
        // A server under such an unwatched nested Claude belongs to it, not
        // to the outer session.
        f.p().spawn(
            72,
            20,
            START + 72 * SEC,
            "/opt/homebrew/bin/claude -p x",
            &wt,
        );
        f.p().spawn(73, 72, START + 73 * SEC, "vite", &wt);
        f.p().add_listener(73, 5174);
        assert_eq!(link_pid(&f, 73, 5174), None);
        // A command merely mentioning claude is still a server.
        f.p()
            .spawn(74, 20, START + 74 * SEC, "node claude-dashboard.js", &wt);
        f.p().add_listener(74, 7302);
        assert!(link_pid(&f, 74, 7302).is_some());

        let r = f.w.tick(T0).unwrap();
        let ports: Vec<u16> = r.servers.iter().map(|v| v.port).collect();
        assert_eq!(ports, vec![3000, 7302]);
    }

    #[test]
    fn unsignallable_server_stays_tracked() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().fail_signals(30); // e.g. EPERM
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert_eq!(r.servers.len(), 1);
        assert!(f.p().is_alive(30));
        assert_eq!(f.state_servers().len(), 1);
        assert!(f.kill_log().is_empty());
    }

    #[test]
    fn excluded_command_not_linked() {
        let mut f = fx();
        let wt = f.wt.to_str().unwrap().to_string();
        // Excluded by its own command.
        f.p()
            .spawn(60, 20, START + 60 * SEC, "node /x/mcp-server.js", &wt);
        f.p().add_listener(60, 7000);
        assert_eq!(link_pid(&f, 60, 7000), None);
        // Excluded through an intermediate ancestor below the session.
        f.p()
            .spawn(61, 20, START + 61 * SEC, "argent tool-server", &wt);
        f.p()
            .spawn(62, 61, START + 62 * SEC, "simulator-server", &wt);
        f.p().add_listener(62, 7100);
        assert_eq!(link_pid(&f, 62, 7100), None);
        // Matching is case-insensitive.
        f.p()
            .spawn(63, 20, START + 63 * SEC, "/opt/Postgres/bin/postgres", &wt);
        f.p().add_listener(63, 5432);
        assert_eq!(link_pid(&f, 63, 5432), None);
        // Claude Code's own listener.
        f.p().add_listener(20, 9000);
        assert_eq!(link_pid(&f, 20, 9000), None);

        let r = f.w.tick(T0).unwrap();
        let ports: Vec<u16> = r.servers.iter().map(|v| v.port).collect();
        assert_eq!(ports, vec![3000]);
    }

    #[test]
    fn tick_kills_after_dead_session_idle_and_logs() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20); // the session exits

        let r = f.w.tick(T0 + 19 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert_eq!(r.servers.len(), 1);
        assert!(!r.servers[0].session_alive);
        assert_eq!(r.servers[0].idle_ms, 19 * MIN);
        assert_eq!(r.servers[0].kill_in_ms, Some(MIN));
        assert!(f.p().signals().is_empty());

        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        let rec = KillRecord {
            port: 3000,
            command: "bun run dev".into(),
            worktree: f.wt.clone(),
            reason: KillReason::DeadSessionIdle { idle_ms: 20 * MIN },
            at_ms: T0 + 20 * MIN,
        };
        assert_eq!(r.killed, vec![rec.clone()]);
        assert!(r.servers.is_empty());
        assert!(!f.p().is_alive(30));
        assert!(!f.p().is_alive(31));
        assert_eq!(f.p().signals(), vec![(30, TERM), (31, TERM)]);
        assert_eq!(f.kill_log(), vec![rec.clone()]);
        assert_eq!(f.state_kills(), vec![rec]);
        assert!(f.state_servers().is_empty());
    }

    #[test]
    fn missing_session_file_never_means_dead() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        fs::remove_file(f.claude.path().join("sessions/20.json")).unwrap();
        let r = f.w.tick(T0 + 60 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert!(r.servers[0].session_alive);
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn session_pid_reused_counts_as_dead() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        f.p()
            .spawn(20, 10, START + 9_999 * SEC, "claude", "/elsewhere");
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert_eq!(r.killed.len(), 1);
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn worktree_deleted_kills_now() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        fs::remove_dir_all(&f.wt).unwrap();
        let r = f.w.tick(T0 + 1_000).unwrap();
        assert_eq!(r.killed.len(), 1);
        assert_eq!(r.killed[0].reason, KillReason::WorktreeDeleted);
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn tick_reports_an_alert_until_it_is_marked() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        assert!(f.w.tick(T0 + 29 * MIN).unwrap().alerts.is_empty());
        let r = f.w.tick(T0 + 30 * MIN).unwrap();
        assert_eq!(r.alerts.len(), 1);
        assert_eq!(r.alerts[0].key, key30());
        assert_eq!(r.alerts[0].idle_ms, 30 * MIN);
        // Not consumed by the pass: the notification may not have been shown.
        assert!(!f.state_servers()[0].alerted);
        assert_eq!(f.w.tick(T0 + 31 * MIN).unwrap().alerts.len(), 1);
        // Marked once the app has actually posted it.
        f.w.mark_alerted(key30()).unwrap();
        assert!(f.state_servers()[0].alerted);
        let r = f.w.tick(T0 + 32 * MIN).unwrap();
        assert!(r.alerts.is_empty());
        assert!(r.killed.is_empty());
        assert!(f.p().is_alive(30));
        // Another process with the same pid is not marked.
        f.w.mark_alerted(ServerKey {
            pid: 30,
            start_time: 1,
        })
        .unwrap();
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn history_fed_each_pass() {
        let mut f = fx();
        f.p().set_resources(30, 100 * 1_048_576, 1_000_000_000);
        f.p().set_resources(31, 50 * 1_048_576, 0);
        f.w.tick(T0).unwrap();
        f.p().set_resources(30, 120 * 1_048_576, 1_500_000_000);
        f.w.tick(T0 + 1000).unwrap();
        let h = f.w.history();
        let s = h.samples(key30());
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].rss_bytes, 150 * 1_048_576);
        assert_eq!(s[1].rss_bytes, 170 * 1_048_576);
        assert!(s[1].cpu_pct > 0.0);
        let started = h
            .events(key30())
            .iter()
            .filter(|e| matches!(e.kind, EventKind::Started { .. }))
            .count();
        assert_eq!(started, 1);
    }

    #[test]
    fn history_records_alert_and_session_end_once() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.w.tick(T0 + 30 * MIN).unwrap();
        f.w.tick(T0 + 31 * MIN).unwrap();
        let alerts = |f: &Fx| {
            f.w.history()
                .events(key30())
                .iter()
                .filter(|e| matches!(e.kind, EventKind::Alert { .. }))
                .count()
        };
        assert_eq!(alerts(&f), 1);
        f.p().remove_proc(20);
        f.w.observe(T0 + 32 * MIN).unwrap();
        f.w.observe(T0 + 33 * MIN).unwrap();
        let ended =
            f.w.history()
                .events(key30())
                .iter()
                .filter(|e| e.kind == EventKind::SessionEnded)
                .count();
        assert_eq!(ended, 1);
    }

    #[test]
    fn session_end_recorded_when_the_tree_is_already_empty() {
        // A dead server is normally forgotten in the same pass; the history
        // must not depend on that, so feed it an empty tree directly.
        let mut f = fx();
        let feed = Feed {
            key: key30(),
            agent: Some(AgentKind::ClaudeCode),
            procs: Vec::new(),
            established: 0,
            session_alive: false,
        };
        let report = TickReport {
            servers: vec![],
            ..TickReport::default()
        };
        f.w.feed_history(&[feed], &report, T0);
        let kinds: Vec<_> =
            f.w.history()
                .events(key30())
                .iter()
                .map(|e| e.kind.clone())
                .collect();
        assert!(kinds.contains(&EventKind::SessionEnded), "{kinds:?}");
    }

    #[test]
    fn sweep_and_observe_never_set_alerted() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        assert!(f.w.sweep(T0 + 30 * MIN).unwrap().alerts.is_empty());
        // observe reports what tick would alert, without consuming it.
        assert_eq!(f.w.observe(T0 + 30 * MIN).unwrap().alerts.len(), 1);
        assert!(!f.state_servers()[0].alerted);
        assert_eq!(f.w.tick(T0 + 31 * MIN).unwrap().alerts.len(), 1);
    }

    #[test]
    fn observe_plans_kills_without_killing() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        let r = f.w.observe(T0 + 20 * MIN).unwrap();
        assert_eq!(r.killed.len(), 1);
        assert_eq!(r.servers.len(), 1);
        assert!(f.p().is_alive(30));
        assert!(f.p().signals().is_empty());
        assert!(f.kill_log().is_empty());
        assert_eq!(f.w.sweep(T0 + 20 * MIN).unwrap().killed.len(), 1);
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn observe_links_and_persists() {
        let mut f = fx();
        assert_eq!(f.w.observe(T0).unwrap().servers.len(), 1);
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn shared_worktree_not_killed() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        f.p()
            .spawn(21, 10, START + 21 * SEC, "claude", web.to_str().unwrap());
        f.session(21, "sess-b", &web, START + 21 * SEC);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);

        let r = f.w.tick(T0 + 60 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert_eq!(r.servers[0].kill_in_ms, None);
        assert!(f.p().is_alive(30));

        // Once the other session is gone too, the server is a leftover.
        f.p().remove_proc(21);
        assert_eq!(f.w.tick(T0 + 61 * MIN).unwrap().killed.len(), 1);
    }

    #[test]
    fn sharing_compares_resolved_paths() {
        let mut f = fx();
        // The other session reports its cwd through a symlink.
        let link_dir = f.repo.path().join("alias");
        std::os::unix::fs::symlink(&f.wt, &link_dir).unwrap();
        f.p().spawn(
            21,
            10,
            START + 21 * SEC,
            "claude",
            link_dir.to_str().unwrap(),
        );
        f.session(21, "sess-b", &link_dir, START + 21 * SEC);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        assert!(f.w.tick(T0 + 60 * MIN).unwrap().killed.is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn own_session_does_not_count_as_sharing() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        // The server's own session is alive, in the same worktree: alerts
        // still fire (sharing only means another live session).
        assert_eq!(f.w.tick(T0 + 30 * MIN).unwrap().alerts.len(), 1);
    }

    #[test]
    fn keep_silences_alerts() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.w.keep(key30()).unwrap();
        assert!(f.state_servers()[0].kept);
        assert!(f.w.tick(T0 + 60 * MIN).unwrap().alerts.is_empty());
    }

    #[test]
    fn kill_tree_escalates_to_sigkill() {
        let p = FakePlatform::new();
        p.spawn(30, 1, 100, "srv", "/");
        p.spawn(31, 30, 101, "child", "/");
        p.ignore_sigterm(30);
        let key = ServerKey {
            pid: 30,
            start_time: 100,
        };
        assert!(kill_tree(
            &p,
            key,
            Duration::from_millis(300),
            &Config::default()
        ));
        assert_eq!(p.signals(), vec![(30, TERM), (31, TERM), (30, KILL)]);
        assert!(!p.is_alive(30));
        assert!(!p.is_alive(31));
    }

    /// Replaces `pid` by an unrelated process right after it receives
    /// SIGTERM (it ignores it), as if the pid were reused at once.
    struct SwapAfterTerm<'a> {
        inner: &'a FakePlatform,
        pid: i32,
    }

    impl Platform for SwapAfterTerm<'_> {
        fn listeners(&self) -> Vec<Listener> {
            self.inner.listeners()
        }
        fn proc_info(&self, pid: i32) -> Option<ProcInfo> {
            self.inner.proc_info(pid)
        }
        fn established(&self, pid: i32, port: u16) -> u32 {
            self.inner.established(pid, port)
        }
        fn descendants(&self, pid: i32) -> Vec<i32> {
            self.inner.descendants(pid)
        }
        fn signal(&self, pid: i32, sig: i32) -> bool {
            let delivered = self.inner.signal(pid, sig);
            if pid == self.pid && sig == TERM {
                self.inner.remove_proc(pid);
                self.inner.spawn(pid, 1, 5_000, "unrelated", "/");
            }
            delivered
        }
        fn proc_start_string(&self, pid: i32) -> Option<String> {
            self.inner.proc_start_string(pid)
        }
        fn proc_env(&self, pid: i32) -> Option<Vec<(String, String)>> {
            self.inner.proc_env(pid)
        }
        fn open_paths(&self, pid: i32) -> Vec<std::path::PathBuf> {
            self.inner.open_paths(pid)
        }
        fn pids_named(&self, name: &str) -> Vec<i32> {
            self.inner.pids_named(name)
        }
        fn proc_resources(&self, pid: i32) -> Option<crate::platform::ProcResources> {
            self.inner.proc_resources(pid)
        }
        fn machine_memory(&self) -> Option<crate::platform::MachineMemory> {
            self.inner.machine_memory()
        }
        fn proc_argv(&self, pid: i32) -> Option<Vec<String>> {
            self.inner.proc_argv(pid)
        }
        fn exe_path(&self, pid: i32) -> Option<std::path::PathBuf> {
            self.inner.exe_path(pid)
        }
    }

    #[test]
    fn kill_tree_never_signals_a_reused_pid() {
        let p = FakePlatform::new();
        p.spawn(30, 1, 100, "srv", "/");
        let stale = ServerKey {
            pid: 30,
            start_time: 99,
        };
        assert!(!kill_tree(
            &p,
            stale,
            Duration::from_millis(100),
            &Config::default()
        ));
        assert!(p.signals().is_empty());
        assert!(p.is_alive(30));

        // A descendant replaced by an unrelated process during the grace
        // period gets SIGTERM (it was ours) but never SIGKILL.
        p.spawn(31, 30, 101, "child", "/");
        p.ignore_sigterm(30);
        p.ignore_sigterm(31);
        let key = ServerKey {
            pid: 30,
            start_time: 100,
        };
        // The swap happens synchronously right after 31 gets its SIGTERM,
        // before any poll: no timing involved.
        let swapping = SwapAfterTerm { inner: &p, pid: 31 };
        assert!(kill_tree(
            &swapping,
            key,
            Duration::from_millis(200),
            &Config::default()
        ));
        assert!(p.signals().contains(&(31, TERM)));
        assert!(!p.signals().contains(&(31, KILL)));
        assert!(p.is_alive(31));
        assert!(!p.is_alive(30));
    }

    #[test]
    fn manual_kill_by_port() {
        let mut f = fx();
        // Not tracked: never killed by hand either.
        f.p()
            .spawn(40, 10, START + 40 * SEC, "python -m http.server", "/tmp");
        f.p().add_listener(40, 8000);
        assert_eq!(f.w.kill(KillTarget::Port(8000)).unwrap(), None);
        assert_eq!(f.w.kill(KillTarget::Port(9999)).unwrap(), None);
        assert!(f.p().is_alive(40));

        // kill links on its own: no prior tick needed.
        let rec = f.w.kill(KillTarget::Port(3000)).unwrap().expect("killed");
        assert_eq!(rec.port, 3000);
        assert_eq!(rec.reason, KillReason::Manual);
        assert_eq!(rec.worktree, f.wt);
        assert!(!f.p().is_alive(30));
        assert!(!f.p().is_alive(31));
        assert!(f.p().is_alive(40));
        assert_eq!(f.kill_log(), vec![rec.clone()]);
        assert_eq!(f.state_kills(), vec![rec]);
        assert!(f.state_servers().is_empty());
    }

    #[test]
    fn manual_kill_by_pid() {
        let mut f = fx();
        f.p()
            .spawn(40, 10, START + 40 * SEC, "python -m http.server", "/tmp");
        f.p().add_listener(40, 8000);
        assert_eq!(f.w.kill(KillTarget::Pid(40)).unwrap(), None);
        // A pid equal to a tracked port matches nothing.
        assert_eq!(f.w.kill(KillTarget::Pid(3000)).unwrap(), None);
        assert!(f.w.kill(KillTarget::Pid(30)).unwrap().is_some());
        assert!(!f.p().is_alive(30));
        assert!(f.p().is_alive(40));
    }

    #[test]
    fn legacy_server_without_session_start_is_relinked() {
        let mut f = fx();
        with_state(f.data.path(), |s| {
            s.servers.push(TrackedServer {
                key: key30(),
                port: 3000,
                command: "bun run dev".into(),
                worktree: "/gone".into(),
                session_id: "old".into(),
                session_pid: 20,
                session_proc_start: String::new(),
                session_name: String::new(),
                first_seen_ms: 0,
                last_active_ms: 0,
                kept: true,
                alerted: false,
                note: None,
                asked_stale: false,
                agent: crate::agents::AgentKind::ClaudeCode,
            })
        })
        .unwrap();
        let r = f.w.tick(T0).unwrap();
        assert!(r.killed.is_empty());
        let s = f.state_servers();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].session_proc_start, format_proc_start(START + 20 * SEC));
        assert_eq!(s[0].worktree, f.wt);
    }

    /// Ticks once a minute from `from` (exclusive) to `to` (inclusive) and
    /// returns every kill.
    fn tick_minutes(f: &mut Fx, from: u64, to: u64) -> Vec<KillRecord> {
        let mut t = from;
        let mut killed = Vec::new();
        while t < to {
            t += MIN;
            killed.extend(f.w.tick(t).unwrap().killed);
        }
        killed
    }

    #[test]
    fn child_listener_belongs_to_its_server_and_parent_activity_keeps_it() {
        let mut f = fx();
        // workerd (31), child of the server, listens on an internal port
        // while the browser uses the parent's 3000.
        f.p().add_listener(31, 63055);
        f.p().set_established(30, 3000, 2);
        let r = f.w.tick(T0).unwrap();
        let rows: Vec<(i32, u16)> = r.servers.iter().map(|v| (v.key.pid, v.port)).collect();
        assert_eq!(rows, vec![(30, 3000)], "one row per server tree");
        assert_eq!(r.servers[0].ports, vec![3000, 63055]);
        f.p().remove_proc(20); // the session exits; :3000 stays in use
        assert!(tick_minutes(&mut f, T0, T0 + 25 * MIN).is_empty());
        assert!(f.p().is_alive(30));
        assert!(f.p().is_alive(31));
    }

    #[test]
    fn idle_parent_with_active_child_is_kept() {
        let mut f = fx();
        f.p().add_listener(31, 8787);
        f.p().set_established(31, 8787, 2);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        assert!(tick_minutes(&mut f, T0, T0 + 25 * MIN).is_empty());
        assert!(f.p().is_alive(30));
        assert!(f.p().is_alive(31));
    }

    #[test]
    fn due_probes_follow_the_last_pass_and_store_caches() {
        let mut f = fx();
        assert!(f.w.due_probes(T0).is_empty());
        f.w.tick(T0).unwrap();
        let due = f.w.due_probes(T0);
        assert_eq!(due, vec![(key30(), 3000)]);
        let meta = PageMeta {
            title: Some("t".into()),
            icon: None,
        };
        f.w.store_probes(vec![(due[0], ProbeOutcome::Meta(meta.clone()))], T0);
        assert!(f.w.due_probes(T0 + 1000).is_empty());
        assert_eq!(f.w.page_meta(key30(), 3000), Some(&meta));
    }

    #[test]
    fn probe_does_not_count_as_use() {
        let mut f = fx();
        f.w.ignored_peers = vec![99];
        f.p().set_established(30, 3000, 1);
        f.p().set_established_from(30, 3000, 99, 1);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        let killed = tick_minutes(&mut f, T0, T0 + 25 * MIN);
        assert_eq!(killed.len(), 1, "only the app's own connection: idle");
        // Control: without the ignored peer the same connection is use.
        let mut g = fx();
        g.p().set_established(30, 3000, 1);
        g.p().set_established_from(30, 3000, 99, 1);
        g.w.tick(T0).unwrap();
        g.p().remove_proc(20);
        assert!(tick_minutes(&mut g, T0, T0 + 25 * MIN).is_empty());
    }

    #[test]
    fn activity_on_a_second_port_counts() {
        let mut f = fx();
        f.p().add_listener(30, 8080);
        f.p().set_established(30, 8080, 3);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        assert!(f.w.tick(T0 + 20 * MIN).unwrap().killed.is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn folded_notes_keep_the_cautious_side() {
        let n = |keep, done, r: &str| {
            Some(Note {
                keep,
                done,
                reason: Some(r.into()),
                set_at_ms: 1,
            })
        };
        assert_eq!(merge_notes(None, None), None);
        assert_eq!(merge_notes(None, n(false, true, "b")), n(false, true, "b"));
        assert_eq!(
            merge_notes(n(false, true, "a"), n(true, false, "b")),
            n(true, false, "b")
        );
        assert_eq!(
            merge_notes(n(false, true, "a"), n(false, true, "b")),
            n(false, true, "a")
        );
    }

    #[test]
    fn child_tracked_first_folds_into_its_parent_server() {
        let mut f = fx();
        // The child listens before the parent does: it links on its own.
        f.p().remove_proc(30);
        let web = f.wt.join("apps/web");
        f.p().spawn(
            30,
            20,
            START + 30 * SEC,
            "bun run dev",
            web.to_str().unwrap(),
        );
        f.p().add_listener(31, 63055);
        let r = f.w.tick(T0).unwrap();
        assert_eq!(r.servers.len(), 1);
        assert_eq!(r.servers[0].key.pid, 31);
        // Then the parent listens: the child folds into the parent's tree.
        f.p().add_listener(30, 3000);
        let r = f.w.tick(T0 + MIN).unwrap();
        let rows: Vec<(i32, u16)> = r.servers.iter().map(|v| (v.key.pid, v.port)).collect();
        assert_eq!(rows, vec![(30, 3000)]);
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn exclusion_added_after_linking_forgets_the_server() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        let mut c = f.cfg();
        c.exclude_commands.push("bun".into());
        save_config(f.data.path(), &c).unwrap();
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert!(
            r.servers.is_empty(),
            "an excluded server is no longer tracked"
        );
        assert!(f.state_servers().is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn excluded_port_is_never_tracked_or_killed() {
        let mut f = fx();
        let mut c = f.cfg();
        c.exclude_ports = vec![3000];
        save_config(f.data.path(), &c).unwrap();
        assert!(f.w.tick(T0).unwrap().servers.is_empty());
        // Excluding a port of an already tracked server forgets it too,
        // even when the port belongs to a child of the server.
        c.exclude_ports = vec![];
        save_config(f.data.path(), &c).unwrap();
        f.p().add_listener(31, 8787);
        assert_eq!(f.w.tick(T0).unwrap().servers.len(), 1);
        c.exclude_ports = vec![8787];
        save_config(f.data.path(), &c).unwrap();
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert!(r.killed.is_empty());
        assert!(r.servers.is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn zero_delay_in_the_file_is_clamped_and_spares_an_active_server() {
        let mut f = fx();
        let mut c = f.cfg();
        assert!(c.set("dead_session_idle_s", "0").is_err());
        c.dead_session_idle_s = 0; // written by hand
        save_config(f.data.path(), &c).unwrap();
        f.p().set_established(30, 3000, 2);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        assert!(f.w.tick(T0 + 5_000).unwrap().killed.is_empty());
        assert!(f.p().is_alive(30));
    }

    #[test]
    fn unparsable_config_keeps_the_last_good_one_and_is_reported() {
        let mut f = fx();
        let mut c = f.cfg();
        c.dead_session_idle_s = 3600;
        save_config(f.data.path(), &c).unwrap();
        let r = f.w.tick(T0).unwrap();
        assert_eq!(r.config_error, None);
        fs::write(f.data.path().join("config.toml"), "dead_session_idle_s = [").unwrap();
        f.p().remove_proc(20);
        // Defaults would kill at 20 min; the last good config says 60.
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert!(r.killed.is_empty());
        let e = r.config_error.expect("reported");
        assert!(e.contains("last valid config"), "{e}");
        assert!(f.p().is_alive(30));
        // The bad file is left as the user wrote it.
        assert_eq!(
            fs::read_to_string(f.data.path().join("config.toml")).unwrap(),
            "dead_session_idle_s = ["
        );
    }

    #[test]
    fn unparsable_config_at_start_uses_defaults_and_is_reported() {
        let f = fx();
        fs::write(f.data.path().join("config.toml"), "nope = [").unwrap();
        let mut w = Watcher::new(FakePlatform::new(), f.data.path().to_path_buf())
            .with_discovery(Vec::new)
            .with_codex_home(None);
        let e = w.tick(T0).unwrap().config_error.expect("reported");
        assert!(e.contains("defaults"), "{e}");
        assert_eq!(w.config_error(), Some(e.as_str()));
    }

    #[test]
    fn manual_kill_by_key_needs_the_exact_process() {
        let mut f = fx();
        let other = ServerKey {
            pid: 30,
            start_time: START,
        };
        assert_eq!(f.w.kill(KillTarget::Key(other)).unwrap(), None);
        assert!(f.p().is_alive(30));
        assert!(f.w.kill(KillTarget::Key(key30())).unwrap().is_some());
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn node_installed_claude_blocks_the_link() {
        let mut f = fx();
        let wt = f.wt.to_str().unwrap().to_string();
        f.p().spawn(
            80,
            20,
            START + 80 * SEC,
            "node /opt/homebrew/bin/claude -p x",
            &wt,
        );
        f.p().spawn(81, 80, START + 81 * SEC, "vite", &wt);
        f.p().add_listener(81, 5175);
        assert_eq!(link_pid(&f, 81, 5175), None);
        f.p().add_listener(80, 7400);
        assert_eq!(link_pid(&f, 80, 7400), None);
        let r = f.w.tick(T0).unwrap();
        let ports: Vec<u16> = r.servers.iter().map(|v| v.port).collect();
        assert_eq!(ports, vec![3000]);
    }

    #[test]
    fn kill_tree_spares_a_claude_process_and_its_children() {
        let p = FakePlatform::new();
        p.spawn(30, 1, 100, "srv", "/");
        p.spawn(31, 30, 101, "worker", "/");
        p.spawn(32, 30, 102, "node /usr/local/bin/claude", "/");
        p.spawn(33, 32, 103, "vite", "/");
        p.spawn(34, 30, 104, "/Users/me/.local/bin/claude --resume", "/");
        let key = ServerKey {
            pid: 30,
            start_time: 100,
        };
        assert!(kill_tree(
            &p,
            key,
            Duration::from_millis(100),
            &Config::default()
        ));
        assert!(!p.is_alive(30));
        assert!(!p.is_alive(31));
        for pid in [32, 33, 34] {
            assert!(p.is_alive(pid), "pid {pid} was signalled");
        }
        assert!(p.signals().iter().all(|&(pid, _)| pid == 30 || pid == 31));
    }

    #[test]
    fn manual_kill_by_a_child_port_kills_the_tree() {
        let mut f = fx();
        f.p().add_listener(31, 63055);
        let rec = f.w.kill(KillTarget::Port(63055)).unwrap().expect("killed");
        assert_eq!(rec.port, 3000);
        assert!(!f.p().is_alive(30));
        assert!(!f.p().is_alive(31));
    }

    /// The fixture's server (30) runs a Claude Code session (40, with a
    /// session file when `with_session`) that runs its own server (41,
    /// listening on 4000).
    fn claude_inside_server(f: &Fx, with_session: bool) {
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        f.p().spawn(40, 30, START + 40 * SEC, "claude", web);
        f.p().spawn(41, 40, START + 41 * SEC, "vite", web);
        f.p().add_listener(41, 4000);
        if with_session {
            f.session(40, "sess-inner", &f.wt.join("apps/web"), START + 40 * SEC);
        }
    }

    #[test]
    fn kill_by_inner_port_targets_the_inner_server_only() {
        let mut f = fx();
        claude_inside_server(&f, true);
        let r = f.w.tick(T0).unwrap();
        let mut rows: Vec<(i32, Vec<u16>)> = r
            .servers
            .iter()
            .map(|v| (v.key.pid, v.ports.clone()))
            .collect();
        rows.sort();
        assert_eq!(rows, vec![(30, vec![3000]), (41, vec![4000])]);
        let rec = f.w.kill(KillTarget::Port(4000)).unwrap().expect("killed");
        assert_eq!(rec.port, 4000);
        assert!(!f.p().is_alive(41));
        for pid in [30, 31, 40] {
            assert!(f.p().is_alive(pid), "pid {pid} was killed");
        }
    }

    #[test]
    fn a_busy_server_under_an_inner_claude_keeps_the_outer_server() {
        let mut f = fx();
        // No session file for the inner Claude: a live inner session in the
        // same worktree would already spare the outer server (sharing).
        claude_inside_server(&f, false);
        // The outer server's own port is idle; the inner Claude's server is
        // in use.
        f.p().set_established(41, 4000, 1);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20); // the outer session exits
        assert!(tick_minutes(&mut f, T0, T0 + 25 * MIN).is_empty());
        for pid in [30, 31, 40, 41] {
            assert!(f.p().is_alive(pid), "pid {pid} was killed");
        }
    }

    #[test]
    fn kill_by_a_port_held_under_an_unwatched_claude_matches_nothing() {
        let mut f = fx();
        claude_inside_server(&f, false);
        let r = f.w.tick(T0).unwrap();
        assert_eq!(r.servers.len(), 1);
        assert_eq!(r.servers[0].ports, vec![3000]);
        assert_eq!(f.w.kill(KillTarget::Port(4000)).unwrap(), None);
        assert!(f.p().signals().is_empty());
        for pid in [30, 31, 40, 41] {
            assert!(f.p().is_alive(pid), "pid {pid} was killed");
        }
    }

    #[test]
    fn a_child_row_from_an_old_state_folds_without_a_new_listener() {
        let mut f = fx();
        f.p().add_listener(31, 63055);
        f.w.tick(T0).unwrap();
        // state.json written before server groups: the child has its own row.
        let mut child = f.state_servers()[0].clone();
        child.key = ServerKey {
            pid: 31,
            start_time: START + 31 * SEC,
        };
        child.port = 63055;
        with_state(f.data.path(), |s| s.servers.push(child)).unwrap();
        let r = f.w.tick(T0 + MIN).unwrap();
        let rows: Vec<i32> = r.servers.iter().map(|v| v.key.pid).collect();
        assert_eq!(rows, vec![30]);
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn exclusion_of_a_tree_port_saved_before_the_kill_spares_the_server() {
        let mut f = fx();
        f.p().add_listener(31, 8787);
        f.w.tick(T0).unwrap();
        let server = f.state_servers()[0].clone();
        let old = f.cfg();
        // Saved between the pass that planned the kill and the kill itself.
        let mut c = old.clone();
        c.exclude_ports = vec![8787];
        save_config(f.data.path(), &c).unwrap();
        let (killed, dropped) =
            f.w.execute(vec![(server, KillReason::Manual)], &old, T0)
                .unwrap();
        assert!(killed.is_empty());
        assert!(dropped.contains(&key30()));
        assert!(f.p().signals().is_empty());
        assert!(f.p().is_alive(30));
        assert!(f.p().is_alive(31));
    }

    #[test]
    fn an_excluded_child_of_a_killed_server_is_spared() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        let web = web.to_str().unwrap();
        // The dev server started a database (excluded by default) that
        // runs a worker of its own.
        f.p()
            .spawn(35, 30, START + 35 * SEC, "postgres -D /tmp/db", web);
        f.p()
            .spawn(36, 35, START + 36 * SEC, "postgres: checkpointer", web);
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        let r = f.w.tick(T0 + 20 * MIN).unwrap();
        assert_eq!(r.killed.len(), 1);
        assert!(!f.p().is_alive(30));
        assert!(!f.p().is_alive(31));
        assert!(f.p().is_alive(35));
        assert!(f.p().is_alive(36));
        assert!(
            f.p()
                .signals()
                .iter()
                .all(|&(pid, _)| pid == 30 || pid == 31),
            "{:?}",
            f.p().signals()
        );
    }

    // Observed servers: listeners no agent started.

    const NODE: &str = "/opt/homebrew/bin/node";

    /// `next dev` (50, a child of zsh, no agent) listens on 4000.
    fn observed_fx() -> Fx {
        let f = fx();
        f.p().spawn(50, 10, START + 50 * SEC, "next dev", "/w");
        f.p().set_exe(50, NODE);
        f.p().add_listener(50, 4000);
        f
    }

    fn key50() -> ServerKey {
        ServerKey {
            pid: 50,
            start_time: START + 50 * SEC,
        }
    }

    #[test]
    fn tick_reports_observed_servers_and_feeds_their_history() {
        let mut f = observed_fx();
        // The tracked server's processes have an executable too.
        f.p().set_exe(30, NODE);
        f.p().set_exe(31, NODE);
        f.p().add_listener(31, 8787);
        f.p().set_resources(50, 80 * 1_048_576, 0);
        f.p().set_established(50, 4000, 2);
        let r = f.w.tick(T0).unwrap();
        assert_eq!(r.servers.len(), 1);
        assert_eq!(r.observed.len(), 1, "{:?}", r.observed);
        assert_eq!(r.observed[0].key, key50());
        assert_eq!(r.observed[0].ports, [4000]);
        assert!(f.w.observe(T0 + 1000).unwrap().observed.len() == 1);
        assert!(f.w.sweep(T0 + 2000).unwrap().observed.is_empty());
        let h = f.w.history();
        assert_eq!(h.samples(key50()).len(), 2);
        assert_eq!(h.samples(key50())[0].rss_bytes, 80 * 1_048_576);
        let started: Vec<&EventKind> = h
            .events(key50())
            .iter()
            .map(|e| &e.kind)
            .filter(|k| matches!(k, EventKind::Started { .. }))
            .collect();
        assert_eq!(started, [&EventKind::Started { agent: None }]);
        assert!(h
            .events(key50())
            .iter()
            .any(|e| e.kind == EventKind::BrowserConnected));
        assert!(f.w.due_probes(T0).contains(&(key50(), 4000)));
        // A sweep keeps the history of observed servers.
        f.w.sweep(T0 + 3000).unwrap();
        assert_eq!(f.w.history().samples(key50()).len(), 2);
        assert!(f.state_servers().iter().all(|s| s.key.pid != 50));
    }

    #[test]
    fn observed_server_never_killed_by_tick() {
        let mut f = observed_fx();
        for i in 0..20 {
            let r = f.w.tick(T0 + i * 60 * MIN).unwrap();
            assert_eq!(r.observed.len(), 1);
            assert!(r.alerts.iter().all(|a| a.key != key50()));
        }
        f.w.sweep(T0 + 30 * 60 * MIN).unwrap();
        assert!(f.p().signals().iter().all(|&(pid, _)| pid != 50));
        assert!(f.p().is_alive(50));
    }

    #[test]
    fn stop_observed_kills_the_tree_and_records_a_manual_kill() {
        let mut f = observed_fx();
        f.p().spawn(51, 50, START + 51 * SEC, "node worker", "/w");
        f.p()
            .spawn(52, 50, START + 52 * SEC, "postgres -D /db", "/w");
        let r = f.w.stop_observed(key50()).unwrap().expect("stopped");
        assert_eq!((r.port, r.command.as_str()), (4000, "next dev"));
        assert_eq!(r.reason, KillReason::Manual);
        assert!(!f.p().is_alive(50));
        assert!(!f.p().is_alive(51));
        assert!(f.p().is_alive(52), "excluded descendant spared");
        assert_eq!(f.state_kills(), std::slice::from_ref(&r));
        assert_eq!(f.kill_log(), [r]);
    }

    #[test]
    fn excluded_command_is_marked_excluded() {
        let mut f = fx();
        f.p()
            .spawn(50, 10, START + 50 * SEC, "postgres -D /db", "/w");
        f.p().set_exe(50, "/opt/homebrew/bin/postgres");
        f.p().add_listener(50, 5432);
        let r = f.w.tick(T0).unwrap();
        assert_eq!(r.observed.len(), 1);
        assert!(r.observed[0].excluded);
        assert_eq!(f.w.stop_observed(key50()).unwrap(), None);
        assert!(f.p().signals().is_empty());
        assert!(f.p().is_alive(50));
    }

    #[test]
    fn excluded_observed_servers_are_never_probed() {
        let mut f = fx();
        f.p()
            .spawn(50, 10, START + 50 * SEC, "postgres -D /db", "/w");
        f.p().set_exe(50, "/opt/homebrew/bin/postgres");
        f.p().add_listener(50, 5432);
        f.p().spawn(60, 10, START + 60 * SEC, "node api.js", "/w");
        f.p().set_exe(60, NODE);
        f.p().add_listener(60, 4000);
        save_config(
            f.data.path(),
            &Config {
                exclude_ports: vec![4000],
                ..f.cfg()
            },
        )
        .unwrap();
        for i in 0..5 {
            let now = T0 + i * 10 * MIN;
            let r = f.w.tick(now).unwrap();
            assert_eq!(r.observed.len(), 2);
            assert!(r.observed.iter().all(|o| o.excluded));
            let due = f.w.due_probes(now);
            assert!(
                due.iter().all(|(k, _)| k.pid != 50 && k.pid != 60),
                "{due:?}"
            );
            f.w.store_probes(
                due.into_iter()
                    .map(|k| (k, ProbeOutcome::Nothing))
                    .collect(),
                now,
            );
            f.w.sweep(now + 1).unwrap();
            assert!(f.w.due_probes(now + 1).iter().all(|(k, _)| k.pid != 50));
        }
        // Their history is kept all the same.
        assert!(!f.w.history().events(key50()).is_empty());
    }

    #[test]
    fn stop_observed_checks_start_time() {
        let mut f = observed_fx();
        f.w.tick(T0).unwrap();
        f.p().remove_proc(50);
        f.p().spawn(50, 10, START + 99 * SEC, "next dev", "/w");
        f.p().set_exe(50, NODE);
        f.p().add_listener(50, 4000);
        assert_eq!(f.w.stop_observed(key50()).unwrap(), None);
        assert!(f.p().signals().is_empty());
        assert!(f.p().is_alive(50));
    }

    #[test]
    fn stop_observed_refuses_tracked_and_unknown_servers() {
        let mut f = observed_fx();
        f.p().set_exe(30, NODE);
        f.w.tick(T0).unwrap();
        assert_eq!(f.w.stop_observed(key30()).unwrap(), None);
        let gone = ServerKey {
            pid: 77,
            start_time: 1,
        };
        assert_eq!(f.w.stop_observed(gone).unwrap(), None);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn kill_still_refuses_an_observed_server() {
        let mut f = observed_fx();
        f.w.tick(T0).unwrap();
        assert_eq!(f.w.kill(KillTarget::Port(4000)).unwrap(), None);
        assert_eq!(f.w.kill(KillTarget::Pid(50)).unwrap(), None);
        assert_eq!(f.w.kill(KillTarget::Key(key50())).unwrap(), None);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn observed_scan_cost_is_bounded() {
        let mut f = fx();
        for i in 0..200 {
            let pid = 1000 + i;
            f.p()
                .spawn(pid, 10, START + pid as u64 * SEC, "node app.js", "/w");
            f.p().set_exe(pid, NODE);
            f.p().add_listener(pid, 10_000 + i as u16);
        }
        // The tracked server is never looked up: it is in a tracked tree.
        f.w.tick(T0).unwrap();
        let first = f.p().exe_calls();
        assert_eq!(first, 200);
        let r = f.w.tick(T0 + 1000).unwrap();
        assert_eq!(r.observed.len(), 200);
        assert_eq!(f.p().exe_calls(), first, "no new process, no new read");
        // A reused pid is a new process: read once more.
        f.p().remove_proc(1000);
        f.p()
            .spawn(1000, 10, START + 5000 * SEC, "node app.js", "/w");
        f.p().set_exe(1000, NODE);
        f.p().add_listener(1000, 10_000);
        f.w.tick(T0 + 2000).unwrap();
        f.w.tick(T0 + 3000).unwrap();
        assert_eq!(f.p().exe_calls(), first + 1);
    }

    #[test]
    fn second_watch_lock_names_owner() {
        let d = tempfile::tempdir().unwrap();
        let a = acquire_watch_lock(d.path(), "Balance ton port.app").unwrap();
        let e = acquire_watch_lock(d.path(), "btp watch").unwrap_err();
        assert_eq!(e.to_string(), "Already watched by Balance ton port.app");
        drop(a);
        let _b = acquire_watch_lock(d.path(), "btp watch").unwrap();
        let e = acquire_watch_lock(d.path(), "x").unwrap_err();
        assert_eq!(e.to_string(), "Already watched by btp watch");
    }
}
