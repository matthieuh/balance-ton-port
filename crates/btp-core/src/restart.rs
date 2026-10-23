//! Restart: stop a server and start it again with its original command,
//! folder and environment. The plan is read from the live process at
//! restart time; the environment is handed to the new process and never
//! stored, logged or printed. A tracked server keeps its row (session, note,
//! Keep, Postpone) through a `PendingRestart` that links the new listener.

use crate::config::Config;
use crate::model::{KillReason, ServerKey, TrackedServer};
use crate::observed::ObservedServer;
use crate::platform::{Listener, Platform, ProcInfo};
use crate::state::with_state;
use crate::watcher::{execute_kills, stop_observed_tree};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A pending restart is kept while its process lives, at most this long.
pub const RESTART_PENDING_MAX_MS: u64 = 86_400_000;
/// Restart logs kept in `<data dir>/logs/` (the newest ones).
pub const MAX_RESTART_LOGS: usize = 20;

/// What a restart runs. No `Serialize` and no derived `Debug`: the
/// environment must never reach a file, a log or an error message.
pub struct RestartPlan {
    pub cwd: PathBuf,
    /// `argv[0]` resolved to a path.
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub port: u16,
}

impl fmt::Debug for RestartPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestartPlan")
            .field("cwd", &self.cwd)
            .field("argv", &self.argv)
            .field("env", &format_args!("<{} entries>", self.env.len()))
            .field("port", &self.port)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartBlocked {
    /// The server is not running (or is not a server this can restart).
    Gone,
    /// Its working directory no longer exists.
    CwdGone,
    /// Its environment cannot be read (another user's process, say).
    EnvUnreadable,
    /// Its command line cannot be read or its program found.
    ArgvUnreadable,
    /// The port was still taken 5 s after the stop (or the stop was
    /// refused): nothing was started.
    PortStillTaken,
    /// An exclusion saved meanwhile matches it: it was neither stopped nor
    /// started.
    Excluded,
    /// The new process could not be started (the server is stopped).
    SpawnFailed,
}

/// A restarted process waiting for its first listener, so that listener
/// links to `owner` (a copy of the stopped server's row) instead of being
/// linked from scratch. `owner` is `None` for an observed server: its new
/// process stays observed. Kept while the process lives, at most a day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    pub child_pid: i32,
    pub child_start: u64,
    pub from: ServerKey,
    pub owner: Option<TrackedServer>,
    pub at_ms: u64,
}

/// Starts a restart's process. Tests use a fake one: they never run a
/// program.
pub trait Spawner: Send + Sync {
    /// Starts `plan`, appending its stdout and stderr to `log`; returns the
    /// new process's pid and its start time as `p` reads it (microseconds
    /// since the epoch). An error means nothing is left running.
    fn spawn(&self, plan: &RestartPlan, log: &Path, p: &dyn Platform) -> io::Result<(i32, u64)>;
}

/// The real spawner: the plan's environment only, its folder, stdin from
/// `/dev/null`, output appended to the log, in a process group of its own
/// so it outlives whoever asked for the restart.
pub struct CommandSpawner;

impl Spawner for CommandSpawner {
    fn spawn(&self, plan: &RestartPlan, log: &Path, p: &dyn Platform) -> io::Result<(i32, u64)> {
        use std::os::unix::process::CommandExt;
        let (program, args) = plan
            .argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty command line"))?;
        let out = open_log(log)?;
        let err = out.try_clone()?;
        let mut child = Command::new(program)
            .args(args)
            .env_clear()
            .envs(plan.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&plan.cwd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .process_group(0)
            .spawn()?;
        let pid = child.id() as i32;
        let Some(start) = p.proc_info(pid).map(|i| i.start_time) else {
            // Not readable: never report a failure with it still running.
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other("the new process cannot be read"));
        };
        // Reaped when it exits, so a stopped server leaves no zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok((pid, start))
    }
}

