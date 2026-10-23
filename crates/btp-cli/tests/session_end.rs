//! `btp session-end`, `note`, `stale`, `recent` and `_sweep-later` against
//! real processes. Every process signalled here is one this test spawned: a
//! `bash` run under the name `claude` stands in for the Claude Code process,
//! and a `perl` one-liner is its dev server. `BTP_DATA_DIR` and `HOME` point to a
//! temp dir, `CLAUDE_CONFIG_DIR` is unset, and notifications go to a file
//! (`BTP_TEST_NOTIFY_FILE`), so osascript never runs.
#![cfg(target_os = "macos")]

use assert_cmd::Command;
use btp_core::config::{save_config, Config};
use btp_core::platform::{MacOs, Platform};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

struct Sandbox {
    data: tempfile::TempDir,
    claude: tempfile::TempDir,
    wt: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        let s = Sandbox {
            data: tempfile::tempdir().unwrap(),
            claude: tempfile::tempdir().unwrap(),
            wt: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir_all(s.claude.path().join("sessions")).unwrap();
        save_config(
            s.data.path(),
            &Config {
                claude_config_dirs: vec![s.claude.path().to_path_buf()],
                kill_grace_s: 1,
                ..Config::default()
            },
        )
        .unwrap();
        s
    }

    fn notify_file(&self) -> PathBuf {
        self.data.path().join("notified.txt")
    }

    fn env(&self, c: &mut std::process::Command) {
        c.env("BTP_DATA_DIR", self.data.path())
            .env("HOME", self.data.path())
            .env("BTP_TEST_NOTIFY_FILE", self.notify_file())
            .env("BTP_HOOK_DEBUG", "1")
            .env_remove("CLAUDE_CONFIG_DIR");
    }

    fn debug_log(&self) -> String {
        std::fs::read_to_string(self.data.path().join("session-end.log")).unwrap_or_default()
    }

    fn btp(&self) -> Command {
        let mut c = Command::cargo_bin("btp").unwrap();
        c.env("BTP_DATA_DIR", self.data.path())
            .env("HOME", self.data.path())
            .env("BTP_TEST_NOTIFY_FILE", self.notify_file())
            .env_remove("CLAUDE_CONFIG_DIR");
        c
    }

    /// Registers `pid` as a live Claude Code session in the sandbox.
    fn session_file(&self, pid: i32, id: &str) {
        let proc_start = MacOs.proc_start_string(pid).expect("running");
        let v = serde_json::json!({
            "pid": pid,
            "sessionId": id,
            "cwd": self.wt.path(),
            "status": "busy",
            "statusUpdatedAt": 0,
            "procStart": proc_start,
        });
        std::fs::write(
            self.claude.path().join(format!("sessions/{pid}.json")),
            v.to_string(),
        )
        .unwrap();
    }

    fn notified(&self) -> String {
        std::fs::read_to_string(self.notify_file()).unwrap_or_default()
    }
}

/// A process this test spawned. SIGKILLed and reaped on drop.
struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A process spawned (indirectly) by this test: SIGKILLed on drop when it
/// is still that very process.
struct Spawned {
    pid: i32,
    start: u64,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if MacOs
            .proc_info(self.pid)
            .is_some_and(|i| i.start_time == self.start)
        {
            MacOs.signal(self.pid, libc_sigkill());
        }
    }
}

