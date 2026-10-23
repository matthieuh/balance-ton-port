//! Integration tests for the macOS platform layer, against real processes.
//!
//! The helper process is this very test binary, re-executed with
//! `BTP_TEST_HELPER` set: it then runs `zz_helper` only, which acts as a tiny
//! dev server (binds 127.0.0.1 and ::1 on one port, accepts and holds
//! connections). Every test signals only the helper it spawned, and the
//! `Helper` guard kills and reaps it on drop, so a failing assertion still
//! cleans up. A grandchild (tree mode) exits by itself once its parent dies.
#![cfg(target_os = "macos")]

use btp_core::platform::{ancestry, Listener, MacOs, Platform};
use std::io::{BufRead, BufReader};
use std::net::{Ipv6Addr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const HELPER_ENV: &str = "BTP_TEST_HELPER";

/// Spawned helper process. Killed (SIGKILL) and reaped on drop.
struct Helper {
    child: Child,
    port: u16,
    grandchild: Option<i32>,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Helper {
    fn pid(&self) -> i32 {
        self.child.id() as i32
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Only ever touches the child this test spawned.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_helper(mode: &str, cwd: &std::path::Path) -> Helper {
    spawn_helper_env(mode, cwd, &[])
}

fn spawn_helper_env(mode: &str, cwd: &std::path::Path, env: &[(&str, &str)]) -> Helper {
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["--exact", "zz_helper", "--nocapture", "--test-threads=1"])
        .env(HELPER_ENV, mode)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut port = 0;
    let mut grandchild = None;
    let mut line = String::new();
    while port == 0 {
        line.clear();
        if stdout.read_line(&mut line).unwrap() == 0 {
            panic!("helper exited before reporting its port");
        }
        // libtest prints "test zz_helper ... " before the marker on the same line.
        if let Some(at) = line.find("BTP_PORT=") {
            let rest = line[at + "BTP_PORT=".len()..].trim();
            let mut parts = rest.split(' ');
            port = parts.next().unwrap().parse().unwrap();
            grandchild = parts.next().map(|g| g.parse().unwrap());
        }
    }
    Helper {
        child,
        port,
        grandchild,
        _stdout: stdout,
    }
}

/// Not a real test: the body of the helper process. A no-op in normal runs.
#[test]
fn zz_helper() {
    let Ok(mode) = std::env::var(HELPER_ENV) else {
        return;
    };
    if mode == "orphan-watch" {
        // Grandchild: exit as soon as the parent helper is gone. The parent
        // pid comes from the env, so a parent that died before this line ran
        // is still noticed.
        let parent: i32 = std::env::var("BTP_TEST_PARENT").unwrap().parse().unwrap();
        loop {
            if unsafe { libc::getppid() } != parent {
                std::process::exit(0);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if mode == "connect" {
        let port: u16 = std::env::var("BTP_TEST_CONNECT").unwrap().parse().unwrap();
        let _c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let v4 = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = v4.local_addr().unwrap().port();
    // Same port on IPv6 too, to check that listeners() reports it once.
    let v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).unwrap();
    let mut grandchild = String::new();
    if mode == "tree" {
        let exe = std::env::current_exe().unwrap();
        let g = Command::new(exe)
            .args(["--exact", "zz_helper", "--test-threads=1"])
            .env(HELPER_ENV, "orphan-watch")
            .env("BTP_TEST_PARENT", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        grandchild = format!(" {}", g.id());
        std::mem::forget(g);
    }
    println!("BTP_PORT={port}{grandchild}");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in v6.incoming().flatten() {
            held.push(s);
        }
    });
    let mut held = Vec::new();
    for s in v4.incoming().flatten() {
        held.push(s);
    }
}

fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn finds_spawned_listener() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let want = Listener {
        pid: h.pid(),
        port: h.port,
    };
    let found: Vec<Listener> = MacOs
        .listeners()
        .into_iter()
        .filter(|l| l.pid == h.pid())
        .collect();
    assert_eq!(found, vec![want], "v4 + v6 on one port must appear once");
}

#[test]
fn proc_info_reads_ppid_cwd_command_and_start() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let info = MacOs.proc_info(h.pid()).expect("own child is inspectable");
    assert_eq!(info.pid, h.pid());
    assert_eq!(info.ppid, std::process::id() as i32);
    assert_eq!(
        info.cwd.map(|c| c.canonicalize().unwrap()),
        Some(dir.path().canonicalize().unwrap())
    );
    assert!(
        info.command.contains("zz_helper") && info.command.contains("--exact"),
        "command line with args, got {:?}",
        info.command
    );
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64;
    assert!(info.start_time <= now_us && now_us - info.start_time < 60_000_000);
}

#[test]
fn proc_info_of_missing_pid_is_none() {
    // pid_max on macOS is 99999.
    assert_eq!(MacOs.proc_info(999_999), None);
}

#[test]
fn ancestry_reaches_test_process() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let chain = ancestry(&MacOs, h.pid());
    let pids: Vec<i32> = chain.iter().map(|p| p.pid).collect();
    assert_eq!(pids[0], h.pid());
    assert_eq!(pids[1], std::process::id() as i32);
    // The walk goes up to launchd, which a non-root user cannot inspect
    // (skipped silently): the chain ends at pid 1 or at its child.
    let top = chain.last().unwrap();
    assert!(top.pid == 1 || top.ppid == 1, "chain {pids:?}");
    assert!(pids.len() <= 64);
}

#[test]
fn established_counts_a_connection() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    assert_eq!(MacOs.established(h.pid(), h.port), 0);
    let _c1 = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    let _c2 = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    assert!(
        wait_until(|| MacOs.established(h.pid(), h.port) == 2),
        "got {}",
        MacOs.established(h.pid(), h.port)
    );
    // The client sockets belong to this process, not the helper.
    assert_eq!(MacOs.established(h.pid(), h.port.wrapping_add(1)), 0);
}

#[test]
fn descendants_include_grandchild() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("tree", dir.path());
    let g = h.grandchild.expect("tree mode reports its child");
    assert_eq!(MacOs.descendants(h.pid()), vec![g]);
    let mine = MacOs.descendants(std::process::id() as i32);
    assert!(mine.contains(&h.pid()) && mine.contains(&g));
}

