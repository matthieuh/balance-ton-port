//! `btp run`: real processes on random high ports. Every test runs `btp`
//! with a temp `BTP_DATA_DIR` and `HOME`, no agent config dirs and no Codex,
//! and without a TTY, so `btp run` acts in agent mode. The dev server is the
//! helper `tests/helpers/serve.py`, recognized through the debug-only
//! `BTP_TEST_FRAMEWORK` entry.

use assert_cmd::Command;
use btp_core::config::{save_config, Config, PortConflict};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(15);

/// A temp data dir whose config adds an empty dir as extra Claude config
/// dir (automatic discovery finds nothing under the temp `HOME`).
fn sandbox(port_conflict: PortConflict) -> (tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let cfg = Config {
        claude_config_dirs: vec![empty.path().to_path_buf()],
        port_conflict,
        ..Config::default()
    };
    save_config(data.path(), &cfg).unwrap();
    (data, empty)
}

fn helper() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/helpers/serve.py")
}

fn env_of(cmd: &mut std::process::Command, data: &Path, style: &str) {
    cmd.env("BTP_DATA_DIR", data)
        .env("HOME", data)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("BTP_RUN_ACTIVE")
        .env("BTP_TEST_NO_CODEX", "1")
        .env("BTP_TEST_FRAMEWORK", format!("serve.py:8000:{style}"));
}

fn btp(data: &Path, style: &str) -> Command {
    let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("btp"));
    env_of(&mut c, data, style);
    let mut c = Command::from_std(c);
    c.timeout(WAIT);
    c
}

/// Every port of `ports` can be bound on 127.0.0.1 (as the helper binds,
/// with SO_REUSEADDR). Checked in a python process, never in this one:
/// macOS sets close-on-exec on a new socket only after creating it, so a
/// socket of this process can leak into a `btp run` another test thread
/// spawns at that moment, and that `btp run` would then listen itself.
fn bindable(ports: &[u16]) -> bool {
    let list: Vec<String> = ports.iter().map(u16::to_string).collect();
    std::process::Command::new("python3")
        .args([
            "-c",
            "import socket,sys\n\
             for p in sys.argv[1:]:\n \
             s=socket.socket()\n \
             s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n \
             s.bind(('127.0.0.1',int(p)))\n \
             s.close()",
        ])
        .args(&list)
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A port `p` with `p + 1` and `p + 2` free as well, below the ephemeral
/// range (where the system hands out neighbours to parallel tests), each
/// test of this run on its own block of ten.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 20_000 + (std::process::id() % 2_000) as u16 * 10;
    loop {
        let n = NEXT.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
        let p = 20_000 + (base - 20_000 + n) % 25_000;
        if bindable(&[p, p + 1, p + 2]) {
            return p;
        }
    }
}

/// A process of its own listening on `port` until dropped: never this test
/// process, which other tests' servers descend from (it would then group
/// their listeners under it).
struct Holder(Child);

