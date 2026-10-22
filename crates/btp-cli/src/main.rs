//! `btp`: list, kill and sweep the dev servers left behind by Claude Code
//! sessions. Exit codes: 0 success, 1 user error / nothing matched / lock
//! held, 2 internal error.

use btp_core::config::{data_dir, effective_claude_dirs, read_config, save_config};
use btp_core::display::format_idle as ago;
use btp_core::display::{format_idle, server_state as state_label};
use btp_core::model::{KillReason, KillTarget};
use btp_core::platform::MacOs;
use btp_core::state::{KillRecord, SessionEndedGroup};
use btp_core::watcher::{
    acquire_watch_lock, finish_kill, now_ms, EndBudget, NoteCmd, NoteOutcome, ServerView,
    TickReport, Watcher,
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
    },
    /// Claude Code SessionEnd hook: stop the idle servers of the session
    /// that quits. Prints nothing and always exits 0.
    SessionEnd(SessionEndArgs),
    /// Detached SIGKILL stage of `session-end` (internal)
    #[command(name = "_finish-kill", hide = true)]
    FinishKill {
        /// When to send SIGKILL, Unix milliseconds
        #[arg(long)]
        deadline_ms: u64,
        /// `<pid>:<start time>` pairs
        targets: Vec<String>,
    },
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("intent").required(true).args(["keep", "done"])))]
struct NoteArgs {
    /// Port the server listens on
    port: u16,
    /// Spare it when the session ends and stop alerts, with an optional reason
    #[arg(long, num_args = 0..=1, value_name = "REASON")]
    keep: Option<Option<String>>,
    /// You are done with it: stopped when the session ends, once idle
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
    #[arg(required_unless_present = "pid", conflicts_with = "pid")]
    port: Option<u16>,
    /// Process id of the server
    #[arg(long)]
    pid: Option<i32>,
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
        Cmd::FinishKill {
            deadline_ms,
            targets,
        } => {
            let targets = parse_targets(&targets);
            let wait =
                Duration::from_millis(deadline_ms.saturating_sub(now_ms())).min(MAX_FINISH_WAIT);
            finish_kill(&MacOs, &targets, Instant::now() + wait);
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
        Cmd::Stale { conversation } => {
            let mut w = Watcher::new(MacOs, dir);
            let out = w.stale(std::process::id() as i32, &conversation, now_ms())?;
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

/// SessionEnd budget: Claude Code gives the hook about 1.5 s in all, the
/// hook script and process start included. A pass takes tens of ms.
const SAMPLE_WINDOW: Duration = Duration::from_millis(300);
const SAMPLE_EVERY: Duration = Duration::from_millis(100);
/// From the start of `session-end` to the hand-off of the survivors.
const TERM_BUDGET: Duration = Duration::from_millis(900);
/// `_finish-kill` never waits longer than this, whatever it is given.
const MAX_FINISH_WAIT: Duration = Duration::from_secs(120);
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

/// `<pid>:<start time>` pairs; malformed ones, and pids <= 1, are dropped.
fn parse_targets(raw: &[String]) -> Vec<(i32, u64)> {
    raw.iter()
        .filter_map(|t| {
            let (pid, start) = t.split_once(':')?;
            let pid: i32 = pid.parse().ok()?;
            let start: u64 = start.parse().ok()?;
            (pid > 1).then_some((pid, start))
        })
        .collect()
}

/// At most this many servers are named per list in a notification.
const NAMED_MAX: usize = 4;

fn name_list(items: Vec<String>) -> String {
    let more = items.len().saturating_sub(NAMED_MAX);
    let shown = items
        .into_iter()
        .take(NAMED_MAX)
        .collect::<Vec<_>>()
        .join(", ");
    if more > 0 {
        format!("{shown} and {more} more")
    } else {
        shown
    }
}

/// The notification for a group: (subtitle, body).
fn notification_text(g: &SessionEndedGroup) -> (String, String) {
    let mut parts = Vec::new();
    if !g.stopped.is_empty() {
        let names = g
            .stopped
            .iter()
            .map(|s| format!("{} :{}", s.tool_label, s.port))
            .collect();
        parts.push(format!("Stopped {}.", name_list(names)));
    }
    if !g.still_open.is_empty() {
        let names = g
            .still_open
            .iter()
            .map(|s| {
                let why = if s.in_use { "in use" } else { "kept" };
                format!("{} :{} ({why})", s.tool_label, s.port)
            })
            .collect();
        parts.push(format!("Still open: {}.", name_list(names)));
    }
    let subtitle = if g.session_name.is_empty() {
        "Session ended".to_string()
    } else {
        format!("Session ended: {}", g.session_name)
    };
    (subtitle, parts.join(" "))
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
    let budget = EndBudget {
        sample_window: SAMPLE_WINDOW,
        sample_every: SAMPLE_EVERY,
        term_until: started + TERM_BUDGET,
    };
    let mut w = Watcher::new(MacOs, dir);
    let me = std::process::id() as i32;
    let report = match w.session_end(
        me,
        a.session_pid,
        a.session_id.as_deref(),
        now_ms(),
        &budget,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("btp: session-end: {e}");
            trace.push(format!("refused: {e}"));
            finish(&trace);
            return;
        }
    };
    trace.extend(report.trace.iter().cloned());
    // The SIGKILL stage first: it matters more than the message.
    if !report.survivors.is_empty() {
        let grace_ms = w.config().kill_grace_s.saturating_mul(1000);
        let deadline = now_ms().saturating_sub(started.elapsed().as_millis() as u64) + grace_ms;
        let spawned = std::env::current_exe().and_then(|exe| {
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("_finish-kill")
                .arg("--deadline-ms")
                .arg(deadline.to_string())
                .args(report.survivors.iter().map(|(p, s)| format!("{p}:{s}")));
            spawn_detached(cmd)
        });
        match spawned {
            Ok(()) => trace.push("SIGKILL stage detached".into()),
            Err(e) => {
                eprintln!("btp: could not start the SIGKILL stage: {e}");
                trace.push(format!("SIGKILL stage failed to start: {e}"));
            }
        }
    }
    match (&report.group, report.post_notification) {
        (Some(g), true) => {
            let (subtitle, body) = notification_text(g);
            notify(NOTIFY_TITLE, &subtitle, &body);
            trace.push(format!("notification posted: {body}"));
        }
        (Some(_), false) => trace.push("group left for the app to notify".into()),
        (None, _) => trace.push("nothing stopped or left open: no notification".into()),
    }
    trace.push(format!("done in {} ms", started.elapsed().as_millis()));
    finish(&trace);
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
        r.command,
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
    fn finish_kill_targets_parse_strictly() {
        let raw: Vec<String> = [
            "12:34", "bad", "1:5", "0:5", "-3:5", "7:x", "8:9:10", "40:41",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(parse_targets(&raw), vec![(12, 34), (40, 41)]);
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
    fn notification_names_stopped_and_still_open_servers() {
        let (sub, body) = notification_text(&group(
            &[("Vite", 5173), ("Astro", 4322)],
            &[("Expo (Metro)", 8081, true), ("Bun dev", 3000, false)],
        ));
        assert_eq!(sub, "Session ended: web-app");
        assert_eq!(
            body,
            "Stopped Vite :5173, Astro :4322. Still open: Expo (Metro) :8081 (in use), Bun dev :3000 (kept)."
        );
        let (_, body) = notification_text(&group(&[("Vite", 5173)], &[]));
        assert_eq!(body, "Stopped Vite :5173.");
        let (_, body) = notification_text(&group(&[], &[("Vite", 5173, true)]));
        assert_eq!(body, "Still open: Vite :5173 (in use).");
    }

    #[test]
    fn notification_caps_long_lists() {
        let many: Vec<(&str, u16)> = (0..6).map(|i| ("Vite", 5000 + i)).collect();
        let (_, body) = notification_text(&group(&many, &[]));
        assert_eq!(
            body,
            "Stopped Vite :5000, Vite :5001, Vite :5002, Vite :5003 and 2 more."
        );
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
            "12 min ago  :5173 vite (app) [session ended]\n2 h ago  :3000 vite (app) [manual]\n"
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
