use assert_cmd::Command;
use btp_core::config::{save_config, Config};
use btp_core::watcher::acquire_watch_lock;
use std::net::TcpListener;
use std::process::Stdio;

/// A temp data dir whose config adds an empty dir as extra Claude config dir.
/// `btp` runs with `HOME` set to the data dir (no `.claude*` inside) and no
/// `CLAUDE_CONFIG_DIR`, so automatic discovery finds nothing and no real
/// session is ever read.
fn sandbox() -> (tempfile::TempDir, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let cfg = Config {
        claude_config_dirs: vec![empty.path().to_path_buf()],
        ..Config::default()
    };
    save_config(data.path(), &cfg).unwrap();
    (data, empty)
}

fn btp(data: &tempfile::TempDir) -> Command {
    let mut c = Command::cargo_bin("btp").unwrap();
    c.env("BTP_DATA_DIR", data.path());
    c.env("HOME", data.path());
    c.env_remove("CLAUDE_CONFIG_DIR");
    c.env("BTP_TEST_NO_CODEX", "1");
    c
}

#[test]
fn config_set_then_get() {
    let (data, _e) = sandbox();
    btp(&data)
        .args(["config", "set", "alert_after_s", "120"])
        .assert()
        .success();
    let out = btp(&data).args(["config", "get"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("alert_after_s = 120"), "{text}");
    assert!(text.contains("exclude_ports = []"), "{text}");
    // Discovered dirs are never written; the read list is a comment.
    assert!(text.contains("# Session dirs read"), "{text}");
    // bare `config` behaves like `config get`
    btp(&data).arg("config").assert().success();
}

#[test]
fn config_set_port_conflict_then_get() {
    let (data, _e) = sandbox();
    btp(&data)
        .args(["config", "set", "port_conflict", "ask"])
        .assert()
        .success();
    let out = btp(&data).args(["config", "get"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("port_conflict = \"ask\""), "{text}");
    assert!(text.contains("port_check_hook = true"), "{text}");
    let out = btp(&data)
        .args(["config", "set", "port_conflict", "kill"])
        .assert()
        .code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("free_idle, ask, reroute"), "{err}");
}

#[test]
fn config_set_unknown_key_fails() {
    let (data, _e) = sandbox();
    btp(&data)
        .args(["config", "set", "nope", "1"])
        .assert()
        .code(1);
}

#[test]
fn config_set_below_minimum_fails() {
    let (data, _e) = sandbox();
    let out = btp(&data)
        .args(["config", "set", "dead_session_idle_s", "0"])
        .assert()
        .code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("at least 60"), "{err}");
}

#[test]
fn broken_config_is_reported_and_never_overwritten() {
    let (data, _e) = sandbox();
    let path = data.path().join("config.toml");
    std::fs::write(&path, "alert_after_s = \"soon\"\n").unwrap();
    let out = btp(&data)
        .args(["config", "set", "kill_grace_s", "3"])
        .assert()
        .code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("invalid"), "{err}");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "alert_after_s = \"soon\"\n"
    );
    btp(&data).args(["config", "get"]).assert().code(1);
    let out = btp(&data).args(["list", "--json"]).assert().success();
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("using defaults"), "{err}");
}