impl Holder {
    fn bind(port: u16) -> Holder {
        let mut child = std::process::Command::new("python3")
            .args([
                "-c",
                "import socket,sys,time\n\
                 s=socket.socket()\n\
                 s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)\n\
                 s.bind(('127.0.0.1',int(sys.argv[1])))\n\
                 s.listen()\n\
                 print('up',flush=True)\n\
                 time.sleep(120)",
                &port.to_string(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "up\n");
        Holder(child)
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The server on `port` answers HTTP 200 (asked from a python process, for
/// the reason given at `bindable`).
fn answers(port: u16) -> bool {
    std::process::Command::new("python3")
        .args([
            "-c",
            "import sys,urllib.request\n\
             r=urllib.request.urlopen('http://127.0.0.1:'+sys.argv[1]+'/',timeout=5)\n\
             sys.exit(0 if r.status==200 else 1)",
            &port.to_string(),
        ])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// `btp run` started in the background, its output read line by line.
struct Running {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    helper_pid: Option<i32>,
}

impl Running {
    fn start(data: &Path, style: &str, cwd: &Path, args: &[&str]) -> Running {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("btp"));
        env_of(&mut cmd, data, style);
        cmd.current_dir(cwd)
            .arg("run")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let (tx, lines) = channel();
        for r in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for l in BufReader::new(r).lines().map_while(Result::ok) {
                    let _ = tx.send(l);
                }
            });
        }
        Running {
            child,
            lines,
            seen: Vec::new(),
            helper_pid: None,
        }
    }

    /// The first line containing `needle`; panics after `WAIT`.
    fn wait_line(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        if let Some(l) = self.seen.iter().find(|l| l.contains(needle)) {
            return l.clone();
        }
        while Instant::now() < deadline {
            if let Ok(l) = self.lines.recv_timeout(Duration::from_millis(100)) {
                self.seen.push(l.clone());
                if let Some(pid) = l
                    .strip_prefix("serve.py listening on ")
                    .and_then(|r| r.split(" pid ").nth(1))
                    .and_then(|p| p.trim().parse().ok())
                {
                    self.helper_pid = Some(pid);
                }
                if l.contains(needle) {
                    return l;
                }
            }
        }
        panic!("no line with {needle:?}; saw {:?}", self.seen);
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("btp run did not exit");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // SIGTERM first: `btp run` forwards it to the server.
        // SAFETY: the `btp run` this test started.
        unsafe { libc::kill(self.pid(), libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && matches!(self.child.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(pid) = self.helper_pid {
            if alive(pid) {
                // SAFETY: the helper this test started.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn run_logs(data: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(data.join("logs/run")) {
        Ok(d) => d.filter_map(Result::ok).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    }
}

#[test]
fn unknown_command_untouched() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let out = btp(data.path(), "long")
        .args(["run", "--", "sh", "-c", "echo hi; exit 7"])
        .assert()
        .code(7);
    let o = out.get_output();
    assert_eq!(text(&o.stdout), "hi\n");
    assert!(!text(&o.stderr).contains("btp:"), "{}", text(&o.stderr));
}

#[test]
fn unreadable_port_runs_untouched_and_logged() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let h = helper();
    // Recognized, but its port is `$PORT` as a script would leave it: no
    // check, no rewrite. The helper fails on it as it would without btp.
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .args(["run", "--", h.to_str().unwrap(), "--port", "$PORT"])
        .assert()
        .code(1);
    let err = text(&out.get_output().stderr);
    assert!(!err.contains("btp:"), "{err}");
    assert!(err.contains("ValueError"), "{err}");
    assert_eq!(run_logs(data.path()).len(), 1, "logged");
}

#[test]
fn the_child_sees_btp_run_active() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let out = btp(data.path(), "long")
        .args(["run", "--", "sh", "-c", "echo \"[$BTP_RUN_ACTIVE]\""])
        .assert()
        .success();
    assert_eq!(text(&out.get_output().stdout), "[1]\n");
}

#[test]
fn usage_error_exits_2() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    btp(data.path(), "long").args(["run"]).assert().code(2);
    btp(data.path(), "long")
        .args(["run", "--bogus", "--", "true"])
        .assert()
        .code(2);
}

#[test]
fn free_port_starts() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let ps = p.to_string();
    let h = helper();
    let mut r = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--", h.to_str().unwrap(), "--port", &ps],
    );
    let line = r.wait_line("btp: ");
    let name = dir.path().file_name().unwrap().to_string_lossy();
    assert_eq!(line, format!("btp: {name} on http://localhost:{p}"));
    r.wait_line(&format!("serve.py listening on {p}"));
    assert!(answers(p), "the server answers on {p}");
}

#[test]
fn reroutes_around_observed_holder() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .env("SERVE_EXIT_AFTER", "1.5")
        .env("SERVE_EXIT_CODE", "5")
        .args([
            "run",
            "--",
            helper().to_str().unwrap(),
            "--port",
            &p.to_string(),
        ])
        .assert()
        .code(5);
    let o = out.get_output();
    let err = text(&o.stderr);
    let name = dir.path().file_name().unwrap().to_string_lossy();
    assert!(
        err.contains(&format!(
            "btp: {name} on http://localhost:{} ({p} is used by ",
            p + 1
        )),
        "{err}"
    );
    assert!(err.contains("not started by an agent"), "{err}");
    assert!(
        text(&o.stdout).contains(&format!("serve.py listening on {}", p + 1)),
        "{}",
        text(&o.stdout)
    );
}

