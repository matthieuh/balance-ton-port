//! `btp`: list, kill and sweep the dev servers left behind by Claude Code
//! sessions. Exit codes: 0 success, 1 user error / nothing matched / lock
//! held, 2 internal error.

use btp_core::config::{data_dir, effective_claude_dirs, read_config, save_config};
use btp_core::display::format_idle as ago;
use btp_core::display::{format_idle, name_list, server_state as state_label, session_ended_body};
use btp_core::labels::tool_label;
use btp_core::model::{KillReason, KillTarget, ServerKey};
use btp_core::platform::MacOs;
use btp_core::state::{KillRecord, SessionEndedGroup};
use btp_core::watcher::{
    acquire_watch_lock, now_ms, NoteCmd, NoteOutcome, OwnKillOutcome, ServerView, TickReport,
    Watcher,
};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "btp",
    version,
    about = "Balance ton port: kill dev servers left behind by Claude Code"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the tracked dev servers
    List {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Kill a tracked server by port or by pid
    Kill(KillArgs),
    /// Kill every server whose worktree is gone or whose session is dead and idle
    Sweep {
        /// Print what would be killed, kill nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Watch in the foreground until Ctrl-C
    Watch,
    /// Show or change the configuration
    Config {
        #[command(subcommand)]
        action: Option<ConfigCmd>,
    },
    /// List the last kills
    Recent {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Tell btp your intent for a server of your own Claude Code session
    Note(NoteArgs),
    /// Servers of an earlier conversation of your session, idle more than
    /// 5 min (JSON; each is listed once)
    Stale {
        /// The current conversation id
        #[arg(long)]
        conversation: String,
        /// The Claude Code process (the hook's parent), used when its
        /// session file is not written yet
        #[arg(long)]
        session_pid: Option<i32>,
    },
    /// Claude Code SessionEnd hook: record the servers the quitting session
    /// leaves running (never stops any). Prints nothing and always exits 0.
    SessionEnd(SessionEndArgs),
    /// Detached waiter of `session-end`: applies the dead-session rule to
    /// that session's servers when no watcher runs (internal)
    #[command(name = "_sweep-later", hide = true)]
    SweepLater {
        /// `<pid>:<procStart>` of the ended session process
        #[arg(long)]
        session: String,
        /// When the first server becomes due, Unix milliseconds
        #[arg(long)]
        at: u64,
    },
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("intent").required(true).args(["keep", "done"])))]
struct NoteArgs {
    /// Port the server listens on
    port: u16,
    /// No idle alerts for it, with an optional reason
    #[arg(long, num_args = 0..=1, value_name = "REASON")]
    keep: Option<Option<String>>,
    /// You are done with it: stopped as soon as idle once your session has ended
    #[arg(long)]
    done: bool,
}

#[derive(Args)]
struct SessionEndArgs {
    /// The Claude Code process (the hook's parent)
    #[arg(long)]
    session_pid: i32,
    /// The SessionEnd `reason` from the hook payload
    #[arg(long)]
    reason: String,
    /// The conversation id from the hook payload
    #[arg(long)]
    session_id: Option<String>,
}

#[derive(Args)]
struct KillArgs {
    /// Port the server listens on
    #[arg(required_unless_present_any = ["pid", "key"], conflicts_with_all = ["pid", "key"])]
    port: Option<u16>,
    /// Process id of the server
    #[arg(long, conflicts_with = "key")]
    pid: Option<i32>,
    /// Exactly this process, `<pid>:<start time>`, and only a server of the
    /// caller's own session (what `btp stale` lists; for the plugin)
    #[arg(long, hide = true, value_parser = parse_key)]
    key: Option<ServerKey>,
}

fn parse_key(s: &str) -> Result<ServerKey, String> {
    let (pid, start) = s
        .split_once(':')
        .ok_or_else(|| "expected <pid>:<start time>".to_string())?;
    Ok(ServerKey {
        pid: pid.parse().map_err(|_| format!("bad pid: {pid}"))?,
        start_time: start
            .parse()
            .map_err(|_| format!("bad start time: {start}"))?,
    })
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the effective configuration as TOML
    Get,
    /// Set a numeric key
    Set { key: String, value: String },
}