/// Opens a restart log for appending, readable by the user only.
fn open_log(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Creates the log of a restart, `<data dir>/logs/<port>-<unix ms>.log`
/// (mode 0600), and keeps only the `MAX_RESTART_LOGS` newest `*.log` files
/// of that folder. Returns its path.
pub fn create_log(data_dir: &Path, port: u16, now_ms: u64) -> io::Result<PathBuf> {
    let dir = data_dir.join("logs");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{port}-{now_ms}.log"));
    open_log(&path)?;
    prune_logs(&dir, MAX_RESTART_LOGS);
    Ok(path)
}

/// Deletes all but the `keep` newest `*.log` files of `dir`, newest by the
/// time in their `<port>-<unix ms>` name, else by modification time.
/// Nothing else is touched.
pub fn prune_logs(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(u64, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "log"))
        .map(|p| (log_time(&p), p))
        .collect();
    logs.sort_by(|a, b| b.cmp(a));
    for (_, old) in logs.into_iter().skip(keep) {
        let _ = fs::remove_file(old);
    }
}

fn log_time(path: &Path) -> u64 {
    let named = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit_once('-'))
        .and_then(|(_, ms)| ms.parse().ok());
    named.unwrap_or_else(|| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64)
    })
}

/// The live process `key`, if it is still that process.
fn live(p: &dyn Platform, key: ServerKey) -> Option<ProcInfo> {
    p.proc_info(key.pid)
        .filter(|i| i.start_time == key.start_time)
}

/// The working directory of `info`, when it still exists.
fn existing_cwd(info: &ProcInfo) -> Option<PathBuf> {
    info.cwd.clone().filter(|c| c.is_dir())
}

/// Reads what restarting `key` would run, from the live process: its folder
/// (which must exist), its argv, its environment. An `argv[0]` without a
/// `/` is looked up in the `PATH` of that environment (first executable
/// match); a relative path with a `/` is taken from the folder.
pub fn plan_restart(
    p: &dyn Platform,
    key: ServerKey,
    port: u16,
) -> Result<RestartPlan, RestartBlocked> {
    let info = live(p, key).ok_or(RestartBlocked::Gone)?;
    let cwd = existing_cwd(&info).ok_or(RestartBlocked::CwdGone)?;
    let env = p.proc_env(key.pid).ok_or(RestartBlocked::EnvUnreadable)?;
    let mut argv = p
        .proc_argv(key.pid)
        .filter(|a| a.first().is_some_and(|a0| !a0.is_empty()))
        .ok_or(RestartBlocked::ArgvUnreadable)?;
    let program = resolve_program(&argv[0], &cwd, &env).ok_or(RestartBlocked::ArgvUnreadable)?;
    argv[0] = program.to_string_lossy().into_owned();
    Ok(RestartPlan {
        cwd,
        argv,
        env,
        port,
    })
}

/// Cheap check for the Restart button: the process is still `key`, its
/// folder exists and its environment can be read.
pub fn can_restart(p: &dyn Platform, key: ServerKey) -> bool {
    live(p, key).is_some_and(|i| existing_cwd(&i).is_some()) && p.proc_env(key.pid).is_some()
}

/// `can_restart` for every pass: whether a process's environment can be
/// read is remembered per process (it never changes), its folder is checked
/// every time.
#[derive(Debug, Default)]
pub struct RestartCheck {
    env_readable: HashMap<ServerKey, bool>,
}

impl RestartCheck {
    pub fn can_restart(&mut self, p: &dyn Platform, key: ServerKey) -> bool {
        if !live(p, key).is_some_and(|i| existing_cwd(&i).is_some()) {
            return false;
        }
        *self
            .env_readable
            .entry(key)
            .or_insert_with(|| p.proc_env(key.pid).is_some())
    }

    pub fn retain(&mut self, live: &std::collections::HashSet<ServerKey>) {
        self.env_readable.retain(|k, _| live.contains(k));
    }
}

