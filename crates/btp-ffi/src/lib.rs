//! UniFFI bindings for the macOS app: one `Core` object that owns the watcher
//! for the app's lifetime. Every method may block on disk and process I/O, so
//! the app calls them off the main thread; `Core` is `Send + Sync`.

uniffi::setup_scaffolding!();

use btp_core::config::{data_dir, load_config, save_config, Config};
use btp_core::display::{format_idle, server_state};
use btp_core::model::{KillReason, KillTarget, ServerKey};
use btp_core::platform::MacOs;
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
    pub worktree_name: String,
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
    /// Servers to notify about now (each reported once).
    pub alerts: Vec<ServerViewFfi>,
    pub killed: Vec<KillRecordFfi>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ConfigFfi {
    pub dead_session_idle_s: u64,
    pub alert_after_s: u64,
    pub kill_grace_s: u64,
    pub scan_interval_s: u64,
    pub claude_config_dirs: Vec<String>,
    pub exclude_commands: Vec<String>,
}

/// The reason as the app shows it.
pub fn reason_label(r: &KillReason) -> String {
    match r {
        KillReason::WorktreeDeleted => "worktree deleted".to_string(),
        KillReason::DeadSessionIdle { idle_ms } => {
            format!("dead session + idle {}", format_idle(*idle_ms))
        }
        KillReason::Manual => "manual".to_string(),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

impl From<&ServerView> for ServerViewFfi {
    fn from(v: &ServerView) -> Self {
        ServerViewFfi {
            pid: v.key.pid,
            start_time: v.key.start_time,
            port: v.port,
            command: v.command.clone(),
            worktree_name: v.worktree_name.clone(),
            session_alive: v.session_alive,
            idle_ms: v.idle_ms,
            kill_in_ms: v.kill_in_ms,
            state: server_state(v).to_string(),
        }
    }
}

impl From<&KillRecord> for KillRecordFfi {
    fn from(r: &KillRecord) -> Self {
        KillRecordFfi {
            port: r.port,
            command: r.command.clone(),
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
        }
    }
}

#[derive(uniffi::Object)]
pub struct Core {
    dir: PathBuf,
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
    fn at(dir: PathBuf) -> Self {
        Core {
            watcher: Mutex::new(Watcher::new(MacOs, dir.clone())),
            dir,
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
        Core::at(data_dir())
    }

    /// One watch pass (link, observe, kill, alert). Blocking; never call it
    /// on the main thread.
    pub fn tick(&self) -> Result<TickReportFfi, BtpError> {
        let report = lock(&self.watcher).tick(now_ms())?;
        Ok((&report).into())
    }

    /// Kills the tracked server on `port`. `nil` when nothing tracked matches.
    pub fn kill_port(&self, port: u16) -> Result<Option<KillRecordFfi>, BtpError> {
        self.kill(KillTarget::Port(port))
    }

    /// Kills the tracked server with `pid`. `nil` when nothing tracked matches.
    pub fn kill_pid(&self, pid: i32) -> Result<Option<KillRecordFfi>, BtpError> {
        self.kill(KillTarget::Pid(pid))
    }

    /// No more alerts for this server until it restarts.
    pub fn keep(&self, pid: i32, start_time: u64) -> Result<(), BtpError> {
        Ok(lock(&self.watcher).keep(ServerKey { pid, start_time })?)
    }

    pub fn config(&self) -> ConfigFfi {
        load_config(&self.dir).into()
    }

    pub fn set_config(&self, c: ConfigFfi) -> Result<(), BtpError> {
        Ok(save_config(&self.dir, &c.into())?)
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
            command: "vite".into(),
            worktree_name: "app".into(),
            session_alive: false,
            idle_ms: 0,
            kill_in_ms: Some(1_000),
        };
        let f = ServerViewFfi::from(&v);
        assert_eq!((f.pid, f.start_time, f.port), (42, 7, 5173));
        assert_eq!(f.kill_in_ms, Some(1_000));
        assert_eq!(f.state, "session dead");
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
        assert_eq!(f.reason, "worktree deleted");
        assert_eq!(f.at_ms, 9);
    }

    #[test]
    fn config_roundtrips_through_disk() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf());
        let mut c = core.config();
        assert_eq!(c.dead_session_idle_s, 1200);
        c.alert_after_s = 60;
        c.claude_config_dirs = vec!["/tmp/claude-x".into()];
        core.set_config(c.clone()).unwrap();
        assert_eq!(core.config(), c);
        assert_eq!(load_config(d.path()).alert_after_s, 60);
    }

    #[test]
    fn recent_kills_empty_on_fresh_dir() {
        let d = tempfile::tempdir().unwrap();
        let core = Core::at(d.path().to_path_buf());
        assert!(core.recent_kills().unwrap().is_empty());
    }

    #[test]
    fn acquire_watch_is_exclusive_and_held_until_drop() {
        let d = tempfile::tempdir().unwrap();
        let a = Core::at(d.path().to_path_buf());
        let b = Core::at(d.path().to_path_buf());
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
