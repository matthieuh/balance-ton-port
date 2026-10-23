//! Restart: stop a server and start it again with its original command,
//! folder and environment. The plan is read from the live process at
//! restart time; the environment is handed to the new process and never
//! stored, logged or printed. A tracked server keeps its row (session, note,
//! Keep, Postpone) through a `PendingRestart` that links the new listener.

use crate::model::{ServerKey, TrackedServer};
use crate::platform::{Listener, Platform, ProcInfo};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a restarted process has to start listening before its pending
/// link is dropped.
pub const RESTART_LINK_MS: u64 = 120_000;

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
    /// The port was still taken 5 s after the stop: nothing was started.
    PortStillTaken,
    /// The new process could not be started (the server is stopped).
    SpawnFailed,
}

/// A restarted process waiting for its first listener, so that listener
/// links to `owner` (a copy of the stopped server's row) instead of being
/// linked from scratch. `owner` is `None` for an observed server: its new
/// process stays observed.
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
    /// new process's pid and start time (microseconds since the epoch).
    fn spawn(&self, plan: &RestartPlan, log: &Path) -> io::Result<(i32, u64)>;
}

/// The real spawner: the plan's environment only, its folder, stdin from
/// `/dev/null`, output appended to the log, in a process group of its own
/// so it outlives whoever asked for the restart.
pub struct CommandSpawner;

impl Spawner for CommandSpawner {
    fn spawn(&self, plan: &RestartPlan, log: &Path) -> io::Result<(i32, u64)> {
        use std::os::unix::process::CommandExt;
        let (program, args) = plan
            .argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty command line"))?;
        let out = OpenOptions::new().create(true).append(true).open(log)?;
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
        let start = start_time_of(pid);
        // Reaped when it exits, so a stopped server leaves no zombie.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        start
            .map(|s| (pid, s))
            .ok_or_else(|| io::Error::other("the new process exited at once"))
    }
}

#[cfg(target_os = "macos")]
fn start_time_of(pid: i32) -> Option<u64> {
    crate::platform::MacOs.proc_info(pid).map(|i| i.start_time)
}

#[cfg(not(target_os = "macos"))]
fn start_time_of(_pid: i32) -> Option<u64> {
    None
}

/// `<data dir>/logs/<port>-<unix ms>.log`.
pub fn log_path(data_dir: &Path, port: u16, now_ms: u64) -> PathBuf {
    data_dir.join("logs").join(format!("{port}-{now_ms}.log"))
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