/// What a restart stops: a tracked server's row (handed to its new
/// process) or an observed server.
pub enum RestartTarget {
    Tracked(TrackedServer),
    Observed(ObservedServer),
}

/// A restart checked and planned by `Watcher::prepare_restart`: the stop,
/// the port wait and the start, which can take seconds, run without the
/// watcher.
pub struct RestartJob {
    pub(crate) key: ServerKey,
    pub(crate) target: RestartTarget,
    pub(crate) plan: RestartPlan,
    pub(crate) cfg: Config,
    pub(crate) data_dir: PathBuf,
    pub(crate) spawner: Arc<dyn Spawner>,
    /// How long the port is waited for, and how often it is looked at.
    pub(crate) wait: (Duration, Duration),
    pub(crate) now_ms: u64,
}

impl RestartJob {
    /// Stops the server (a manual kill, start times re-checked, an
    /// exclusion saved meanwhile honoured), waits for its port, starts the
    /// plan and records the pending restart. `p` is the platform it was
    /// prepared on.
    pub fn run(self, p: &dyn Platform) -> io::Result<Result<(), RestartBlocked>> {
        let RestartJob {
            key,
            target,
            plan,
            cfg,
            data_dir,
            spawner,
            wait: (limit, interval),
            now_ms,
        } = self;
        // `dropped`: no longer tracked although not killed, which for a
        // process still running means an exclusion saved meanwhile matched.
        let (stopped, dropped, owner) = match target {
            RestartTarget::Tracked(s) => {
                let (killed, gone) = execute_kills(
                    p,
                    &data_dir,
                    vec![(s.clone(), KillReason::Manual)],
                    &cfg,
                    now_ms,
                )?;
                let dropped = killed.is_empty() && gone.contains(&key);
                (!killed.is_empty(), dropped, Some(s))
            }
            RestartTarget::Observed(o) => {
                let stopped = stop_observed_tree(p, &data_dir, &o, &cfg, now_ms)?.is_some();
                (stopped, false, None)
            }
        };
        if !stopped && live(p, key).is_some() {
            return Ok(Err(if dropped {
                RestartBlocked::Excluded
            } else {
                // A refused signal leaves it running: no point waiting.
                RestartBlocked::PortStillTaken
            }));
        }
        // Only the recorded port is waited for, not every port of the tree.
        if !wait_port_free(p, plan.port, limit, interval) {
            return Ok(Err(RestartBlocked::PortStillTaken));
        }
        let spawned =
            create_log(&data_dir, plan.port, now_ms).and_then(|log| spawner.spawn(&plan, &log, p));
        let Ok((child_pid, child_start)) = spawned else {
            return Ok(Err(RestartBlocked::SpawnFailed));
        };
        with_state(&data_dir, |state| {
            state.pending_restarts.push(PendingRestart {
                child_pid,
                child_start,
                from: key,
                owner,
                at_ms: now_ms,
            })
        })?;
        Ok(Ok(()))
    }
}

fn resolve_program(argv0: &str, cwd: &Path, env: &[(String, String)]) -> Option<PathBuf> {
    let path = Path::new(argv0);
    if argv0.contains('/') {
        return Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            cwd.join(path)
        });
    }
    let search = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v)?;
    search
        .split(':')
        .filter(|d| !d.is_empty())
        .map(|d| cwd.join(d).join(argv0))
        .find(|c| is_executable(c))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Polls `listeners` until nobody listens on `port` (`true`) or `limit`
/// passed (`false`).
pub fn wait_port_free(p: &dyn Platform, port: u16, limit: Duration, interval: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !p.listeners().iter().any(|l| l.port == port) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(interval.min(deadline - now));
    }
}