#[derive(Serialize)]
struct ServerOut<'a> {
    #[serde(flatten)]
    view: &'a ServerView,
    /// `active`, `idle` or `session dead`, as in the plain output.
    state: &'static str,
}

#[derive(Serialize)]
struct ListOutput<'a> {
    servers: Vec<ServerOut<'a>>,
    recent_kills: &'a [KillRecord],
}

enum Failure {
    /// Exit 1.
    User(String),
    /// Exit 2.
    Internal(String),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::WouldBlock {
            Failure::User(e.to_string())
        } else {
            Failure::Internal(e.to_string())
        }
    }
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        // The SessionEnd hook never fails Claude Code's shutdown.
        Err(e) if std::env::args().nth(1).as_deref() == Some("session-end") => {
            eprintln!("btp: session-end: {}", e.kind());
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            let code = if e.use_stderr() { 1 } else { 0 };
            let _ = e.print();
            return ExitCode::from(code);
        }
    };
    match run(cli.command) {
        Ok(code) => ExitCode::from(code),
        Err(Failure::User(m)) => {
            eprintln!("btp: {m}");
            ExitCode::from(1)
        }
        Err(Failure::Internal(m)) => {
            eprintln!("btp: internal error: {m}");
            ExitCode::from(2)
        }
    }
}

