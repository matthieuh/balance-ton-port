//! `btp run`: real processes on random high ports. Every test runs `btp`
//! with a temp `BTP_DATA_DIR` and `HOME`, no agent config dirs and no Codex,
//! and without a TTY, so `btp run` acts in agent mode. The dev server is the
//! helper `tests/helpers/serve.py`, recognized through the debug-only
//! `BTP_TEST_FRAMEWORK` entry.

use assert_cmd::Command;
use btp_core::config::{save_config, Config, PortConflict};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
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

/// A port `p` with `p + 1` and `p + 2` free as well, below the ephemeral
/// range (where the system hands out neighbours to parallel tests), each
/// test of this run on its own block of ten.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 20_000 + (std::process::id() % 2_000) as u16 * 10;
    loop {
        let n = NEXT.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
        let p = 20_000 + (base - 20_000 + n) % 25_000;
        if (p..p + 3).all(|q| TcpListener::bind(("127.0.0.1", q)).is_ok()) {
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

fn answers(port: u16) -> bool {
    let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.write_all(b"GET / HTTP/1.0\r\n\r\n");
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out.contains("200")
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
    assert!(TcpListener::bind(("127.0.0.1", p)).is_ok(), "port free");
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
        let mut cmd = std::process::Command::new("script");
        env_of(&mut cmd, data, "long");
        cmd.current_dir(cwd)
            .args(["-q", "/dev/null"])
            .arg(assert_cmd::cargo::cargo_bin("btp"))
            .arg("run")
            .args(args)
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
    t.type_keys("r");
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
    assert!(TcpListener::bind(("127.0.0.1", p)).is_ok(), "port free");
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
