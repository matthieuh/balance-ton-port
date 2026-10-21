use assert_cmd::Command;
use btp_core::config::{save_config, Config};
use btp_core::watcher::acquire_watch_lock;
use std::net::TcpListener;
use std::process::Stdio;

/// A temp data dir whose config points `claude_config_dirs` at an empty dir,
/// so no real session is ever read.
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
    c
}

#[test]
fn config_set_then_get() {
    let (data, _e) = sandbox();
    btp(&data)
        .args(["config", "set", "alert_after_s", "42"])
        .assert()
        .success();
    let out = btp(&data).args(["config", "get"]).assert().success();
    let text = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    assert!(text.contains("alert_after_s = 42"), "{text}");
    // bare `config` behaves like `config get`
    btp(&data).arg("config").assert().success();
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