#[test]
fn env_style_reroutes_through_port() {
    let (data, _e) = sandbox(PortConflict::Reroute);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    let out = btp(data.path(), "env")
        .current_dir(dir.path())
        .env("PORT", p.to_string())
        .env("SERVE_EXIT_AFTER", "1")
        .args(["run", "--", helper().to_str().unwrap()])
        .assert()
        .code(0);
    let o = out.get_output();
    assert!(
        text(&o.stdout).contains(&format!("serve.py listening on {}", p + 1)),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn agent_mode_ask_exits_3() {
    let (data, _e) = sandbox(PortConflict::Ask);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .args([
            "run",
            "--",
            helper().to_str().unwrap(),
            "--port",
            &p.to_string(),
        ])
        .assert()
        .code(3);
    let o = out.get_output();
    let err = text(&o.stderr);
    assert!(err.contains(&format!("btp: port {p} is used by ")), "{err}");
    assert!(
        err.contains("Ask the user, then rerun with --reroute or a free port."),
        "{err}"
    );
    assert!(!err.contains("--free"), "nothing to free: {err}");
    assert_eq!(text(&o.stdout), "", "nothing started");
    assert!(run_logs(data.path()).is_empty());
}

#[test]
fn block_when_not_reroutable() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    let out = btp(data.path(), "none")
        .current_dir(dir.path())
        .args([
            "run",
            "--",
            helper().to_str().unwrap(),
            "--port",
            &p.to_string(),
        ])
        .assert()
        .code(3);
    let o = out.get_output();
    let err = text(&o.stderr);
    assert!(err.contains(&format!("btp: port {p} is used by ")), "{err}");
    assert_eq!(text(&o.stdout), "", "nothing started");
    assert!(!answers(p + 1));
}

#[test]
fn a_broken_config_reroutes_and_never_frees() {
    use btp_core::platform::{MacOs, Platform};
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut holder = Holder::bind(p);
    // Freeable but for the config: a dead session's server, idle for
    // hours, while a watcher holds the lock.
    let pid = holder.0.id() as i32;
    let start_time = MacOs.proc_info(pid).unwrap().start_time;
    let state = serde_json::json!({"servers": [{
        "key": {"pid": pid, "start_time": start_time},
        "port": p, "command": "python3 holder", "worktree": dir.path(),
        "session_id": "s1", "session_pid": 1, "session_proc_start": "x",
        "session_name": "gone", "first_seen_ms": 1, "last_active_ms": 1,
        "kept": false, "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.path().join("state.json"), state.to_string()).unwrap();
    std::fs::write(data.path().join("config.toml"), "not = [toml").unwrap();
    let _watcher = btp_core::watcher::acquire_watch_lock(data.path(), "btp watch").unwrap();
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .env("SERVE_EXIT_AFTER", "0.5")
        .args([
            "run",
            "--free",
            "--",
            helper().to_str().unwrap(),
            "--port",
            &p.to_string(),
        ])
        .assert()
        .success();
    let err = text(&out.get_output().stderr);
    assert!(err.contains("btp: invalid "), "warned: {err}");
    assert!(
        err.contains(&format!("on http://localhost:{} ({p} is used by ", p + 1)),
        "{err}"
    );
    assert!(!err.contains("freed"), "{err}");
    assert!(
        holder.0.try_wait().unwrap().is_none(),
        "the holder was killed"
    );
}

#[test]
fn reuse_agent_mode() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let ps = p.to_string();
    let h = helper();
    let args = ["--", h.to_str().unwrap(), "--port", &ps];
    let mut first = Running::start(data.path(), "long", dir.path(), &args);
    first.wait_line(&format!("serve.py listening on {p}"));
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .arg("run")
        .args(args)
        .assert()
        .code(0);
    let o = out.get_output();
    let err = text(&o.stderr);
    let name = dir.path().file_name().unwrap().to_string_lossy();
    assert!(
        err.contains(&format!(
            "btp: {name} already running on http://localhost:{p} ("
        )),
        "{err}"
    );
    assert!(err.trim_end().ends_with("), reusing it"), "{err}");
    assert_eq!(text(&o.stdout), "", "nothing started");
    assert!(!answers(p + 1), "no second server");
    // `--new` starts a second copy, rerouted.
    let mut second = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--new", "--", h.to_str().unwrap(), "--port", &ps],
    );
    second.wait_line(&format!("serve.py listening on {}", p + 1));
}

#[test]
fn ctrl_c_forwards() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let ps = p.to_string();
    let h = helper();
    let mut r = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--", h.to_str().unwrap(), "--port", &ps],
    );
    r.wait_line(&format!("serve.py listening on {p}"));
    let server = r.helper_pid.expect("helper pid");
    // SAFETY: the `btp run` this test started.
    unsafe { libc::kill(r.pid(), libc::SIGINT) };
    let status = r.wait();
    // The child died of SIGINT: so does `btp run` (R14).
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    assert!(!alive(server), "the server is gone");
    assert!(bindable(&[p]), "port free");
}

#[test]
fn sigterm_forwards_too() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--", "sh", "-c", "echo up; exec sleep 30"],
    );
    r.wait_line("up");
    std::thread::sleep(Duration::from_millis(100));
    // SAFETY: the `btp run` this test started.
    unsafe { libc::kill(r.pid(), libc::SIGTERM) };
    let status = r.wait();
    assert_eq!(status.signal(), Some(libc::SIGTERM), "{status:?}");
}