fn run(cmd: Cmd) -> Result<u8, Failure> {
    let dir = data_dir();
    match cmd {
        Cmd::List { json } => {
            let report = Watcher::new(MacOs, dir.clone()).observe(now_ms())?;
            warn_config(report.config_error.as_deref());
            let kills = btp_core::state::with_state(&dir, |s| s.recent_kills.clone())?;
            if json {
                let out = ListOutput {
                    servers: report
                        .servers
                        .iter()
                        .map(|view| ServerOut {
                            view,
                            state: state_label(view),
                        })
                        .collect(),
                    recent_kills: &kills,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&out)
                        .map_err(|e| Failure::Internal(e.to_string()))?
                );
            } else {
                print!("{}", render_list(&report.servers));
            }
            Ok(0)
        }
        Cmd::Kill(KillArgs { key: Some(key), .. }) => {
            let mut w = Watcher::new(MacOs, dir);
            let out = w.kill_own(std::process::id() as i32, key)?;
            warn_config(w.config_error());
            match out {
                OwnKillOutcome::Killed(r) => {
                    println!("killed {}", describe_kill(&r));
                    Ok(0)
                }
                OwnKillOutcome::NoSession => Err(Failure::User(
                    "not run from a Claude Code session: --key only applies to your own session's servers".into(),
                )),
                OwnKillOutcome::NotTracked => Err(Failure::User(format!(
                    "no tracked server is process {}:{} (it exited or restarted): nothing stopped",
                    key.pid, key.start_time
                ))),
                OwnKillOutcome::OtherSession => Err(Failure::User(format!(
                    "process {} belongs to another Claude Code session: nothing stopped",
                    key.pid
                ))),
            }
        }
        Cmd::Kill(a) => {
            let target = match (a.port, a.pid) {
                (Some(p), None) => KillTarget::Port(p),
                (None, Some(p)) => KillTarget::Pid(p),
                _ => return Err(Failure::User("give a port or --pid <pid>".into())),
            };
            let mut w = Watcher::new(MacOs, dir);
            let killed = w.kill(target)?;
            warn_config(w.config_error());
            match killed {
                Some(r) => {
                    println!("killed {}", describe_kill(&r));
                    Ok(0)
                }
                None => Err(Failure::User(match target {
                    KillTarget::Port(p) => format!("no tracked server on port {p}"),
                    KillTarget::Pid(p) => format!("no tracked server with pid {p}"),
                    KillTarget::Key(k) => format!("no tracked server with pid {}", k.pid),
                })),
            }
        }
        Cmd::Sweep { dry_run } => {
            let mut w = Watcher::new(MacOs, dir);
            let report = if dry_run {
                w.observe(now_ms())?
            } else {
                w.sweep(now_ms())?
            };
            warn_config(report.config_error.as_deref());
            if report.killed.is_empty() {
                println!("nothing to kill");
            }
            let verb = if dry_run { "would kill" } else { "killed" };
            for r in &report.killed {
                println!("{verb} {}", describe_kill(r));
            }
            Ok(0)
        }
        Cmd::Watch => watch(dir),
        Cmd::SessionEnd(a) => {
            // Any internal error or panic still exits 0.
            let notify = notifier();
            if std::panic::catch_unwind(|| session_end(dir, &a, notify.as_ref(), hook_debug()))
                .is_err()
            {
                eprintln!("btp: session-end failed");
            }
            Ok(0)
        }
        Cmd::SweepLater { session, at } => {
            let Some((pid, proc_start)) = parse_session(&session) else {
                return Err(Failure::User(format!("bad --session {session:?}")));
            };
            let notify = notifier();
            sweep_later(dir, pid, &proc_start, at, notify.as_ref(), hook_debug());
            Ok(0)
        }
        Cmd::Note(a) => {
            let cmd = match (a.keep, a.done) {
                (Some(reason), false) => NoteCmd::Keep(reason),
                (None, true) => NoteCmd::Done,
                _ => return Err(Failure::User("give --keep [reason] or --done".into())),
            };
            let mut w = Watcher::new(MacOs, dir);
            let out = w.note(std::process::id() as i32, a.port, cmd.clone(), now_ms())?;
            warn_config(w.config_error());
            match out {
                NoteOutcome::Set { port, tool_label } => {
                    match cmd {
                        NoteCmd::Keep(_) => println!("noted: keep {tool_label} :{port}"),
                        NoteCmd::Done => println!("noted: done with {tool_label} :{port}"),
                    }
                    Ok(0)
                }
                NoteOutcome::NoSession => Err(Failure::User(
                    "not run from a Claude Code session: notes only apply to your own session's servers".into(),
                )),
                NoteOutcome::NotTracked => Err(Failure::User(format!(
                    "no tracked server on port {}",
                    a.port
                ))),
                NoteOutcome::OtherSession => Err(Failure::User(format!(
                    "port {} belongs to another Claude Code session: notes only apply to your own session's servers",
                    a.port
                ))),
            }
        }
        Cmd::Stale {
            conversation,
            session_pid,
        } => {
            let mut w = Watcher::new(MacOs, dir);
            let out = w.stale(
                std::process::id() as i32,
                session_pid,
                &conversation,
                now_ms(),
            )?;
            warn_config(w.config_error());
            let Some(list) = out else {
                return Err(Failure::User("not run from a Claude Code session".into()));
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&list)
                    .map_err(|e| Failure::Internal(e.to_string()))?
            );
            Ok(0)
        }
        Cmd::Recent { json } => {
            let kills = btp_core::state::with_state(&dir, |s| s.recent_kills.clone())?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&kills)
                        .map_err(|e| Failure::Internal(e.to_string()))?
                );
            } else {
                print!("{}", render_recent(&kills, now_ms()));
            }
            Ok(0)
        }
        Cmd::Config { action } => {
            // A broken file is reported, never shown or saved as defaults
            // (saving would erase what the user wrote).
            let mut cfg = read_config(&dir).map_err(|e| {
                Failure::User(format!(
                    "{e}; fix or delete it (the watcher keeps its last valid config)"
                ))
            })?;
            match action.unwrap_or(ConfigCmd::Get) {
                ConfigCmd::Get => {
                    let text = toml::to_string_pretty(&cfg)
                        .map_err(|e| Failure::Internal(e.to_string()))?;
                    print!("{text}");
                    // `claude_config_dirs` lists extras only; show what is read.
                    println!("# Session dirs read (found automatically, then extras):");
                    for d in effective_claude_dirs(&cfg) {
                        println!("#   {}", d.display());
                    }
                }
                ConfigCmd::Set { key, value } => {
                    cfg.set(&key, &value).map_err(Failure::User)?;
                    save_config(&dir, &cfg)?;
                    println!("{key} = {value}");
                }
            }
            Ok(0)
        }
    }
}

