//! `btp session-end`, `note`, `stale`, `recent` and `_finish-kill` against
//! real processes. Every process signalled here is one this test spawned: a
//! `sh` stands in for the Claude Code process (a session file names it), and
//! a `perl` one-liner is its dev server. `BTP_DATA_DIR` and `HOME` point to a
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
            .env_remove("CLAUDE_CONFIG_DIR");
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
    let mut c = std::process::Command::new("/bin/sh");
    c.arg("-c")
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

#[test]
fn session_end_stops_an_idle_server_and_hands_survivors_to_the_kill_stage() {
    let sb = Sandbox::new();
    let port = free_port();
    let mut session = spawn_session(
        &sb,
        &perl_server(port, true),
        "session-end --session-pid $$ --reason prompt_input_exit --session-id conv-1",
    );
    let sh = session.0.id() as i32;
    let server = wait_listener(sh, port);
    sb.session_file(sh, "conv-1");
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    let server = server.pid;
    let (code, ms) = run_result(&sb);
    assert_eq!(code, 0);
    assert!(ms < 1500, "session-end took {ms} ms");
    let out = std::fs::read_to_string(sb.data.path().join("out.txt")).unwrap();
    assert_eq!(out, "", "nothing printed");
    // SIGTERM is ignored: the detached stage SIGKILLs it after the grace.
    assert!(wait_gone(server, Duration::from_secs(6)), "server survived");
    let n = sb.notified();
    assert!(n.starts_with("Balance ton port|Session ended: "), "{n}");
    assert!(
        n.contains("|Stopped ") && n.contains(&format!(":{port}.")),
        "{n}"
    );
    let recent = sb.btp().args(["recent", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&recent.get_output().stdout).unwrap();
    assert_eq!(v[0]["port"], port);
    assert_eq!(v[0]["reason"]["kind"], "session_ended");
    // The fake session itself is never signalled: it exits on its own once
    // its server is gone.
    let status = session.0.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{status:?}");
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
    let server = guard.pid;
    sb.session_file(sh, "conv-1");
    let _tab = TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(sb.data.path().join("go"), "").unwrap();
    assert_eq!(run_result(&sb).0, 0);
    assert!(!wait_gone(server, Duration::from_millis(500)), "killed");
    let n = sb.notified();
    assert!(n.contains(&format!(":{port} (in use)")), "{n}");
    assert!(!n.contains("Stopped"), "{n}");
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
}

#[test]
fn finish_kill_kills_only_the_exact_process() {
    let sb = Sandbox::new();
    let mut c = std::process::Command::new("perl");
    c.args(["-e", "$SIG{TERM}=q(IGNORE); sleep 30"]);
    let mut child = Owned(c.spawn().unwrap());
    let pid = child.0.id() as i32;
    let start = MacOs.proc_info(pid).unwrap().start_time;
    // A wrong start time: never signalled.
    sb.btp()
        .args([
            "_finish-kill",
            "--deadline-ms",
            "0",
            &format!("{pid}:{}", start + 1),
        ])
        .assert()
        .success();
    assert!(MacOs.proc_info(pid).is_some());
    sb.btp()
        .args([
            "_finish-kill",
            "--deadline-ms",
            "0",
            &format!("{pid}:{start}"),
        ])
        .assert()
        .success();
    // Our own child: reap it to see it gone.
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "not killed");
        std::thread::sleep(Duration::from_millis(50));
    }
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