#[test]
fn nested_is_untouched() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let out = btp(data.path(), "long")
        .env("BTP_RUN_ACTIVE", "1")
        .args(["run", "--", "sh", "-c", "echo nested; exit 4"])
        .assert()
        .code(4);
    let o = out.get_output();
    assert_eq!(text(&o.stdout), "nested\n");
    assert!(!text(&o.stderr).contains("btp:"));
    assert!(run_logs(data.path()).is_empty(), "no log");
}

#[test]
fn log_written_and_tee_identical() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let out = btp(data.path(), "long")
        .args([
            "run",
            "--",
            "sh",
            "-c",
            "printf 'a\\377b\\n'; printf 'e\\376\\n' >&2; printf 'c\\0d'",
        ])
        .assert()
        .success();
    let o = out.get_output();
    assert_eq!(o.stdout, b"a\xffb\nc\0d");
    assert_eq!(o.stderr, b"e\xfe\n");
    let logs = run_logs(data.path());
    assert_eq!(logs.len(), 1, "{logs:?}");
    let log = std::fs::read(&logs[0]).unwrap();
    assert_eq!(log.len(), o.stdout.len() + o.stderr.len());
    let has = |needle: &[u8]| log.windows(needle.len()).any(|w| w == needle);
    assert!(
        has(b"a\xffb\n") && has(b"e\xfe\n") && has(b"c\0d"),
        "{log:?}"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&logs[0]).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn no_check_runs_on_the_taken_port() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    // The helper cannot bind and fails: btp left the command as it was.
    let out = btp(data.path(), "long")
        .current_dir(dir.path())
        .args([
            "run",
            "--no-check",
            "--",
            helper().to_str().unwrap(),
            "--port",
            &p.to_string(),
        ])
        .assert()
        .failure();
    let err = text(&out.get_output().stderr);
    assert!(!err.contains("btp:"), "{err}");
    assert!(err.contains("Address already in use"), "{err}");
}

/// `btp run` in a terminal: through `script`, which gives it a pty. Its
/// output (the pty's) is read as it comes; `keys` are typed into it.
struct InTerminal {
    child: Child,
    out: Receiver<u8>,
    seen: Vec<u8>,
}

impl InTerminal {
    fn start(data: &Path, cwd: &Path, args: &[&str]) -> InTerminal {
        let mut argv = vec![
            assert_cmd::cargo::cargo_bin("btp").display().to_string(),
            "run".to_string(),
        ];
        argv.extend(args.iter().map(|a| a.to_string()));
        InTerminal::spawn(data, cwd, &argv)
    }

    /// `sh -c <line>` in the terminal.
    fn shell(data: &Path, cwd: &Path, line: &str) -> InTerminal {
        InTerminal::spawn(data, cwd, &["sh".into(), "-c".into(), line.into()])
    }

    fn spawn(data: &Path, cwd: &Path, argv: &[String]) -> InTerminal {
        let mut cmd = std::process::Command::new("script");
        env_of(&mut cmd, data, "long");
        cmd.current_dir(cwd)
            .args(["-q", "/dev/null"])
            .args(argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().unwrap();
        let (tx, out) = channel();
        let mut src = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            while matches!(src.read(&mut b), Ok(1)) {
                let _ = tx.send(b[0]);
            }
        });
        InTerminal {
            child,
            out,
            seen: Vec::new(),
        }
    }

    fn wait_text(&mut self, needle: &str) {
        self.wait_count(needle, 1);
    }

    /// Until `needle` was printed `n` times.
    fn wait_count(&mut self, needle: &str, n: usize) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if text(&self.seen).matches(needle).count() >= n {
                return;
            }
            if let Ok(b) = self.out.recv_timeout(Duration::from_millis(50)) {
                self.seen.push(b);
            }
        }
        panic!("no {needle:?} in {:?}", text(&self.seen));
    }

    /// Reads what comes for 300 ms more.
    fn settle(&mut self) {
        let end = Instant::now() + Duration::from_millis(300);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if let Ok(b) = self.out.recv_timeout(left) {
                self.seen.push(b);
            }
        }
    }

    fn type_keys(&mut self, keys: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(keys.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("script did not exit; saw {:?}", text(&self.seen));
    }
}