fn watch(dir: std::path::PathBuf) -> Result<u8, Failure> {
    let _lock = acquire_watch_lock(&dir, "btp watch")?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))
        .map_err(|e| Failure::Internal(e.to_string()))?;
    let mut w = Watcher::new(MacOs, dir);
    println!("watching (Ctrl-C to stop)");
    let mut last_config_error = None;
    while !stop.load(Ordering::SeqCst) {
        match w.tick(now_ms()) {
            Ok(report) => {
                if report.config_error != last_config_error {
                    warn_config(report.config_error.as_deref());
                    last_config_error = report.config_error.clone();
                }
                print!("{}", render_events(&report));
                let _ = io::Write::flush(&mut io::stdout());
                // Printed is delivered: no repeat on the next pass.
                for a in &report.alerts {
                    if let Err(e) = w.mark_alerted(a.key) {
                        eprintln!("btp: could not record the alert: {e}");
                    }
                }
            }
            Err(e) => eprintln!("btp: scan failed: {e}"),
        }
        let wait = w.config().scan_interval();
        let start = Instant::now();
        while start.elapsed() < wait && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100).min(wait));
        }
    }
    Ok(0)
}

/// The waiter wakes at least this often to record activity.
const WAITER_POLL: Duration = Duration::from_secs(60);
/// The waiter gives up this long after it started.
const WAITER_CAP_MS: u64 = 24 * 3_600_000;
const NOTIFY_TITLE: &str = "Balance ton port";

/// Posts a notification (title, subtitle, body).
type Notifier<'a> = dyn Fn(&str, &str, &str) + std::panic::RefUnwindSafe + 'a;

/// The real notifier is `osascript`, detached and not waited for. Tests set
/// `BTP_TEST_NOTIFY_FILE` to get one line per notification in that file
/// instead (osascript never runs then).
fn notifier() -> Box<Notifier<'static>> {
    if let Some(path) = std::env::var_os("BTP_TEST_NOTIFY_FILE") {
        return Box::new(move |title, subtitle, body| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(f, "{title}|{subtitle}|{body}");
            }
        });
    }
    Box::new(|title, subtitle, body| {
        // The text goes in as arguments, never into the script source.
        let mut cmd = std::process::Command::new("/usr/bin/osascript");
        cmd.args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 1 of argv) with title (item 2 of argv) subtitle (item 3 of argv)",
            "-e",
            "end run",
            body,
            title,
            subtitle,
        ]);
        if let Err(e) = spawn_detached(cmd) {
            eprintln!("btp: could not post the notification: {e}");
        }
    })
}

/// Runs `cmd` fully detached: double fork (the intermediate process exits
/// at once and is reaped here), new session, stdio to /dev/null. Returns as
/// soon as the grandchild has exec'd.
fn spawn_detached(mut cmd: std::process::Command) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls (setsid, fork, _exit) between
    // fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            match libc::fork() {
                -1 => Err(io::Error::last_os_error()),
                0 => Ok(()),
                _ => libc::_exit(0),
            }
        });
    }
    let mut child = cmd.spawn()?;
    child.wait()?;
    Ok(())
}

/// Claude Code's SessionEnd `reason` values that mean the process stops.
/// `clear` and `resume` keep the process (a new conversation); anything
/// unknown does nothing either (the normal rules still apply).
fn ends_process(reason: &str) -> bool {
    matches!(reason, "prompt_input_exit" | "other" | "logout")
}

