//! `btp hook pre-bash` against real processes. Like `tests/run.rs`: temp
//! `BTP_DATA_DIR` and `HOME`, no agent config dirs, a python holder of the
//! port, and the debug-only `BTP_TEST_FRAMEWORK` entry so `tests/helpers/
//! serve.py` stands for a framework. Sockets are only ever touched in
//! python3 subprocesses, never in this process.

use assert_cmd::Command;
use btp_core::config::{save_config, Config, PortConflict};
use btp_core::platform::{MacOs, Platform};
use btp_core::watcher::acquire_watch_lock;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::Duration;

fn sandbox(f: impl FnOnce(&mut Config)) -> (tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let mut cfg = Config {
        claude_config_dirs: vec![empty.path().to_path_buf()],
        kill_grace_s: 1,
        ..Config::default()
    };
    f(&mut cfg);
    save_config(data.path(), &cfg).unwrap();
    (data, empty)
}

fn helper() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/helpers/serve.py")
}

/// `btp hook pre-bash` fed `stdin`.
fn hook(data: &Path, stdin: &str, wrap: bool) -> std::process::Output {
    hook_with(data, stdin, wrap, Some("serve.py:8000:long"))
}

/// `hook` with `framework` as `BTP_TEST_FRAMEWORK`, or with the real
/// recognition only (`None`).
fn hook_with(
    data: &Path,
    stdin: &str,
    wrap: bool,
    framework: Option<&str>,
) -> std::process::Output {
    let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("btp"));
    c.args(["hook", "pre-bash"])
        .env("BTP_DATA_DIR", data)
        .env("HOME", data)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("PORT")
        .env_remove("BTP_SHELL_WRAP")
        .env("BTP_TEST_NO_CODEX", "1")
        .env_remove("BTP_TEST_FRAMEWORK");
    if let Some(f) = framework {
        c.env("BTP_TEST_FRAMEWORK", f);
    }
    if wrap {
        c.env("BTP_SHELL_WRAP", "1");
    }
    let mut c = Command::from_std(c);
    c.timeout(Duration::from_secs(20)).write_stdin(stdin);
    c.output().unwrap()
}

fn payload(command: &str, cwd: &Path) -> String {
    serde_json::json!({
        "session_id": "s", "hook_event_name": "PreToolUse", "tool_name": "Bash",
        "tool_input": {"command": command}, "cwd": cwd,
    })
    .to_string()
}

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

/// A port with the next one free too, in a block of its own per call.
fn free_port() -> u16 {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);
    let base = 30_000 + (std::process::id() % 2_000) as u16 * 10;
    loop {
        let n = NEXT.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
        let p = 30_000 + (base - 30_000 + n) % 20_000;
        if bindable(&[p, p + 1]) {
            return p;
        }
    }
}

/// A process of its own listening on `port` until dropped.
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

    fn pid(&self) -> i32 {
        self.0.id() as i32
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn serve_cmd(port: u16) -> String {
    format!("python3 {} --port {port}", helper().display())
}

fn reason_of(o: &std::process::Output) -> String {
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["hookEventName"], "PreToolUse");
    assert_eq!(h["permissionDecision"], "deny");
    h["permissionDecisionReason"].as_str().unwrap().to_string()
}

#[test]
fn non_dev_command_silent() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    for c in ["ls -la", "npm test", "git status"] {
        let o = hook(data.path(), &payload(c, cwd.path()), false);
        assert!(o.status.success());
        assert_eq!(out(&o), "", "{c}");
    }
    // A non-Bash tool too.
    let p = serde_json::json!({"tool_name": "Read", "tool_input": {"command": serve_cmd(8000)}});
    assert_eq!(out(&hook(data.path(), &p.to_string(), false)), "");
}

#[test]
fn hook_disabled_silent() {
    let (data, _e) = sandbox(|c| c.port_check_hook = false);
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let _h = Holder::bind(p);
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), false);
    assert!(o.status.success());
    assert_eq!(out(&o), "");
}