impl Drop for InTerminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn terminal_ask_offers_other_port_or_cancel() {
    let (data, _e) = sandbox(PortConflict::Ask);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _holder = Holder::bind(p);
    let h = helper();
    let ps = p.to_string();
    let mut t = InTerminal::start(
        data.path(),
        dir.path(),
        &["--", h.to_str().unwrap(), "--port", &ps],
    );
    t.wait_text(&format!(
        "Port {p} is used by Python import (not started by an agent). [o]ther port, [c]ancel?"
    ));
    // Not offered: nothing happens.
    t.type_keys("k");
    std::thread::sleep(Duration::from_millis(300));
    t.type_keys("c");
    assert_eq!(t.wait().code(), Some(3));
    assert!(
        !text(&t.seen).contains("serve.py listening"),
        "nothing started"
    );
}

#[test]
fn terminal_reuse_follows_logs_and_r_restarts_here() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let ps = p.to_string();
    let h = helper();
    let args = ["--", h.to_str().unwrap(), "--port", &ps];
    let mut first = Running::start(data.path(), "long", dir.path(), &args);
    first.wait_line(&format!("serve.py listening on {p}"));
    let old = first.helper_pid.unwrap();
    let mut t = InTerminal::start(data.path(), dir.path(), &args);
    t.wait_text("reusing it");
    t.wait_text("Following its logs. Ctrl-C stops following; the server keeps running. Press r to restart it here.");
    // The log so far, then `r`: the old server stops, a new one starts here.
    t.wait_text(&format!("serve.py listening on {p} pid {old}"));
    // `r` asks first; Return (like any key but `y`) keeps the server.
    let question = format!(" on :{p} and start it here? [y/N]");
    t.type_keys("r");
    t.wait_text(&question);
    assert!(text(&t.seen).contains("Stop "), "{:?}", text(&t.seen));
    t.type_keys("\r");
    t.wait_text("btp: not stopped; still following");
    assert!(alive(old), "Return stopped it");
    t.type_keys("r");
    t.wait_count(&question, 2);
    t.type_keys("y");
    t.wait_text("stopped it; starting");
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    t.wait_text(&format!("btp: {name} on http://localhost:{p}\r\n"));
    t.wait_count(&format!("serve.py listening on {p} pid "), 2);
    assert_eq!(first.wait().signal(), Some(libc::SIGTERM));
    assert!(!alive(old));
    assert!(answers(p));
    // Ctrl-C reaches the new server through the pty: it stops too.
    t.type_keys("\x03");
    let status = t.wait();
    // `script` reports the signal its child (btp) died of.
    assert_eq!(status.code(), Some(libc::SIGINT), "{:?}", text(&t.seen));
    assert!(bindable(&[p]), "port free");
}

#[test]
fn terminal_background_job_acts_as_an_agent() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let ps = p.to_string();
    let h = helper();
    let args = ["--", h.to_str().unwrap(), "--port", &ps];
    let mut first = Running::start(data.path(), "long", dir.path(), &args);
    first.wait_line(&format!("serve.py listening on {p}"));
    // A job in the background of an interactive shell (`set -m`): not the
    // terminal's foreground group, so no following and no raw mode (which
    // would stop it with SIGTTOU), just the reuse line and exit 0.
    let btp = assert_cmd::cargo::cargo_bin("btp");
    let line = format!(
        "set -m; '{}' run -- '{}' --port {p} & wait $!; echo \"exit=$?\"",
        btp.display(),
        h.display()
    );
    let mut t = InTerminal::shell(data.path(), dir.path(), &line);
    t.wait_text("reusing it");
    t.wait_text("exit=0");
    assert!(
        !text(&t.seen).contains("Following its logs"),
        "{:?}",
        text(&t.seen)
    );
    assert!(alive(first.helper_pid.unwrap()));
}

/// A python helper that counts the SIGINTs it gets for `secs` seconds,
/// then prints the count and exits 0.
const COUNT_SIGINT: &str = "import signal,sys,time\n\
n=0\n\
def h(s,f):\n global n\n n+=1\n\
signal.signal(signal.SIGINT,h)\n\
print('ready',flush=True)\n\
end=time.time()+float(sys.argv[1])\n\
while time.time()<end: time.sleep(0.05)\n\
print('count',n,flush=True)";

#[test]
fn terminal_ctrl_c_reaches_the_child_once() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let mut t = InTerminal::start(
        data.path(),
        dir.path(),
        &["--", "python3", "-c", COUNT_SIGINT, "2"],
    );
    t.wait_text("ready");
    t.type_keys("\x03");
    t.wait_text("count ");
    t.settle();
    assert!(text(&t.seen).contains("count 1"), "{:?}", text(&t.seen));
    assert_eq!(t.wait().code(), Some(0));
}