#[test]
fn signal_terminates_child() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = spawn_helper("listen", dir.path());
    assert!(MacOs.signal(h.pid(), 0), "alive check");
    assert!(MacOs.signal(h.pid(), libc::SIGTERM));
    let status = h.child.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(libc::SIGTERM));
    assert!(!MacOs.signal(h.pid(), 0), "reaped pid is gone");
}

#[test]
fn signal_refuses_non_positive_pids() {
    // kill(0, ..) and kill(-1, ..) would hit a whole group or every process.
    assert!(!MacOs.signal(0, 0));
    assert!(!MacOs.signal(-1, 0));
}

#[test]
fn proc_start_matches_ps() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    // Exactly what Claude Code runs to fill `procStart`.
    let out = Command::new("ps")
        .args(["-o", "lstart=", "-p", &h.pid().to_string()])
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .output()
        .unwrap();
    let ps = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert!(!ps.is_empty());
    assert_eq!(MacOs.proc_start_string(h.pid()), Some(ps));
}

/// `sleep` started with an empty argv[0] and a secret in its environment.
/// Killed and reaped on drop.
struct Sleeper(Child);

impl Drop for Sleeper {
    fn drop(&mut self) {
        // Only ever touches the child this test spawned.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn empty_argv0_keeps_the_environment_out_of_the_command() {
    use std::os::unix::process::CommandExt;
    let child = Command::new("/bin/sleep")
        .arg0("")
        .arg("30")
        .env("BTP_TEST_SECRET", "hunter2")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let s = Sleeper(child);
    let pid = s.0.id() as i32;
    let mut command = String::new();
    assert!(wait_until(|| {
        command = MacOs.proc_info(pid).map(|i| i.command).unwrap_or_default();
        command.contains("30")
    }));
    assert!(!command.contains("hunter2"), "env leaked: {command:?}");
    assert_eq!(command, " 30");
}

#[test]
fn proc_env_reads_a_child_env() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper_env("listen", dir.path(), &[("BTP_TEST_MARK", "42")]);
    let env = MacOs
        .proc_env(h.pid())
        .expect("own child's env is readable");
    assert!(
        env.contains(&("BTP_TEST_MARK".to_string(), "42".to_string())),
        "mark missing"
    );
    // pid 1 belongs to root: never readable by a regular user.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(MacOs.proc_env(1), None);
    }
}

#[test]
fn open_paths_sees_a_held_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("held.lock");
    let file = std::fs::File::create(&path).unwrap();
    assert!(file.try_lock().is_ok());
    let want = path.canonicalize().unwrap();
    let paths = MacOs.open_paths(std::process::id() as i32);
    assert!(
        paths.contains(&want),
        "{want:?} not in {} paths",
        paths.len()
    );
    assert!(MacOs.open_paths(999_999).is_empty());
}

#[test]
fn pids_named_finds_the_helper() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let info = MacOs.proc_info(h.pid()).unwrap();
    // comm is the executable name, truncated by the kernel to 15 bytes.
    let exe = std::env::current_exe().unwrap();
    let name = exe.file_name().unwrap().to_str().unwrap();
    let comm: String = name.chars().take(15).collect();
    assert!(info.command.contains("zz_helper"));
    assert!(MacOs.pids_named(&comm).contains(&h.pid()));
    assert!(MacOs.pids_named("btp-no-such-process").is_empty());
}