#[test]
fn free_port_silent() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let o = hook(
        data.path(),
        &payload(&serve_cmd(free_port()), cwd.path()),
        false,
    );
    assert!(o.status.success());
    assert_eq!(out(&o), "");
}

#[test]
fn cd_and_env_stripped() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir(cwd.path().join("web")).unwrap();
    let p = free_port();
    let _h = Holder::bind(p);
    let cmd = format!("cd web && PORT={p} python3 {}", helper().display());
    let o = hook(data.path(), &payload(&cmd, cwd.path()), false);
    assert!(o.status.success());
    let r = reason_of(&o);
    assert!(r.starts_with(&format!("port {p} is used by ")), "{r}");
}

#[test]
fn blocks_with_message_when_taken() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let _h = Holder::bind(p);
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), false);
    assert!(o.status.success());
    let r = reason_of(&o);
    assert!(r.starts_with(&format!("port {p} is used by ")), "{r}");
    assert!(r.contains("not started by an agent"), "{r}");
    assert!(
        r.contains(&format!("Rerun as `btp run -- {}`", serve_cmd(p))),
        "{r}"
    );
    assert!(r.contains(&format!("or pass --port {}.", p + 1)), "{r}");
}

#[test]
fn ask_mode_tells_to_ask() {
    let (data, _e) = sandbox(|c| c.port_conflict = PortConflict::Ask);
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let _h = Holder::bind(p);
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), false);
    let r = reason_of(&o);
    // The holder is not freeable: the normal advice, nothing more.
    assert!(r.contains(&format!("or pass --port {}.", p + 1)), "{r}");
    assert!(!r.contains("Ask the user"), "{r}");
}

#[test]
fn wrap_env_never_blocks() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let _h = Holder::bind(p);
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), true);
    assert!(o.status.success());
    assert_eq!(out(&o), "");
}

