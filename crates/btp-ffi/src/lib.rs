//! UniFFI bindings for the macOS app: one `Core` object that owns the watcher
//! for the app's lifetime. Every method may block on disk and process I/O, so
//! the app calls them off the main thread; `Core` is `Send + Sync`.

uniffi::setup_scaffolding!();

use btp_core::config::{data_dir, read_config, save_config, Config};
use btp_core::display::{format_idle, server_state, started_label};
use btp_core::labels::tool_label;
use btp_core::model::{KillReason, KillTarget, ServerKey};
use btp_core::platform::MacOs;
use btp_core::sessions::default_config_dirs;
use btp_core::state::{with_state, KillRecord};
use btp_core::watcher::{acquire_watch_lock, now_ms, ServerView, TickReport, WatchLock, Watcher};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Owner written into `watch.lock` while the app holds it.
pub const APP_OWNER: &str = "Balance ton port.app";

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

/// One tracked server as the menu shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ServerViewFfi {
    pub pid: i32,
    pub start_time: u64,
    pub port: u16,
    pub command: String,
    /// A readable name for the server (`Expo (Metro)`, `Vite`, `Node x.js`).
    pub tool_label: String,
    /// Full worktree path.
    pub worktree: String,
    pub worktree_name: String,
    /// `<repo> › <worktree>`, the repo dir name, or a `~`-shortened path.
    pub location_label: String,
    /// The Claude Code session's display name.
    pub session_name: String,
    /// When the server process started, Unix milliseconds.
    pub started_ms: u64,
    /// `started 4 min ago`, as of the pass that produced this view.
    pub started_label: String,
    pub session_alive: bool,
    pub idle_ms: u64,
    pub kill_in_ms: Option<u64>,
    /// `active`, `idle` or `session dead`, as `btp list` prints it.
    pub state: String,
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

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TickReportFfi {
    pub servers: Vec<ServerViewFfi>,
    /// Servers due an alert, reported on every pass until `mark_alerted`.
    pub alerts: Vec<ServerViewFfi>,
    pub killed: Vec<KillRecordFfi>,
    /// Why `config.toml` could not be used, if it could not (the pass ran on
    /// the last valid config, or the defaults).
    pub config_error: Option<String>,
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

impl ServerViewFfi {
    /// `v` as the menu shows it at `now_ms`.
    pub fn at(v: &ServerView, now_ms: u64) -> Self {
        ServerViewFfi {
            pid: v.key.pid,
            start_time: v.key.start_time,
            port: v.port,
            command: v.command.clone(),
            tool_label: v.tool_label.clone(),
            worktree: v.worktree.display().to_string(),
            worktree_name: v.worktree_name.clone(),
            location_label: v.location_label.clone(),
            session_name: v.session_name.clone(),
            started_ms: v.started_ms,
            started_label: started_label(v.started_ms, now_ms),
            session_alive: v.session_alive,
            idle_ms: v.idle_ms,
            kill_in_ms: v.kill_in_ms,
            state: server_state(v).to_string(),
        }
    }
}

impl From<&ServerView> for ServerViewFfi {
    fn from(v: &ServerView) -> Self {
        ServerViewFfi::at(v, now_ms())
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

impl From<&TickReport> for TickReportFfi {
    fn from(t: &TickReport) -> Self {
        TickReportFfi {
            servers: t.servers.iter().map(Into::into).collect(),
            alerts: t.alerts.iter().map(Into::into).collect(),
            killed: t.killed.iter().map(Into::into).collect(),
            config_error: t.config_error.clone(),
        }
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
}

/// A poisoned mutex only means an earlier call panicked; the watcher holds no
/// in-memory state worth distrusting (everything lives in `state.json`).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Core {
    fn at(dir: PathBuf, discover: fn() -> Vec<PathBuf>) -> Self {
        Core {
            watcher: Mutex::new(Watcher::new(MacOs, dir.clone()).with_discovery(discover)),
            dir,
            discover,
            watch_lock: Mutex::new(None),
        }
    }

    fn kill(&self, target: KillTarget) -> Result<Option<KillRecordFfi>, BtpError> {
        Ok(lock(&self.watcher).kill(target)?.as_ref().map(Into::into))
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
        let report = lock(&self.watcher).tick(now_ms())?;
        Ok((&report).into())
    }

    /// A read-only watch pass for when another process holds the watch lock:
    /// links new servers and records port activity, but kills nothing and
    /// marks no alert. `killed` lists the kills the watcher would make;
    /// ignore it. Blocking; never call it on the main thread.
    pub fn observe(&self) -> Result<TickReportFfi, BtpError> {
        let report = lock(&self.watcher).observe(now_ms())?;
        Ok((&report).into())
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
    /// minutes >= 60 s, grace and scan >= 1 s).
    pub fn set_config(&self, c: ConfigFfi) -> Result<(), BtpError> {
        let c: Config = c.into();
        c.validate().map_err(|message| BtpError::Io { message })?;
        Ok(save_config(&self.dir, &c)?)
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

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn core_is_send_and_sync() {
        assert_send_sync::<Core>();
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
            started_ms: 0,
            session_alive: false,
            idle_ms: 0,
            kill_in_ms: Some(1_000),
        };
        let f = ServerViewFfi::at(&v, 4 * 60_000);
        assert_eq!((f.pid, f.start_time, f.port), (42, 7, 5173));
        assert_eq!(f.kill_in_ms, Some(1_000));
        assert_eq!(f.state, "session dead");
        assert_eq!(f.tool_label, "Vite");
        assert_eq!(f.worktree, "/repo/app");
        assert_eq!(f.location_label, "app");
        assert_eq!(f.session_name, "app-1d");
        assert_eq!(f.started_label, "started 4 min ago");
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
        c.alert_after_s = 60;
        c.claude_config_dirs = vec!["/tmp/claude-x".into()];
        core.set_config(c.clone()).unwrap();
        assert_eq!(core.config().unwrap(), c);
        assert_eq!(read_config(d.path()).unwrap().alert_after_s, 60);
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
        assert!(core.observe().unwrap().servers.is_empty());
    }

    #[test]
    fn recent_kills_empty_on_fresh_dir() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf(), Vec::new);
        assert!(core.recent_kills().unwrap().is_empty());
    }

    #[test]
    fn observe_works_without_the_watch_lock() {
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
        assert!(report.servers.is_empty());
        assert!(report.alerts.is_empty());
        assert!(report.killed.is_empty());
    }

    #[test]
    fn acquire_watch_is_exclusive_and_held_until_drop() {
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
