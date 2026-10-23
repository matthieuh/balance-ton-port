//! The watcher ties everything together: it links new listeners to the
//! Claude Code session that started them, observes tracked servers, asks
//! `decide` what to do, and executes it (kill, alert, forget).
//!
//! Safety: only a server linked at first sight to a live Claude Code session
//! is ever tracked, and only tracked servers are ever signalled, each signal
//! guarded by a fresh start-time check (a pid can be reused at any moment).

use crate::agents::claude::ClaudeAgent;
use crate::agents::codex::CodexAgent;
use crate::agents::{is_agent_cli, session_idle_ms, Agent, AgentKind, AgentSession};
use crate::config::{effective_claude_dirs_with, read_config, Config};
use crate::decide::{decide, touch};
use crate::labels::{location_label, session_display_name, tool_label};
use crate::model::{Action, KillReason, KillTarget, Note, Observation, ServerKey, TrackedServer};
use crate::platform::{ancestry, format_proc_start, Listener, Platform, ProcInfo};
use crate::sessions::default_config_dirs;
use crate::state::{append_kill_log, with_state, KillRecord, State};
use crate::worktree::worktree_root;
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

/// Links a new listener to the live Claude Code session it descends from.
///
/// `chain` is the listener's ancestry (`chain[0]` is the listener's own
/// process). The session is the first ancestor that matches a session file
/// by pid and by start time (`procStart`), so a stale file whose pid now
/// belongs to another process never links. `None` when the listener is a
/// session itself, when no live session is found, when the server or any
/// process between it and the session is a Claude Code CLI (watched or not)
/// or matches an excluded command, when the port is excluded, or when
/// the server's working directory cannot be read.
pub fn link(
    listener: &Listener,
    chain: &[ProcInfo],
    sessions: &[AgentSession],
    cfg: &Config,
    now_ms: u64,
) -> Option<TrackedServer> {
    let server = chain.first().filter(|p| p.pid == listener.pid)?;
    if sessions.iter().any(|s| s.pid == listener.pid) || cfg.exclude_ports.contains(&listener.port)
    {
        return None;
    }
    let (depth, session) = chain.iter().enumerate().skip(1).find_map(|(i, p)| {
        let started = format_proc_start(p.start_time);
        sessions
            .iter()
            .find(|s| s.pid == p.pid && s.proc_start == started)
            .map(|s| (i, s))
    })?;
    if chain[..depth]
        .iter()
        .any(|p| is_agent_cli(&p.command).is_some() || is_excluded(&p.command, cfg))
    {
        return None;
    }
    let cwd = server.cwd.as_deref()?;
    Some(TrackedServer {
        key: ServerKey {
            pid: server.pid,
            start_time: server.start_time,
        },
        port: listener.port,
        command: server.command.clone(),
        worktree: worktree_root(cwd),
        session_id: session.conversation_id.clone(),
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

/// `true` when `pid` is the very process that started at `start_time`.
fn same_process(p: &dyn Platform, pid: i32, start_time: u64) -> bool {
    p.proc_info(pid).is_some_and(|i| i.start_time == start_time)
}

/// A tracked server's process tree as seen in one pass: the server and its
/// descendants, and every port any of them listens on. Empty when the
/// server's process is gone.
#[derive(Debug, Clone, Default)]
struct Tree {
    pids: Vec<i32>,
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
    let mut pids = vec![s.key.pid];
    pids.extend(
        own_descendants(p, s.key.pid, &|_| false)
            .into_iter()
            .map(|(pid, _)| pid),
    );
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

/// Signals `pid` only if it is still the process we snapshotted, and never
/// pid <= 1 or ourselves.
fn signal_checked(p: &dyn Platform, pid: i32, start_time: u64, sig: i32) -> bool {
    pid > 1
        && pid as u32 != std::process::id()
        && same_process(p, pid, start_time)
        && p.signal(pid, sig)
}

/// SIGTERM to the server and its descendants (except a Claude Code process
/// or a command `cfg` excludes, each with its subtree), then SIGKILL to whatever
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
    /// The last config read successfully: kept while `config.toml` is broken.
    last_good: Option<Config>,
    /// Why the last read of `config.toml` failed, cleared once it parses.
    config_error: Option<String>,
}

impl<P: Platform> Watcher<P> {
    pub fn new(platform: P, data_dir: PathBuf) -> Self {
        Watcher {
            platform,
            data_dir,
            discover: default_config_dirs,
            last_good: None,
            config_error: None,
        }
    }

    /// Replaces the automatic discovery of Claude config dirs (tests pass
    /// `Vec::new` so only the configured extras are read).
    pub fn with_discovery(mut self, discover: fn() -> Vec<PathBuf>) -> Self {
        self.discover = discover;
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
        vec![
            Box::new(ClaudeAgent {
                dirs: effective_claude_dirs_with((self.discover)(), cfg),
            }),
            Box::new(CodexAgent {
                default_home: dirs::home_dir().map(|h| h.join(".codex")),
            }),
        ]
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
            let Some(new) = link(l, &chain, &sessions, &scan.cfg, now_ms) else {
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
                .map(|&(pid, port)| self.platform.established(pid, port))
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
        let (mut report, to_kill) = with_state(&self.data_dir, |state| {
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
            (report, to_kill)
        })?;

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
    use crate::sessions::read_sessions;
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
        let w = Watcher::new(p, data.path().to_path_buf()).with_discovery(Vec::new);
        Fx {
            data,
            claude,
            repo,
            wt,
            w,
        }
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
        link(&Listener { pid, port }, &chain, &sessions(f), &f.cfg(), T0)
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
        assert_eq!(link(&l, &chain, &sessions(&f), &f.cfg(), T0), None);

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
        let mut w =
            Watcher::new(FakePlatform::new(), f.data.path().to_path_buf()).with_discovery(Vec::new);
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