/// `<pid>:<procStart>` (the start time contains colons too); pid > 1.
fn parse_session(raw: &str) -> Option<(i32, String)> {
    let (pid, start) = raw.split_once(':')?;
    let pid: i32 = pid.parse().ok()?;
    (pid > 1 && !start.is_empty()).then(|| (pid, start.to_string()))
}

/// The session-end notification: (subtitle, body), for the servers kept
/// running. `dead_idle_s` is the dead-session delay. The app posts the same
/// text (`session_ended_body`).
fn notification_text(g: &SessionEndedGroup, dead_idle_s: u64) -> (String, String) {
    (g.session_name.clone(), session_ended_body(g, dead_idle_s))
}

/// The waiter's notification for what it stopped: (subtitle, body).
fn stopped_text(kills: &[KillRecord]) -> (String, String) {
    let names = kills
        .iter()
        .map(|k| format!("{} :{}", tool_label(&k.command), k.port))
        .collect();
    (
        "Unused servers of an ended session".to_string(),
        format!("Stopped {}.", name_list(names)),
    )
}

/// One line per kill, newest first: `12 min ago  :5173 vite (app) [session ended]`.
fn render_recent(kills: &[KillRecord], now: u64) -> String {
    if kills.is_empty() {
        return "no kills yet\n".to_string();
    }
    kills
        .iter()
        .map(|k| {
            let age = ago(now.saturating_sub(k.at_ms));
            let age = if age.starts_with('<') {
                "just now".to_string()
            } else {
                format!("{age} ago")
            };
            format!("{age}  {}\n", describe_kill(k))
        })
        .collect()
}

/// `BTP_HOOK_DEBUG=1`: `btp session-end` appends a trace of what it found
/// and did to `<data dir>/session-end.log`. Off by default.
fn hook_debug() -> bool {
    std::env::var_os("BTP_HOOK_DEBUG").is_some_and(|v| v == "1")
}

/// Appends `lines` to `<dir>/session-end.log`, each stamped with the time
/// and this pid. Best effort.
fn debug_log(dir: &std::path::Path, lines: &[String]) {
    use std::io::Write;
    let _ = std::fs::create_dir_all(dir);
    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("session-end.log"))
    else {
        return;
    };
    let (now, pid) = (now_ms(), std::process::id());
    for l in lines {
        let _ = writeln!(f, "{now} btp[{pid}] {l}");
    }
}

fn session_end(dir: std::path::PathBuf, a: &SessionEndArgs, notify: &Notifier, debug: bool) {
    let mut trace = vec![format!(
        "session-end reason={} session_pid={} session_id={} parent={}",
        a.reason,
        a.session_pid,
        a.session_id.as_deref().unwrap_or("-"),
        std::os::unix::process::parent_id()
    )];
    let log_dir = dir.clone();
    let finish = |trace: &[String]| {
        if debug {
            debug_log(&log_dir, trace);
        }
    };
    if !ends_process(&a.reason) {
        trace.push("not a process stop: nothing to do".into());
        finish(&trace);
        return;
    }
    let started = Instant::now();
    let mut w = Watcher::new(MacOs, dir);
    let me = std::process::id() as i32;
    let report = match w.session_end(me, a.session_pid, a.session_id.as_deref(), now_ms()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("btp: session-end: {e}");
            trace.push(format!("refused: {e}"));
            finish(&trace);
            return;
        }
    };
    trace.extend(report.trace.iter().cloned());
    if let (Some(at), Some(g)) = (report.sweep_at, &report.group) {
        let spawned = std::env::current_exe().and_then(|exe| {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("_sweep-later")
                .arg(format!(
                    "--session={}:{}",
                    g.session_pid, g.session_proc_start
                ))
                .arg(format!("--at={at}"));
            spawn_detached(cmd)
        });
        match spawned {
            Ok(()) => trace.push(format!("waiter started for {at}")),
            Err(e) => {
                eprintln!("btp: could not start the waiter: {e}");
                trace.push(format!("waiter failed to start: {e}"));
            }
        }
    }
    match (&report.group, report.post_notification) {
        (Some(g), true) => {
            let (subtitle, body) = notification_text(g, w.config().dead_session_idle_s);
            notify(NOTIFY_TITLE, &subtitle, &body);
            trace.push(format!("notification posted: {body}"));
        }
        (Some(_), false) => trace.push("group left for the app to notify".into()),
        (None, _) => trace.push("no server kept running: no notification".into()),
    }
    trace.push(format!("done in {} ms", started.elapsed().as_millis()));
    finish(&trace);
}