#[test]
fn sweep_dry_run_kills_nothing() {
    let (data, _e) = sandbox();
    let _listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    btp(&data).args(["sweep", "--dry-run"]).assert().success();
    assert!(child.try_wait().unwrap().is_none(), "child was killed");
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn kill_unknown_port_exits_1() {
    let (data, _e) = sandbox();
    btp(&data).args(["kill", "59999"]).assert().code(1);
    btp(&data)
        .args(["kill", "--pid", "999999"])
        .assert()
        .code(1);
}

/// A real child that listens on a free loopback port. Killed on drop, so a
/// failing assertion never leaks it.
struct ChildListener {
    child: std::process::Child,
    port: u16,
}

impl ChildListener {
    fn spawn() -> Self {
        use std::io::BufRead;
        let mut child = std::process::Command::new("/usr/bin/perl")
            .args([
                "-MIO::Socket::INET",
                "-e",
                "$s = IO::Socket::INET->new(Listen => 5, LocalAddr => '127.0.0.1', \
                 LocalPort => 0) or die; $| = 1; print $s->sockport, \"\\n\"; sleep 60",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut guard = ChildListener { child, port: 0 };
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .unwrap();
        guard.port = line.trim().parse().unwrap();
        guard
    }
}

impl Drop for ChildListener {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn kill_refuses_an_untracked_listener() {
    let (data, _e) = sandbox();
    let mut l = ChildListener::spawn();
    btp_isolated(&data)
        .args(["kill", &l.port.to_string()])
        .assert()
        .code(1);
    btp_isolated(&data)
        .args(["kill", "--pid", &l.child.id().to_string()])
        .assert()
        .code(1);
    assert!(l.child.try_wait().unwrap().is_none(), "child was killed");
}

#[test]
fn list_json_has_stable_shape() {
    let (data, _e) = sandbox();
    let out = btp(&data).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert!(v["servers"].is_array());
    assert!(v["recent_kills"].is_array());
}

#[test]
fn list_plain_prints_header() {
    let (data, _e) = sandbox();
    let out = btp(&data).arg("list").assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let header: Vec<&str> = text.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["PORT", "COMMAND", "WORKTREE", "STATE", "IDLE"]);
}

#[test]
fn watch_refuses_when_locked() {
    let (data, _e) = sandbox();
    let _lock = acquire_watch_lock(data.path(), "Balance ton port.app").unwrap();
    btp(&data)
        .arg("watch")
        .assert()
        .code(1)
        .stderr(predicates::str::contains(
            "Already watched by Balance ton port.app",
        ));
}

#[test]
fn list_json_recent_kill_reason_shape() {
    let (data, _e) = sandbox();
    std::fs::write(
        data.path().join("state.json"),
        r#"{"servers":[],"recent_kills":[
{"port":1,"command":"a","worktree":"/w","reason":{"kind":"manual"},"at_ms":1},
{"port":2,"command":"b","worktree":"/w","reason":{"kind":"worktree_deleted"},"at_ms":2},
{"port":3,"command":"c","worktree":"/w","reason":{"kind":"dead_session_idle","idle_ms":7},"at_ms":3}]}"#,
    )
    .unwrap();
    let out = btp(&data).args(["list", "--json"]).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    let k = &v["recent_kills"];
    assert_eq!(k[0]["reason"], serde_json::json!({"kind": "manual"}));
    assert_eq!(k[1]["reason"]["kind"], "worktree_deleted");
    assert_eq!(
        k[2]["reason"],
        serde_json::json!({"kind": "dead_session_idle", "idle_ms": 7})
    );
}

fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// `btp` isolated from any real agent: no Claude or Codex env, temp HOME.
fn btp_isolated(data: &tempfile::TempDir) -> Command {
    let mut c = btp(data);
    c.env_remove("CODEX_HOME");
    c.env_remove("CODEX_THREAD_ID");
    c
}

fn saved_server_state() -> &'static str {
    r#"{"servers":[{"key":{"pid":999999,"start_time":1},"port":4321,"command":"node vite","worktree":"/tmp/some-wt","session_id":"s","session_pid":1,"session_proc_start":"x","session_name":"demo","first_seen_ms":0,"last_active_ms":0,"kept":false,"alerted":false}]}"#
}

fn read_only(data: &tempfile::TempDir) -> impl Drop + '_ {
    use std::os::unix::fs::PermissionsExt;
    struct Restore<'a>(&'a std::path::Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o700));
        }
    }
    std::fs::write(data.path().join("state.json"), saved_server_state()).unwrap();
    std::fs::set_permissions(data.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    Restore(data.path())
}

#[test]
fn list_degrades_to_read_only_when_data_dir_not_writable() {
    if running_as_root() {
        return;
    }
    let (data, _e) = sandbox();
    let _g = read_only(&data);
    let out = btp_isolated(&data).arg("list").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        stdout.contains("4321") && stdout.contains("saved"),
        "{stdout}"
    );
    assert!(
        stderr.contains("btp: data dir is read-only here (sandbox?), showing the last saved state"),
        "{stderr}"
    );
    btp_isolated(&data)
        .args(["list", "--json"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read(data.path().join("state.json")).unwrap(),
        saved_server_state().as_bytes()
    );
}

#[test]
fn note_refuses_when_data_dir_not_writable() {
    if running_as_root() {
        return;
    }
    let (data, _e) = sandbox();
    let _g = read_only(&data);
    let out = btp_isolated(&data)
        .args(["note", "4321", "--done"])
        .assert()
        .code(1);
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(
        err.contains("btp: data dir is read-only here; run btp outside the sandbox"),
        "{err}"
    );
}

// ---- btp shell-init ----

fn shell_init_text(data: &tempfile::TempDir, shell: &str) -> String {
    let out = btp_isolated(data)
        .args(["shell-init", shell])
        .assert()
        .success();
    String::from_utf8(out.get_output().stdout.clone()).unwrap()
}