#[test]
fn frees_dead_idle_holder() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut h = Holder::bind(p);
    let start_time = MacOs.proc_info(h.pid()).unwrap().start_time;
    // Tracked for a session that is gone (pid 1 never has this start), idle
    // for far more than 10 minutes.
    let state = serde_json::json!({"servers": [{
        "key": {"pid": h.pid(), "start_time": start_time},
        "port": p,
        "command": serve_cmd(p),
        "worktree": cwd.path(),
        "session_id": "s1",
        "session_pid": 1,
        "session_proc_start": "x",
        "session_name": "gone",
        "first_seen_ms": 1,
        "last_active_ms": 1,
        "kept": false,
        "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.path().join("state.json"), state.to_string()).unwrap();
    // A watcher runs: its passes keep the idle clock fresh.
    let _watcher = acquire_watch_lock(data.path(), "btp watch").unwrap();
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), false);
    assert!(o.status.success());
    assert_eq!(out(&o), "");
    // Freed synchronously: gone (reaped here) and the port bindable already.
    assert!(
        h.0.try_wait().unwrap().is_some(),
        "the holder is still alive"
    );
    assert!(bindable(&[p]));
}

/// The port is taken by an observed holder that stays untouched, and the
/// hook says nothing for `command`.
fn silent_with_taken_port(command_for: impl Fn(u16) -> String) {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut h = Holder::bind(p);
    let o = hook(data.path(), &payload(&command_for(p), cwd.path()), false);
    assert!(o.status.success());
    assert_eq!(out(&o), "");
    assert!(h.0.try_wait().unwrap().is_none(), "the holder was touched");
}

#[test]
fn heredoc_body_is_never_a_command() {
    silent_with_taken_port(|p| format!("cat > f.sh <<'EOF'\n{}\nEOF", serve_cmd(p)));
}

#[test]
fn comment_is_never_a_command() {
    silent_with_taken_port(|p| format!("ls # x; {}", serve_cmd(p)));
}

#[test]
fn quoted_dev_text_is_silent() {
    silent_with_taken_port(|p| format!("echo \"{}\"", serve_cmd(p)));
}

#[test]
fn heredoc_body_never_frees_a_holder() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut h = Holder::bind(p);
    let start_time = MacOs.proc_info(h.pid()).unwrap().start_time;
    let state = serde_json::json!({"servers": [{
        "key": {"pid": h.pid(), "start_time": start_time},
        "port": p, "command": serve_cmd(p), "worktree": cwd.path(),
        "session_id": "s1", "session_pid": 1, "session_proc_start": "x",
        "session_name": "gone", "first_seen_ms": 1, "last_active_ms": 1,
        "kept": false, "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.path().join("state.json"), state.to_string()).unwrap();
    // A watcher runs, so the holder would be freeable for a real command.
    let _watcher = acquire_watch_lock(data.path(), "btp watch").unwrap();
    let cmd = format!("cat > f.sh <<'EOF'\n{}\nEOF", serve_cmd(p));
    let o = hook(data.path(), &payload(&cmd, cwd.path()), false);
    assert_eq!(out(&o), "");
    assert!(h.0.try_wait().unwrap().is_none(), "the holder was killed");
}

#[test]
fn garbage_stdin_exit_0() {
    let (data, _e) = sandbox(|_| {});
    for s in ["", "not json", "{}", "{\"tool_name\":\"Bash\"}", "[1,2]"] {
        let o = hook(data.path(), s, false);
        assert!(o.status.success(), "{s}");
        assert_eq!(out(&o), "", "{s}");
    }
}

/// `state.json` tracking `h` on `p` for a session that is gone (pid 1 never
/// has this start), idle for far more than 10 minutes.
fn track_dead_idle(data: &Path, cwd: &Path, h: &Holder, p: u16) {
    let start_time = MacOs.proc_info(h.pid()).unwrap().start_time;
    let state = serde_json::json!({"servers": [{
        "key": {"pid": h.pid(), "start_time": start_time},
        "port": p, "command": serve_cmd(p), "worktree": cwd,
        "session_id": "s1", "session_pid": 1, "session_proc_start": "x",
        "session_name": "gone", "first_seen_ms": 1, "last_active_ms": 1,
        "kept": false, "alerted": false,
    }], "recent_kills": []});
    std::fs::write(data.join("state.json"), state.to_string()).unwrap();
}

#[test]
fn never_frees_without_a_watcher() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut h = Holder::bind(p);
    track_dead_idle(data.path(), cwd.path(), &h, p);
    // No watcher: the idle clock is as old as the last pass, so it proves
    // nothing. The command is denied with the usual advice instead.
    let o = hook(data.path(), &payload(&serve_cmd(p), cwd.path()), false);
    let r = reason_of(&o);
    assert!(r.starts_with(&format!("port {p} is used by ")), "{r}");
    assert!(r.contains(&format!("or pass --port {}.", p + 1)), "{r}");
    assert!(h.0.try_wait().unwrap().is_none(), "the holder was killed");
}

#[test]
fn a_framework_in_an_argument_is_not_a_dev_command() {
    let (data, _e) = sandbox(|_| {});
    let cwd = tempfile::tempdir().unwrap();
    let p = free_port();
    let mut h = Holder::bind(p);
    let real = |c: &str| hook_with(data.path(), &payload(c, cwd.path()), false, None);
    // The control: a real Vite command on the taken port is denied.
    assert!(reason_of(&real(&format!("vite --port {p}"))).starts_with(&format!("port {p} ")));
    for c in [
        format!("ls node_modules/.bin/vite --port {p}"),
        format!("rm node_modules/.bin/vite --port {p}"),
        format!("cat ./node_modules/.bin/next -p {p}"),
        format!("git add node_modules/.bin/vite --port {p}"),
        format!("npx rimraf node_modules/.bin/vite --port {p}"),
        format!("npx prettier --write node_modules/.bin/next -p {p}"),
    ] {
        let o = real(&c);
        assert!(o.status.success(), "{c}");
        assert_eq!(out(&o), "", "{c}");
    }
    assert!(h.0.try_wait().unwrap().is_none(), "the holder was touched");
}