#[test]
fn a_kill_is_forwarded_once() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let mut r = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--", "python3", "-c", COUNT_SIGINT, "2"],
    );
    r.wait_line("ready");
    // SAFETY: the `btp run` this test started.
    unsafe { libc::kill(r.pid(), libc::SIGINT) };
    r.wait_line("count 1");
    assert_eq!(r.wait().code(), Some(0));
}

#[test]
fn terminal_child_sees_a_terminal() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let mut t = InTerminal::start(
        data.path(),
        dir.path(),
        &[
            "--",
            "python3",
            "-c",
            "import os; print('tty', os.isatty(1), os.isatty(2))",
        ],
    );
    t.wait_text("tty ");
    t.settle();
    assert!(
        text(&t.seen).contains("tty True True"),
        "{:?}",
        text(&t.seen)
    );
    assert_eq!(t.wait().code(), Some(0));
    let logs = run_logs(data.path());
    assert_eq!(logs.len(), 1);
    let log = std::fs::read(&logs[0]).unwrap();
    assert_eq!(log, b"tty True True\n", "the child's bytes, unchanged");
}

#[test]
fn a_background_grandchild_does_not_hold_btp_run() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let started = Instant::now();
    let out = btp(data.path(), "long")
        .args(["run", "--", "sh", "-c", "sleep 30 & echo $!"])
        .assert()
        .success();
    let elapsed = started.elapsed();
    let pid: i32 = text(&out.get_output().stdout).trim().parse().unwrap();
    // SAFETY: the sleep this test started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn a_closed_stdout_ends_the_child() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let bin = assert_cmd::cargo::cargo_bin("btp");
    let out = Command::new("sh")
        .env("BTP_DATA_DIR", data.path())
        .env("HOME", data.path())
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("BTP_RUN_ACTIVE")
        .env("BTP_TEST_NO_CODEX", "1")
        .arg("-c")
        .arg(format!("'{}' run -- yes | head -1", bin.display()))
        .timeout(WAIT)
        .assert()
        .success();
    assert_eq!(text(&out.get_output().stdout), "y\n");
}

#[test]
fn a_panic_before_the_start_runs_the_command_untouched() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let out = btp(data.path(), "long")
        .env("BTP_TEST_PANIC_BEFORE_SPAWN", "1")
        .args(["run", "--", "sh", "-c", "echo hi; exit 7"])
        .assert()
        .code(7);
    let o = out.get_output();
    assert_eq!(text(&o.stdout), "hi\n");
    assert_eq!(
        text(&o.stderr),
        "btp: port check skipped (internal error), running the command as it is\n"
    );
}

#[test]
fn terminal_stderr_redirect_is_kept() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let err = dir.path().join("err.txt");
    let mut t = InTerminal::shell(
        data.path(),
        dir.path(),
        &format!(
            "'{}' run -- python3 -c \"import os,sys; print('out', os.isatty(1)); sys.stderr.write('err-bytes\\n')\" 2>'{}'",
            assert_cmd::cargo::cargo_bin("btp").display(),
            err.display()
        ),
    );
    assert_eq!(t.wait().code(), Some(0));
    t.settle();
    let seen = text(&t.seen);
    assert!(seen.contains("out True"), "{seen:?}");
    assert!(!seen.contains("err-bytes"), "{seen:?}");
    assert_eq!(std::fs::read_to_string(&err).unwrap(), "err-bytes\n");
}

#[test]
fn a_slow_reader_still_gets_everything() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let bin = assert_cmd::cargo::cargo_bin("btp");
    // 100000 bytes fit in the two pipe buffers: the child exits at once and
    // btp still holds what it left when the reader starts, 1 s later.
    for size in [100_000, 300_000] {
        let out = Command::new("sh")
            .env("BTP_DATA_DIR", data.path())
            .env("HOME", data.path())
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("BTP_RUN_ACTIVE")
            .env("BTP_TEST_NO_CODEX", "1")
            .arg("-c")
            .arg(format!(
                "'{}' run -- sh -c 'head -c {size} /dev/zero' | (sleep 1; wc -c)",
                bin.display()
            ))
            .timeout(WAIT)
            .assert()
            .success();
        assert_eq!(text(&out.get_output().stdout).trim(), size.to_string());
    }
}

// ---- btp logs ----

/// `btp run` of a shell that prints `prelude` and then execs the helper on
/// `port`, so the listener is the very process whose output is logged.
fn start_logged(data: &Path, cwd: &Path, port: u16, prelude: &str) -> Running {
    let h = helper();
    let script = format!("{prelude}exec python3 '{}' --port {port}", h.display());
    let mut r = Running::start(data, "long", cwd, &["--", "sh", "-c", &script]);
    r.wait_line(&format!("serve.py listening on {port}"));
    wait_log(data, &format!("serve.py listening on {port}"));
    r
}

