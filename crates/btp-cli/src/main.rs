//! `btp`: list, kill and sweep the dev servers left behind by Claude Code
//! sessions. Exit codes: 0 success, 1 user error / nothing matched / lock
//! held, 2 internal error.

use btp_core::config::{data_dir, effective_claude_dirs, load_config, save_config};
use btp_core::display::{format_idle, server_state as state_label};
use btp_core::model::{KillReason, KillTarget};
use btp_core::platform::MacOs;
use btp_core::state::KillRecord;
use btp_core::watcher::{acquire_watch_lock, now_ms, ServerView, TickReport, Watcher};
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
            match Watcher::new(MacOs, dir).kill(target)? {
                Some(r) => {
                    println!("killed {}", describe_kill(&r));
                    Ok(0)
                }
                None => Err(Failure::User(match target {
                    KillTarget::Port(p) => format!("no tracked server on port {p}"),
                    KillTarget::Pid(p) => format!("no tracked server with pid {p}"),
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
        Cmd::Config { action } => {
            let mut cfg = load_config(&dir);
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
    let mut w = Watcher::new(MacOs, dir.clone());
    println!("watching (Ctrl-C to stop)");
    while !stop.load(Ordering::SeqCst) {
        match w.tick(now_ms()) {
            Ok(report) => {
                print!("{}", render_events(&report));
                let _ = io::Write::flush(&mut io::stdout());
            }
            Err(e) => eprintln!("btp: scan failed: {e}"),
        }
        let wait = load_config(&dir).scan_interval();
        let start = Instant::now();
        while start.elapsed() < wait && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(100).min(wait));
        }
    }
    Ok(0)
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
    fn list_has_header_and_rows() {
        let t = render_list(&[view(3000, true, 720_000)]);
        let lines: Vec<_> = t.lines().collect();
        assert_eq!(lines[0].split_whitespace().count(), 5);
        assert!(lines[1].starts_with("3000"));
        assert!(lines[1].ends_with("12 min"));
    }
}