/// Whether a pending restart is still worth keeping: its process is alive
/// (same pid and start time) and it is less than a day old.
pub fn pending_holds(p: &dyn Platform, r: &PendingRestart, now_ms: u64) -> bool {
    now_ms.saturating_sub(r.at_ms) < RESTART_PENDING_MAX_MS
        && p.proc_info(r.child_pid)
            .is_some_and(|i| i.start_time == r.child_start)
}

/// The pending restart whose process is in `chain` (a listener's ancestry,
/// the listener itself included), matched by pid and start time, with that
/// process.
pub fn pending_for<'a>(
    pending: &[PendingRestart],
    chain: &'a [ProcInfo],
) -> Option<(usize, &'a ProcInfo)> {
    pending.iter().enumerate().find_map(|(i, r)| {
        chain
            .iter()
            .find(|p| p.pid == r.child_pid && p.start_time == r.child_start)
            .map(|p| (i, p))
    })
}

/// The stopped server's row carried over to its restarted process `server`
/// once `listener` (that process or one of its descendants) listens: same
/// session, note, Keep and Postpone; the restarted process as key and
/// command (its tree holds every port it opens), the listener's port; seen
/// and active from now; a fresh alert.
pub fn relinked(
    owner: &TrackedServer,
    from: ServerKey,
    server: &ProcInfo,
    listener: &Listener,
    now_ms: u64,
) -> TrackedServer {
    TrackedServer {
        key: ServerKey {
            pid: server.pid,
            start_time: server.start_time,
        },
        port: listener.port,
        command: server.command.clone(),
        first_seen_ms: now_ms,
        last_active_ms: now_ms,
        alerted: false,
        restarted_from: Some(from),
        ..owner.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FakePlatform;
    use std::fs;

    const START: u64 = 1_790_000_000_000_000;

    fn key(pid: i32, start: u64) -> ServerKey {
        ServerKey {
            pid,
            start_time: start,
        }
    }

    fn executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn plan_blocked_when_cwd_gone() {
        let dir = tempfile::tempdir().unwrap();
        let p = FakePlatform::new();
        let gone = dir.path().join("gone");
        p.spawn(30, 1, START, "/bin/node server.js", gone.to_str().unwrap());
        p.set_env(30, &[("PATH", "/usr/bin")]);
        assert_eq!(
            plan_restart(&p, key(30, START), 3000).unwrap_err(),
            RestartBlocked::CwdGone
        );
        assert!(!can_restart(&p, key(30, START)));
    }

    #[test]
    fn plan_blocked_when_env_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let p = FakePlatform::new();
        p.spawn(30, 1, START, "/bin/node x.js", dir.path().to_str().unwrap());
        assert_eq!(
            plan_restart(&p, key(30, START), 3000).unwrap_err(),
            RestartBlocked::EnvUnreadable
        );
        assert!(!can_restart(&p, key(30, START)));
        p.set_env(30, &[("PATH", "/usr/bin")]);
        assert!(can_restart(&p, key(30, START)));
    }

    #[test]
    fn restart_check_reads_the_env_once_and_the_folder_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("app");
        fs::create_dir_all(&cwd).unwrap();
        let p = FakePlatform::new();
        p.spawn(30, 1, START, "/bin/node x.js", cwd.to_str().unwrap());
        p.set_env(30, &[("PATH", "/usr/bin")]);
        let k = key(30, START);
        let mut c = RestartCheck::default();
        assert!(c.can_restart(&p, k));
        // The environment of a process never changes: not read again.
        p.clear_env(30);
        assert!(c.can_restart(&p, k));
        // Its folder is checked on every call.
        fs::remove_dir(&cwd).unwrap();
        assert!(!c.can_restart(&p, k));
        fs::create_dir_all(&cwd).unwrap();
        // Forgotten once gone from the live servers: read again.
        c.retain(&std::collections::HashSet::new());
        assert!(!c.can_restart(&p, k));
    }

    #[test]
    fn plan_blocked_for_reused_pid() {
        let dir = tempfile::tempdir().unwrap();
        let p = FakePlatform::new();
        p.spawn(30, 1, START, "/bin/node x.js", dir.path().to_str().unwrap());
        p.set_env(30, &[("PATH", "/usr/bin")]);
        let old = key(30, START - 1);
        assert_eq!(
            plan_restart(&p, old, 3000).unwrap_err(),
            RestartBlocked::Gone
        );
        assert!(!can_restart(&p, old));
    }

    #[test]
    fn plan_resolves_argv0_with_the_process_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("x/bin");
        // An earlier PATH entry without it, a non-executable one, then it.
        fs::create_dir_all(dir.path().join("empty")).unwrap();
        fs::create_dir_all(dir.path().join("plain")).unwrap();
        fs::write(dir.path().join("plain/npm"), "").unwrap();
        executable(&bin.join("npm"));
        let path = format!(
            "{}:{}:{}:/usr/bin",
            dir.path().join("empty").display(),
            dir.path().join("plain").display(),
            bin.display()
        );
        let p = FakePlatform::new();
        p.spawn(30, 1, START, "npm run dev", dir.path().to_str().unwrap());
        p.set_env(30, &[("PATH", &path), ("SECRET_TOKEN", "abc")]);
        let plan = plan_restart(&p, key(30, START), 3000).unwrap();
        assert_eq!(plan.argv[0], bin.join("npm").to_str().unwrap());
        assert_eq!(plan.argv[1..], ["run", "dev"]);
        assert_eq!(plan.cwd, dir.path());
        assert_eq!(plan.port, 3000);
        assert!(plan
            .env
            .iter()
            .any(|(k, v)| k == "SECRET_TOKEN" && v == "abc"));
        let debug = format!("{plan:?}");
        assert!(!debug.contains("abc") && !debug.contains("SECRET_TOKEN"));
        // Not in the PATH: unreadable.
        p.set_env(30, &[("PATH", "/nowhere")]);
        assert_eq!(
            plan_restart(&p, key(30, START), 3000).unwrap_err(),
            RestartBlocked::ArgvUnreadable
        );
    }

    #[test]
    fn relative_program_is_taken_from_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let p = FakePlatform::new();
        p.spawn(
            30,
            1,
            START,
            "./node_modules/.bin/vite --port 5173",
            dir.path().to_str().unwrap(),
        );
        p.set_env(30, &[]);
        let plan = plan_restart(&p, key(30, START), 5173).unwrap();
        assert_eq!(
            Path::new(&plan.argv[0]),
            dir.path().join("./node_modules/.bin/vite")
        );
    }

    #[test]
    fn logs_are_private_and_only_the_newest_20_kept() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        for i in 0..25u64 {
            fs::write(logs.join(format!("3000-{}.log", 1000 + i)), "").unwrap();
        }
        fs::write(logs.join("notes.txt"), "").unwrap();
        fs::create_dir_all(logs.join("sub.log")).unwrap();
        let path = create_log(dir.path(), 4000, 5000).unwrap();
        assert_eq!(path, logs.join("4000-5000.log"));
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let mut names: Vec<String> = fs::read_dir(&logs)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut want: Vec<String> = (1006..1025u64).map(|i| format!("3000-{i}.log")).collect();
        want.extend(["4000-5000.log".into(), "notes.txt".into(), "sub.log".into()]);
        want.sort();
        assert_eq!(names, want);
    }

    #[test]
    fn wait_port_free_polls_until_the_limit() {
        let p = FakePlatform::new();
        p.spawn(30, 1, START, "x", "/");
        p.add_listener(30, 3000);
        let ms = Duration::from_millis;
        assert!(!wait_port_free(&p, 3000, ms(30), ms(5)));
        assert!(wait_port_free(&p, 3001, ms(30), ms(5)));
        p.remove_proc(30);
        assert!(wait_port_free(&p, 3000, ms(30), ms(5)));
    }
}