/// The run log of the data dir holds `needle` (the terminal output arrives
/// a moment before the log write).
fn wait_log(data: &Path, needle: &str) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let found = run_logs(data)
            .iter()
            .any(|p| text(&std::fs::read(p).unwrap_or_default()).contains(needle));
        if found {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no log with {needle:?}");
}

fn wait_log_bytes(data: &Path, needle: &[u8]) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        let found = run_logs(data).iter().any(|p| {
            let b = std::fs::read(p).unwrap_or_default();
            b.windows(needle.len()).any(|w| w == needle)
        });
        if found {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no log with {needle:?}");
}

fn logs_of(data: &Path, args: &[&str]) -> std::process::Output {
    btp(data, "long")
        .arg("logs")
        .args(args)
        .assert()
        .success()
        .get_output()
        .clone()
}

#[test]
fn logs_tail_default_200() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _r = start_logged(
        data.path(),
        dir.path(),
        p,
        "i=1; while [ $i -le 300 ]; do echo line$i; i=$((i+1)); done; ",
    );
    let out = text(&logs_of(data.path(), &[&p.to_string()]).stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 200, "{out}");
    assert_eq!(lines[0], "line102");
    assert!(lines[199].starts_with("serve.py listening on"), "{out}");
    let out = text(&logs_of(data.path(), &[&p.to_string(), "-n", "2"]).stdout);
    assert_eq!(out.lines().collect::<Vec<_>>().len(), 2);
    assert!(out.starts_with("line300\n"), "{out}");
}

#[test]
fn logs_errors_only() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _r = start_logged(
        data.path(),
        dir.path(),
        p,
        "for i in 1 2 3 4 5 6 7 8 9 10; do echo fill$i; done; \
         echo 'TypeError: x is not defined'; \
         for i in 1 2 3 4 5 6 7 8 9 10; do echo tail$i; done; \
         (sleep 1; printf 'bin\\377ary failed\\n') & ",
    );
    // Not valid UTF-8, so the terminal reader of `Running` stops there.
    wait_log_bytes(data.path(), b"ary failed");
    let out = text(&logs_of(data.path(), &[&p.to_string(), "--errors"]).stdout);
    let lines: Vec<&str> = out.lines().collect();
    // Two lines of context around the first error; the second is binary
    // output with invalid UTF-8, which must not panic.
    assert_eq!(
        &lines[..5],
        [
            "fill9",
            "fill10",
            "TypeError: x is not defined",
            "tail1",
            "tail2"
        ]
    );
    assert_eq!(lines[5], "--", "{out}");
    assert!(lines[6..].iter().any(|l| l.contains("ary failed")), "{out}");
    assert!(!out.contains("fill8") && !out.contains("tail3"), "{out}");
}

#[test]
fn logs_unknown_exit_1() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    for target in ["59999", "nothing-like-this"] {
        let out = btp(data.path(), "long")
            .args(["logs", target])
            .assert()
            .code(1);
        assert_eq!(
            text(&out.get_output().stderr),
            format!("btp: no logs for {target} (it was not started through btp run)\n")
        );
    }
}

#[test]
fn logs_of_a_server_without_a_log_exit_1() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let p = free_port();
    let _holder = Holder::bind(p);
    let out = btp(data.path(), "long")
        .args(["logs", &p.to_string()])
        .assert()
        .code(1);
    assert_eq!(
        text(&out.get_output().stderr),
        format!("btp: no logs for {p} (it was not started through btp run)\n")
    );
}

#[test]
fn logs_ambiguous_name_lists_the_matches() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let (p1, p2) = (free_port(), free_port());
    let _a = start_logged(data.path(), dir.path(), p1, "");
    let _b = start_logged(data.path(), dir.path(), p2, "");
    let name = dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_uppercase();
    let out = btp(data.path(), "long")
        .args(["logs", &name])
        .assert()
        .code(1);
    let err = text(&out.get_output().stderr);
    assert!(err.contains("matches several servers"), "{err}");
    assert!(
        err.contains(&p1.to_string()) && err.contains(&p2.to_string()),
        "{err}"
    );
    // A port is never ambiguous.
    logs_of(data.path(), &[&p1.to_string()]);
}