/// Holds an exclusive lock on `path` for the life of the process (one
/// waiter per ended session). `None` when another process holds it.
fn exclusive(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    // SAFETY: a plain flock on an fd we own.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (r == 0).then_some(f)
}

/// `btp _sweep-later`: the dead-session rule for one ended session, when
/// no watcher runs. Quiet; errors end it.
fn sweep_later(
    dir: std::path::PathBuf,
    pid: i32,
    proc_start: &str,
    at: u64,
    notify: &Notifier,
    debug: bool,
) {
    let tag: String = proc_start
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let lock_path = dir.join(format!("sweep-later-{pid}-{tag}.lock"));
    let _ = std::fs::create_dir_all(&dir);
    let Some(_lock) = exclusive(&lock_path) else {
        if debug {
            debug_log(&dir, &[format!("waiter for {pid}: another one runs")]);
        }
        return;
    };
    let mut trace = vec![format!("waiter for {pid} ({proc_start}) first due at {at}")];
    let mut w = Watcher::new(MacOs, dir.clone());
    let cap = now_ms().saturating_add(WAITER_CAP_MS);
    let mut stopped = |kills: &[KillRecord]| {
        let (subtitle, body) = stopped_text(kills);
        notify(NOTIFY_TITLE, &subtitle, &body);
        if debug {
            debug_log(&dir, &[format!("waiter for {pid}: {body}")]);
        }
    };
    let end = w.sweep_later(
        pid,
        proc_start,
        at,
        cap,
        WAITER_POLL,
        &now_ms,
        &mut std::thread::sleep,
        &mut stopped,
    );
    trace.push(format!("waiter for {pid} ended: {end:?}"));
    if debug {
        debug_log(&dir, &trace);
    }
    let _ = std::fs::remove_file(&lock_path);
}

/// Reports a `config.toml` that could not be used (stderr).
fn warn_config(error: Option<&str>) {
    if let Some(e) = error {
        eprintln!("btp: {e}");
    }
}

/// One line per kill and per alert.
fn render_events(r: &TickReport) -> String {
    let mut out = String::new();
    for k in &r.killed {
        out.push_str(&format!("killed {}\n", describe_kill(k)));
    }
    for a in &r.alerts {
        out.push_str(&format!(
            "alert :{} {} ({}) idle {}\n",
            a.port,
            a.command,
            a.worktree_name,
            format_idle(a.idle_ms)
        ));
    }
    out
}

fn describe_kill(r: &KillRecord) -> String {
    let reason = match &r.reason {
        KillReason::WorktreeDeleted => "worktree deleted".to_string(),
        KillReason::DeadSessionIdle { idle_ms } => {
            format!("session dead, idle {}", format_idle(*idle_ms))
        }
        KillReason::Manual => "manual".to_string(),
        KillReason::SessionEnded => "session ended".to_string(),
        KillReason::Other => "other".to_string(),
    };
    format!(
        ":{} {} ({}) [{reason}]",
        r.port,
        tool_label(&r.command),
        r.worktree
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| r.worktree.display().to_string())
    )
}

/// Cuts to `max` characters with a trailing `...` (plain output only; JSON
/// keeps the full command).
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max - 3).collect();
        format!("{head}...")
    }
}