fn libc_sigkill() -> i32 {
    9
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The fake session: starts the dev server, waits for `go`, then runs
/// `btp <args...>` as its own child (like a hook), writes `done` with the
/// time the command took, and waits for the server.
fn spawn_session(sb: &Sandbox, server: &str, btp_args: &str) -> Owned {
    let go = sb.data.path().join("go");
    let done = sb.data.path().join("done");
    let btp = assert_cmd::cargo::cargo_bin("btp");
    let script = format!(
        r#"{server} &
while [ ! -f "{go}" ]; do sleep 0.02; done
start=$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')
"{btp}" {btp_args} > "{out}" 2>&1
code=$?
end=$(perl -MTime::HiRes=time -e 'printf "%d", time*1000')
echo "$code $((end - start))" > "{done}"
wait"#,
        go = go.display(),
        done = done.display(),
        btp = btp.display(),
        out = sb.data.path().join("out.txt").display(),
    );
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new("/bin/bash");
    c.arg0("claude")
        .arg("-c")
        .arg(script)
        .current_dir(sb.wt.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    sb.env(&mut c);
    Owned(c.spawn().unwrap())
}

/// A perl dev server on `port`; `ignore_term` makes it survive SIGTERM;
/// it accepts connections and holds them.
fn perl_server(port: u16, ignore_term: bool) -> String {
    let term = if ignore_term {
        "$SIG{TERM}=q(IGNORE);"
    } else {
        ""
    };
    format!(
        "perl -MIO::Socket::INET -e '{term} $s=IO::Socket::INET->new(LocalAddr=>q(127.0.0.1),LocalPort=>{port},Listen=>5,ReuseAddr=>1) or die; @c=(); while(1){{ push @c, $s->accept }}'"
    )
}

/// The process listening on `port`, wherever it hangs.
fn wait_port(port: u16) -> Spawned {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(l) = MacOs.listeners().into_iter().find(|l| l.port == port) {
            let start = MacOs.proc_info(l.pid).unwrap().start_time;
            return Spawned { pid: l.pid, start };
        }
        assert!(Instant::now() < deadline, "server never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The process listening on `port` among the descendants of `root`.
fn wait_listener(root: i32, port: u16) -> Spawned {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let kids = MacOs.descendants(root);
        if let Some(l) = MacOs
            .listeners()
            .into_iter()
            .find(|l| l.port == port && kids.contains(&l.pid))
        {
            let start = MacOs.proc_info(l.pid).unwrap().start_time;
            return Spawned { pid: l.pid, start };
        }
        assert!(Instant::now() < deadline, "server never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_file(p: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(t) = std::fs::read_to_string(p) {
            if !t.is_empty() {
                return t;
            }
        }
        assert!(Instant::now() < deadline, "{} never written", p.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_gone(pid: i32, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if MacOs.proc_info(pid).is_none() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// `(exit code, milliseconds)` of the btp run inside the fake session.
fn run_result(sb: &Sandbox) -> (i32, u64) {
    let t = wait_file(&sb.data.path().join("done"));
    let mut it = t.split_whitespace();
    (
        it.next().unwrap().parse().unwrap(),
        it.next().unwrap().parse().unwrap(),
    )
}

/// The `_sweep-later` processes started for the session `sh`.
fn waiters(sh: i32) -> Vec<Spawned> {
    let out = std::process::Command::new("/usr/bin/pgrep")
        .args(["-f", &format!("_sweep-later --session={sh}:")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .filter_map(|p| p.parse::<i32>().ok())
        .filter_map(|pid| {
            let start = MacOs.proc_info(pid)?.start_time;
            Some(Spawned { pid, start })
        })
        .collect()
}

#[test]
fn session_end_kills_nothing_and_starts_one_waiter() {
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &perl_server(port, false),
        "session-end --session-pid $$ --reason prompt_input_exit --session-id conv-1",
    );
    let sh = session.0.id() as i32;
    let server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    let (code, ms) = run_result(&sb);
    assert_eq!(code, 0);
    assert!(ms < 1500, "session-end took {ms} ms");
    let out = std::fs::read_to_string(sb.data.path().join("out.txt")).unwrap();
    assert_eq!(out, "", "nothing printed");
    // One detached waiter (held for cleanup before any assertion).
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut found = waiters(sh);
    while found.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        found = waiters(sh);
    }
    assert_eq!(found.len(), 1, "one waiter");
    assert!(!wait_gone(server.pid, Duration::from_millis(500)), "killed");
    let n = sb.notified();
    assert!(n.starts_with("Balance ton port|"), "{n}");
    assert!(
        n.contains("|Session ended: 1 server kept running, stopped after 20 min unused: "),
        "{n}"
    );
    assert!(n.contains(&format!(":{port}.")), "{n}");
    assert!(sb
        .btp()
        .args(["recent"])
        .assert()
        .success()
        .get_output()
        .stdout
        .starts_with(b"no kills"));
    // A second waiter for the same session gives way at once.
    let proc_start = MacOs.proc_start_string(sh).unwrap();
    let t = Instant::now();
    sb.btp()
        .env("BTP_HOOK_DEBUG", "1")
        .args([
            "_sweep-later",
            &format!("--session={sh}:{proc_start}"),
            "--at=0",
        ])
        .timeout(Duration::from_secs(5))
        .assert()
        .success();
    assert!(t.elapsed() < Duration::from_secs(2));
    assert!(MacOs.proc_info(server.pid).is_some());
    let log = sb.debug_log();
    assert!(log.contains("kept running"), "{log}");
    assert!(log.contains("waiter started for"), "{log}");
    assert!(log.contains("another one runs"), "{log}");
}

#[test]
fn session_end_works_after_the_session_file_is_gone() {
    // Claude Code removes its session file before SessionEnd hooks run, and
    // no watcher tracked the server: the process identity is enough.
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &perl_server(port, false),
        "session-end --session-pid $$ --reason other --session-id conv-1",
    );
    let sh = session.0.id() as i32;
    let server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    std::fs::remove_file(sb.claude.path().join(format!("sessions/{sh}.json"))).unwrap();
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    assert!(!wait_gone(server.pid, Duration::from_millis(300)), "killed");
    let log = sb.debug_log();
    assert!(log.contains("reason=other"), "{log}");
    assert!(log.contains("session file: missing"), "{log}");
    assert!(log.contains(&format!("linked :{port}")), "{log}");
    assert!(sb.notified().contains("kept running"), "{}", sb.notified());
    let _w = waiters(sh);
}

#[test]
fn a_server_already_moved_out_of_the_session_is_left_and_traced() {
    // A background task detached before the hook ran: its parent exited and
    // it hangs off launchd. Nothing proves it was the session's: left.
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &format!("( {} & ); sleep 0", perl_server(port, false)),
        "session-end --session-pid $$ --reason other",
    );
    let server = wait_port(port);
    assert_eq!(MacOs.proc_info(server.pid).unwrap().ppid, 1);
    let sh = session.0.id() as i32;
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    assert!(!wait_gone(server.pid, Duration::from_millis(300)), "killed");
    let log = sb.debug_log();
    assert!(
        log.contains(&format!("left :{port} pid {}", server.pid)),
        "{log}"
    );
    assert_eq!(sb.notified(), "");
    assert!(waiters(sh).is_empty(), "no waiter without kept servers");
}

#[test]
fn session_end_leaves_a_server_with_an_open_connection() {
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &perl_server(port, false),
        "session-end --session-pid $$ --reason other",
    );
    let sh = session.0.id() as i32;
    let guard = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    let _tab = TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    assert!(!wait_gone(guard.pid, Duration::from_millis(300)), "killed");
    assert!(
        sb.notified().contains(&format!(":{port}.")),
        "{}",
        sb.notified()
    );
    let _w = waiters(sh);
}

#[test]
fn the_waiter_stops_a_done_server_once_the_session_is_gone() {
    // The session marks its server done and quits; the server stays
    // behind (reparented). The waiter then applies the rule, unless a
    // watcher holds the lock.
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &perl_server(port, false),
        &format!("note {port} --done"),
    );
    let sh = session.0.id() as i32;
    let server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    let proc_start = MacOs.proc_start_string(sh).unwrap();
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    // The fake session quits; its server lives on.
    drop(session);
    assert!(wait_gone(sh, Duration::from_secs(3)));
    assert!(MacOs.proc_info(server.pid).is_some());
    let waiter = || {
        let mut c = sb.btp();
        c.args([
            "_sweep-later",
            &format!("--session={sh}:{proc_start}"),
            "--at=0",
        ])
        .timeout(Duration::from_secs(20));
        c
    };
    {
        let _lock = btp_core::watcher::acquire_watch_lock(sb.data.path(), "btp watch").unwrap();
        waiter().assert().success();
        assert!(
            MacOs.proc_info(server.pid).is_some(),
            "acted under a watcher"
        );
    }
    waiter().assert().success();
    assert!(wait_gone(server.pid, Duration::from_secs(3)), "not stopped");
    let n = sb.notified();
    assert!(
        n.contains(&format!("|Stopped Perl :{port}.")) || n.contains(&format!(":{port}.")),
        "{n}"
    );
    let recent = sb.btp().args(["recent", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&recent.get_output().stdout).unwrap();
    assert_eq!(v[0]["port"], port);
    assert_eq!(v[0]["reason"]["kind"], "dead_session_idle");
}

#[test]
fn clear_and_resume_kill_nothing() {
    for reason in ["clear", "resume"] {
        let sb = Sandbox::new();
        let port = free_port();
        let session = spawn_session(
            &sb,
            &perl_server(port, false),
            &format!("session-end --session-pid $$ --reason {reason}"),
        );
        let sh = session.0.id() as i32;
        let guard = wait_listener(sh, port);
        sb.session_file(sh, "conv-1");
        std::fs::write(sb.data.path().join("go"), "").unwrap();
        assert_eq!(run_result(&sb).0, 0);
        assert!(MacOs.proc_info(guard.pid).is_some(), "{reason} killed it");
        assert_eq!(sb.notified(), "");
        assert!(!sb.data.path().join("state.json").exists(), "{reason}");
    }
}

#[test]
fn note_from_the_owning_session_is_stored() {
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(
        &sb,
        &perl_server(port, false),
        &format!("note {port} --keep 'form demo'"),
    );
    let sh = session.0.id() as i32;
    let _server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.data.path().join("state.json")).unwrap())
            .unwrap();
    let note = &state["servers"][0]["note"];
    assert_eq!(note["keep"], true);
    assert_eq!(note["reason"], "form demo");
}

#[test]
fn note_from_outside_the_session_is_refused() {
    let sb = Sandbox::new();
    let port = free_port();
    // The session runs nothing but its server here; `note` comes from the
    // test process, which is not under it.
    let session = spawn_session(&sb, &perl_server(port, false), "--version");
    let sh = session.0.id() as i32;
    let _server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    sb.btp()
        .args(["list"])
        .assert()
        .success()
        .stdout(predicates::str::contains(port.to_string()));
    let out = sb
        .btp()
        .args(["note", &port.to_string(), "--done"])
        .assert()
        .code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("not run from a Claude Code session"), "{err}");
    let state = std::fs::read_to_string(sb.data.path().join("state.json")).unwrap();
    assert!(state.contains("\"note\": null"), "{state}");
}

#[test]
fn kill_by_key_from_outside_the_session_is_refused() {
    let sb = Sandbox::new();
    let port = free_port();
    let session = spawn_session(&sb, &perl_server(port, false), "--version");
    let sh = session.0.id() as i32;
    let server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    sb.btp().args(["list"]).assert().success();
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sb.data.path().join("state.json")).unwrap())
            .unwrap();
    let key = &state["servers"][0]["key"];
    let key = format!("{}:{}", key["pid"], key["start_time"]);
    let out = sb.btp().args(["kill", "--key", &key]).assert().code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("not run from a Claude Code session"), "{err}");
    assert!(MacOs.proc_info(server.pid).is_some(), "killed from outside");
}

#[test]
fn stale_outside_a_session_exits_1() {
    let sb = Sandbox::new();
    sb.btp()
        .args(["stale", "--conversation", "c"])
        .assert()
        .code(1);
}

#[test]
fn session_end_never_fails_even_on_bad_input() {
    let sb = Sandbox::new();
    sb.btp().args(["session-end"]).assert().code(0);
    sb.btp()
        .args(["session-end", "--session-pid", "x", "--reason", "other"])
        .assert()
        .code(0);
    // Not an ancestor of btp: refused, quietly.
    sb.btp()
        .args([
            "session-end",
            "--session-pid",
            "999999",
            "--reason",
            "other",
        ])
        .assert()
        .code(0)
        .stdout("");
    assert_eq!(sb.notified(), "");
    // Diagnostics are opt-in: no BTP_HOOK_DEBUG, no log.
    assert_eq!(sb.debug_log(), "");
}

#[test]
fn recent_plain_and_empty() {
    let sb = Sandbox::new();
    sb.btp()
        .arg("recent")
        .assert()
        .success()
        .stdout("no kills yet\n");
}