#[test]
fn list_json_has_logs() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let r = start_logged(data.path(), dir.path(), p, "echo hello-tracked; ");
    let pid = r.helper_pid.unwrap();
    use btp_core::platform::{MacOs, Platform};
    let start_time = MacOs.proc_info(pid).unwrap().start_time;
    // The same server, tracked for a session.
    let state = serde_json::json!({"servers": [{
        "key": {"pid": pid, "start_time": start_time},
        "port": p,
        "command": format!("python3 {} --port {p}", helper().display()),
        "worktree": dir.path(),
        "session_id": "s1",
        "session_pid": 1,
        "session_proc_start": "x",
        "session_name": "my-tracked-session",
        "first_seen_ms": 1,
        "last_active_ms": 1,
        "kept": true,
        "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.path().join("state.json"), state.to_string()).unwrap();
    let out = btp(data.path(), "long")
        .args(["list", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let servers = v["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "{v}");
    assert_eq!(servers[0]["has_logs"], true);
    // A tracked server answers to its session name too.
    let out = text(&logs_of(data.path(), &["MY-TRACKED-SESSION"]).stdout);
    assert!(out.contains("hello-tracked"), "{out}");
}

/// `btp logs -f` started in the background, its output read line by line.
fn follow_logs(data: &Path, args: &[&str]) -> Running {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("btp"));
    env_of(&mut cmd, data, "long");
    cmd.arg("logs")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let (tx, lines) = channel();
    for r in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().unwrap()),
    ] {
        let tx = tx.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(r).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
    }
    Running {
        child,
        lines,
        seen: Vec::new(),
        helper_pid: None,
    }
}

#[test]
fn logs_follow_streams_new_lines_and_stops_with_the_server() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _r = start_logged(
        data.path(),
        dir.path(),
        p,
        "echo before; (sleep 2; echo after-follow; sleep 1; echo Error: late; kill $$) & ",
    );
    let mut f = follow_logs(data.path(), &[&p.to_string(), "-f"]);
    f.wait_line("before");
    f.wait_line("after-follow");
    f.wait_line("Error: late");
    // The server is killed by the background job (the exec'd python has
    // the shell's pid): `-f` ends by itself, with status 0.
    assert!(f.wait().success());
}

#[test]
fn logs_follow_errors_filters_new_lines() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _r = start_logged(
        data.path(),
        dir.path(),
        p,
        "(sleep 2; echo noise-one; echo Error: boom; echo noise-two; sleep 1; kill $$) & ",
    );
    let mut f = follow_logs(data.path(), &[&p.to_string(), "-f", "--errors"]);
    f.wait_line("Error: boom");
    assert!(f.wait().success());
    assert!(!f.seen.iter().any(|l| l.contains("noise")), "{:?}", f.seen);
}

#[test]
fn logs_follow_ends_on_sigint() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let _r = start_logged(data.path(), dir.path(), p, "echo hi; ");
    let mut f = follow_logs(data.path(), &[&p.to_string(), "-f"]);
    f.wait_line("hi");
    // SAFETY: the `btp logs` this test started.
    unsafe { libc::kill(f.pid(), libc::SIGINT) };
    assert!(f.wait().success());
    assert!(answers(p), "the server keeps running");
}

#[test]
fn logs_found_through_a_launcher_that_did_not_exec() {
    let (data, _e) = sandbox(PortConflict::FreeIdle);
    let dir = tempfile::tempdir().unwrap();
    let p = free_port();
    let h = helper();
    // `sh` stays the parent of the listener, as `npm run dev` does.
    let script = format!("echo launcher-line; python3 '{}' --port {p}", h.display());
    let mut r = Running::start(
        data.path(),
        "long",
        dir.path(),
        &["--", "sh", "-c", &script],
    );
    r.wait_line(&format!("serve.py listening on {p}"));
    wait_log(data.path(), "launcher-line");
    let out = text(&logs_of(data.path(), &[&p.to_string()]).stdout);
    assert!(out.contains("launcher-line"), "{out}");
    // Tracked as well: the list flags it.
    let pid = r.helper_pid.unwrap();
    use btp_core::platform::{MacOs, Platform};
    let start_time = MacOs.proc_info(pid).unwrap().start_time;
    let state = serde_json::json!({"servers": [{
        "key": {"pid": pid, "start_time": start_time},
        "port": p,
        "command": format!("python3 {} --port {p}", h.display()),
        "worktree": dir.path(),
        "session_id": "s1",
        "session_pid": 1,
        "session_proc_start": "x",
        "first_seen_ms": 1,
        "last_active_ms": 1,
        "kept": true,
        "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.path().join("state.json"), state.to_string()).unwrap();
    let out = btp(data.path(), "long")
        .args(["list", "--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["servers"][0]["has_logs"], true, "{v}");
}