#[test]
#[ignore = "benchmark: cargo test -p btp-core --release --test platform_macos -- --ignored --nocapture"]
fn bench_full_scan() {
    let p = MacOs;
    let _ = p.listeners();
    let runs = 20;
    let mut best = Duration::MAX;
    let mut total = Duration::ZERO;
    let mut n = 0;
    for _ in 0..runs {
        let t = Instant::now();
        let ls = p.listeners();
        let infos = ls.iter().filter_map(|l| p.proc_info(l.pid)).count();
        let d = t.elapsed();
        n = infos;
        best = best.min(d);
        total += d;
    }
    println!(
        "full scan: {n} listeners, best {:?}, mean {:?} over {runs} runs",
        best,
        total / runs
    );
}

#[test]
fn proc_resources_of_self_is_nonzero() {
    let r = MacOs
        .proc_resources(std::process::id() as i32)
        .expect("own process is inspectable");
    assert!(r.rss_bytes > 0 && r.cpu_time_ns > 0, "{r:?}");
    assert_eq!(MacOs.proc_resources(999_999), None);
}

#[test]
fn cpu_time_grows_with_work() {
    let pid = std::process::id() as i32;
    let first = MacOs.proc_resources(pid).unwrap().cpu_time_ns;
    let start = Instant::now();
    let mut x = 0u64;
    while start.elapsed() < Duration::from_millis(50) {
        x = std::hint::black_box(x.wrapping_add(1));
    }
    let second = MacOs.proc_resources(pid).unwrap().cpu_time_ns;
    assert!(second > first, "{first} -> {second}");
}

#[test]
fn machine_memory_is_sane() {
    let m = MacOs.machine_memory().expect("memory readable");
    assert!(m.used_bytes > 0 && m.used_bytes <= m.total_bytes, "{m:?}");
    let mut value: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut value as *mut u64).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 0);
    assert_eq!(m.total_bytes, value);
}

#[test]
fn proc_argv_matches_command() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let argv = MacOs.proc_argv(h.pid()).expect("own child is inspectable");
    assert!(argv.iter().any(|a| a == "zz_helper"), "{argv:?}");
    assert_eq!(argv.join(" "), MacOs.proc_info(h.pid()).unwrap().command);
    assert_eq!(MacOs.proc_argv(999_999), None);
}

#[test]
fn exe_path_of_helper() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let path = MacOs.exe_path(h.pid()).expect("own child is inspectable");
    let me = std::env::current_exe().unwrap();
    assert_eq!(path.file_name(), me.file_name());
    assert_eq!(MacOs.exe_path(999_999), None);
}

#[test]
fn established_excluding_ignores_connections_from_listed_pid() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let _c1 = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    let _c2 = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    assert!(wait_until(|| MacOs.established(h.pid(), h.port) == 2));
    let me = std::process::id() as i32;
    assert_eq!(MacOs.established_excluding(h.pid(), h.port, &[me]), 0);
    assert_eq!(MacOs.established_excluding(h.pid(), h.port, &[999_999]), 2);
    assert_eq!(MacOs.established_excluding(h.pid(), h.port, &[]), 2);
}

#[test]
fn established_excluding_handles_v4_and_v6_clients() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let me = std::process::id() as i32;
    let _c4 = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    assert!(wait_until(|| MacOs.established(h.pid(), h.port) == 1));
    assert_eq!(MacOs.established_excluding(h.pid(), h.port, &[me]), 0);
    let _c6 = TcpStream::connect((Ipv6Addr::LOCALHOST, h.port)).unwrap();
    assert!(wait_until(|| MacOs.established(h.pid(), h.port) == 2));
    assert_eq!(MacOs.established_excluding(h.pid(), h.port, &[me]), 0);
}

#[test]
fn established_excluding_keeps_connections_from_other_processes() {
    let dir = tempfile::tempdir().unwrap();
    let h = spawn_helper("listen", dir.path());
    let me = std::process::id() as i32;
    let _mine = TcpStream::connect(("127.0.0.1", h.port)).unwrap();
    // A second process that connects and holds the connection.
    let mut other = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "zz_helper", "--nocapture", "--test-threads=1"])
        .env(HELPER_ENV, "connect")
        .env("BTP_TEST_CONNECT", h.port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let ok = wait_until(|| MacOs.established(h.pid(), h.port) == 2);
    let excluding = MacOs.established_excluding(h.pid(), h.port, &[me]);
    let both = MacOs.established_excluding(h.pid(), h.port, &[me, other.id() as i32]);
    let _ = other.kill();
    let _ = other.wait();
    assert!(ok);
    assert_eq!(excluding, 1);
    assert_eq!(both, 0);
}