/// Aligned table, header `PORT  COMMAND  WORKTREE  STATE  IDLE`.
fn render_list(servers: &[ServerView]) -> String {
    let mut rows: Vec<[String; 5]> = vec![[
        "PORT".into(),
        "COMMAND".into(),
        "WORKTREE".into(),
        "STATE".into(),
        "IDLE".into(),
    ]];
    for s in servers {
        rows.push([
            s.port.to_string(),
            truncate(&s.command, 50),
            s.worktree_name.clone(),
            state_label(s).to_string(),
            format_idle(s.idle_ms),
        ]);
    }
    let mut widths = [0usize; 5];
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let mut out = String::new();
    for r in &rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use btp_core::model::ServerKey;

    fn view(port: u16, alive: bool, idle_ms: u64) -> ServerView {
        ServerView {
            key: ServerKey {
                pid: 1,
                start_time: 1,
            },
            port,
            ports: vec![port],
            command: "vite".into(),
            tool_label: "Vite".into(),
            worktree: "/repo/app".into(),
            worktree_name: "app".into(),
            location_label: "app".into(),
            session_name: "app-1d".into(),
            started_ms: 0,
            session_alive: alive,
            idle_ms,
            kill_in_ms: None,
        }
    }

    #[test]
    fn idle_formats() {
        assert_eq!(format_idle(5_000), "<1 min");
        assert_eq!(format_idle(12 * 60_000), "12 min");
        assert_eq!(format_idle(2 * 3_600_000), "2 h");
        assert_eq!(format_idle(3 * 86_400_000), "3 d");
    }

    #[test]
    fn truncates_long_commands() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcdefgh", 5), "ab...");
    }

    #[test]
    fn states() {
        assert_eq!(state_label(&view(1, false, 0)), "session dead");
        assert_eq!(state_label(&view(1, true, 1_000)), "active");
        assert_eq!(state_label(&view(1, true, 600_000)), "idle");
    }

    #[test]
    fn only_a_real_stop_ends_the_process() {
        for r in ["prompt_input_exit", "other", "logout"] {
            assert!(ends_process(r), "{r}");
        }
        for r in [
            "clear",
            "resume",
            "bypass_permissions_disabled",
            "",
            "OTHER",
        ] {
            assert!(!ends_process(r), "{r}");
        }
    }

    #[test]
    fn waiter_session_parses_strictly() {
        assert_eq!(
            parse_session("20:Thu Oct 22 10:00:00 2026"),
            Some((20, "Thu Oct 22 10:00:00 2026".to_string()))
        );
        for bad in ["", "20", "20:", "x:Thu", "1:Thu", "-4:Thu"] {
            assert_eq!(parse_session(bad), None, "{bad}");
        }
    }

    fn group(stopped: &[(&str, u16)], open: &[(&str, u16, bool)]) -> SessionEndedGroup {
        use btp_core::state::GroupServer;
        let key = ServerKey {
            pid: 9,
            start_time: 9,
        };
        SessionEndedGroup {
            session_pid: 1,
            session_proc_start: "x".into(),
            conversation_id: "c".into(),
            session_name: "web-app".into(),
            at_ms: 0,
            stopped: stopped
                .iter()
                .map(|&(l, port)| GroupServer {
                    key,
                    port,
                    tool_label: l.into(),
                    in_use: false,
                })
                .collect(),
            still_open: open
                .iter()
                .map(|&(l, port, in_use)| GroupServer {
                    key,
                    port,
                    tool_label: l.into(),
                    in_use,
                })
                .collect(),
            delivered: true,
        }
    }

    #[test]
    fn notification_names_the_servers_kept_running() {
        let (sub, body) = notification_text(
            &group(
                &[],
                &[("Expo (Metro)", 8081, true), ("Bun dev", 3000, false)],
            ),
            1200,
        );
        assert_eq!(sub, "web-app");
        assert_eq!(
            body,
            "Session ended: 2 servers kept running: Expo (Metro) :8081, Bun dev :3000. Stopped after 20 min unused, once no session works in that folder."
        );
        let (_, body) = notification_text(&group(&[], &[("Vite", 5173, false)]), 3600);
        assert_eq!(
            body,
            "Session ended: 1 server kept running: Vite :5173. Stopped after 1 h unused, once no session works in that folder."
        );
    }

    #[test]
    fn notification_caps_long_lists() {
        let many: Vec<(&str, u16, bool)> = (0..6).map(|i| ("Vite", 5000 + i, false)).collect();
        let (_, body) = notification_text(&group(&[], &many), 1200);
        assert!(
            body.contains(": Vite :5000, Vite :5001, Vite :5002, Vite :5003 and 2 more. Stopped"),
            "{body}"
        );
    }

    #[test]
    fn the_waiter_notification_names_what_it_stopped() {
        let k = |port, command: &str| KillRecord {
            port,
            command: command.into(),
            worktree: "/w".into(),
            reason: KillReason::DeadSessionIdle { idle_ms: 1 },
            at_ms: 0,
        };
        let (_, body) =
            stopped_text(&[k(5173, "node vite"), k(8790, "python3 -m http.server 8790")]);
        assert!(body.starts_with("Stopped Vite :5173, "), "{body}");
        assert!(body.ends_with(":8790."), "{body}");
    }

    #[test]
    fn recent_lists_kills_newest_first_with_their_age() {
        let rec = |port, reason, at_ms| KillRecord {
            port,
            command: "vite".into(),
            worktree: "/repo/app".into(),
            reason,
            at_ms,
        };
        let now = 1_000 * 60_000;
        let text = render_recent(
            &[
                rec(5173, KillReason::SessionEnded, now - 12 * 60_000),
                rec(3000, KillReason::Manual, now - 2 * 3_600_000),
            ],
            now,
        );
        assert_eq!(
            text,
            "12 min ago  :5173 Vite (app) [session ended]\n2 h ago  :3000 Vite (app) [manual]\n"
        );
        assert_eq!(render_recent(&[], now), "no kills yet\n");
    }

    #[test]
    fn clear_and_resume_do_nothing_at_all() {
        let d = tempfile::tempdir().unwrap();
        let called = std::sync::atomic::AtomicBool::new(false);
        let notify = |_: &str, _: &str, _: &str| {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        for reason in ["clear", "resume"] {
            let a = SessionEndArgs {
                session_pid: std::process::id() as i32,
                reason: reason.into(),
                session_id: None,
            };
            session_end(d.path().to_path_buf(), &a, &notify, false);
        }
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        // Nothing read or written: the data dir stays empty.
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_debug_trace_is_written_only_when_asked() {
        let d = tempfile::tempdir().unwrap();
        let notify = |_: &str, _: &str, _: &str| {};
        let a = SessionEndArgs {
            session_pid: 1,
            reason: "clear".into(),
            session_id: Some("conv-9".into()),
        };
        session_end(d.path().to_path_buf(), &a, &notify, false);
        assert!(!d.path().join("session-end.log").exists());
        session_end(d.path().to_path_buf(), &a, &notify, true);
        let log = std::fs::read_to_string(d.path().join("session-end.log")).unwrap();
        assert!(
            log.contains("reason=clear session_pid=1 session_id=conv-9"),
            "{log}"
        );
        assert!(log.contains("nothing to do"), "{log}");
    }

    #[test]
    fn a_session_pid_that_is_not_an_ancestor_does_nothing() {
        let d = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let called = std::sync::atomic::AtomicBool::new(false);
        let notify = |_: &str, _: &str, _: &str| {
            called.store(true, std::sync::atomic::Ordering::SeqCst);
        };
        let a = SessionEndArgs {
            session_pid: child.id() as i32,
            reason: "prompt_input_exit".into(),
            session_id: None,
        };
        session_end(d.path().to_path_buf(), &a, &notify, false);
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(child.try_wait().unwrap().is_none(), "never signalled");
        assert!(!d.path().join("state.json").exists());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn list_has_header_and_rows() {
        let t = render_list(&[view(3000, true, 720_000)]);
        let lines: Vec<_> = t.lines().collect();
        assert_eq!(lines[0].split_whitespace().count(), 5);
        assert!(lines[1].starts_with("3000"));
        assert!(lines[1].ends_with("12 min"));
    }
}