fn have(shell: &str) -> bool {
    std::process::Command::new(shell)
        .arg("-c")
        .arg("true")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn parses(shell: &str, text: &str) {
    if !have(shell) {
        return;
    }
    let out = std::process::Command::new(shell)
        .arg("-n")
        .arg("-c")
        .arg(text)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{shell} -n: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn shell_init_zsh_parses() {
    let (data, _e) = sandbox();
    let text = shell_init_text(&data, "zsh");
    assert!(text.contains("export BTP_SHELL_WRAP=1"), "{text}");
    for name in ["npm", "pnpm", "yarn", "bun", "npx", "next", "vite", "astro"] {
        assert!(text.contains(&format!("{name}() {{")), "{name}: {text}");
    }
    assert!(!text.contains("eval"), "the hint stays out of the output");
    parses("zsh", &text);
}

#[test]
fn shell_init_bash_parses() {
    let (data, _e) = sandbox();
    parses("bash", &shell_init_text(&data, "bash"));
}

#[test]
fn shell_init_unknown_shell_exits_2() {
    let (data, _e) = sandbox();
    btp_isolated(&data)
        .args(["shell-init", "fish"])
        .assert()
        .code(2);
}

#[test]
fn shell_init_help_has_the_eval_hint() {
    let (data, _e) = sandbox();
    let out = btp_isolated(&data)
        .args(["shell-init", "--help"])
        .assert()
        .success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("eval \"$(btp shell-init zsh)\""), "{text}");
}

#[test]
fn is_dev_exit_codes() {
    let (data, _e) = sandbox();
    let cwd = tempfile::tempdir().unwrap();
    btp_isolated(&data)
        .current_dir(cwd.path())
        .args(["_is-dev", "--", "vite"])
        .assert()
        .code(0);
    btp_isolated(&data)
        .current_dir(cwd.path())
        .args(["_is-dev", "--", "vite", "--port", "4000"])
        .assert()
        .code(0);
    btp_isolated(&data)
        .current_dir(cwd.path())
        .args(["_is-dev", "--", "ls", "-la"])
        .assert()
        .code(1);
}

/// A project with `"dev": "vite --port <free>"` and a fake `npm` that prints
/// its arguments and whether it runs inside `btp run`, exiting 7.
struct Wrapped {
    data: tempfile::TempDir,
    _empty: tempfile::TempDir,
    proj: tempfile::TempDir,
    bin: tempfile::TempDir,
}

fn wrapped() -> Wrapped {
    let (data, empty) = sandbox();
    let proj = tempfile::tempdir().unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        proj.path().join("package.json"),
        format!(r#"{{"name":"w","scripts":{{"dev":"vite --port {port}"}}}}"#),
    )
    .unwrap();
    let bin = tempfile::tempdir().unwrap();
    let fake = bin.path().join("npm");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho \"fake-npm args=$* active=${BTP_RUN_ACTIVE:-}\"\nexit 7\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    Wrapped {
        data,
        _empty: empty,
        proj,
        bin,
    }
}

/// Source the bash init, then run `line` in the project. Returns (code, stdout).
fn run_wrapped(w: &Wrapped, line: &str, extra_env: &[(&str, &str)]) -> (i32, String) {
    let init = shell_init_text(&w.data, "bash");
    let btp_dir = assert_cmd::cargo::cargo_bin("btp")
        .parent()
        .unwrap()
        .to_path_buf();
    let path = format!(
        "{}:{}:/usr/bin:/bin",
        w.bin.path().display(),
        btp_dir.display()
    );
    let mut c = std::process::Command::new("bash");
    c.arg("-c")
        .arg(format!("{init}\n{line}"))
        .current_dir(w.proj.path())
        .env_clear()
        .env("PATH", path)
        .env("HOME", w.data.path())
        .env("BTP_DATA_DIR", w.data.path())
        .env("BTP_TEST_NO_CODEX", "1");
    for (k, v) in extra_env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn wrapped_npm_test_passes_through() {
    let w = wrapped();
    let (code, out) = run_wrapped(&w, "npm test -- --watch=false", &[]);
    assert_eq!(code, 7, "{out}");
    assert_eq!(
        out.trim(),
        "fake-npm args=test -- --watch=false active=",
        "{out}"
    );
}

#[test]
fn wrapped_npm_run_dev_goes_through_btp_run() {
    let w = wrapped();
    let (code, out) = run_wrapped(&w, "npm run dev", &[]);
    assert_eq!(code, 7, "{out}");
    assert!(out.contains("active=1"), "{out}");
}

#[test]
fn btp_run_zero_disables() {
    let w = wrapped();
    let (code, out) = run_wrapped(&w, "npm run dev", &[("BTP_RUN", "0")]);
    assert_eq!(code, 7, "{out}");
    assert_eq!(out.trim(), "fake-npm args=run dev active=", "{out}");
}

#[test]
fn wrapper_falls_back_when_btp_is_not_on_path() {
    let w = wrapped();
    let init = shell_init_text(&w.data, "bash");
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(format!("{init}\nnpm run dev"))
        .current_dir(w.proj.path())
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", w.bin.path().display()))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "fake-npm args=run dev active="
    );
    assert!(out.stderr.is_empty());
}
