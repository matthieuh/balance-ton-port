//! `btp logs`: the output of a server started through `btp run`, read from
//! its run log (`btp_core::runlog`).

use crate::{new_watcher, now_ms, saved_view, Failure};
use btp_core::labels::tool_label;
use btp_core::model::ServerKey;
use btp_core::runlog;
use btp_core::state::{is_read_only_error, read_state_readonly};
use btp_core::worktree::worktree_root;
use clap::Args;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Lines of context around an error with `--errors`.
const ERROR_CONTEXT: usize = 2;

#[derive(Args)]
pub struct LogsArgs {
    /// Port of the server, or its name: tool (`Vite`), worktree or session
    /// name, case-insensitive
    target: String,
    /// Keep showing what the server prints until it stops or Ctrl-C
    #[arg(short = 'f', long)]
    follow: bool,
    /// Only the lines that look like errors, with 2 lines around them (while
    /// following: the matching lines only, without context)
    #[arg(long)]
    errors: bool,
    /// How many of the last lines to show
    #[arg(short = 'n', long, default_value_t = 200, value_name = "N")]
    lines: usize,
}

/// A server the target can name.
struct Candidate {
    key: ServerKey,
    ports: Vec<u16>,
    names: [String; 3],
}

impl Candidate {
    fn describe(&self) -> String {
        let [tool, worktree, session] = &self.names;
        let who = if session.is_empty() {
            worktree
        } else {
            session
        };
        format!(
            "  {}  {tool}  {who}",
            self.ports.first().copied().unwrap_or(0)
        )
    }

    fn answers_to(&self, target: &str) -> bool {
        match target.parse::<u16>() {
            Ok(port) => self.ports.contains(&port),
            Err(_) => {
                let t = target.to_lowercase();
                self.names
                    .iter()
                    .any(|n| !n.is_empty() && n.to_lowercase() == t)
            }
        }
    }
}

/// Every server `btp list` knows, tracked or observed.
fn candidates(dir: &Path) -> Result<Vec<Candidate>, Failure> {
    let pass = new_watcher(dir.to_path_buf())
        .with_observed(true)
        .observe(now_ms());
    let report = match pass {
        Ok(r) => r,
        Err(e) if is_read_only_error(&e) => {
            let saved = read_state_readonly(dir)?;
            let now = now_ms();
            btp_core::watcher::TickReport {
                servers: saved.servers.iter().map(|s| saved_view(s, now)).collect(),
                ..Default::default()
            }
        }
        Err(e) => return Err(e.into()),
    };
    let mut out: Vec<Candidate> = report
        .servers
        .iter()
        .map(|v| Candidate {
            key: v.key,
            ports: v.ports.clone(),
            names: [
                v.tool_label.clone(),
                v.worktree_name.clone(),
                v.session_name.clone(),
            ],
        })
        .collect();
    for o in &report.observed {
        let worktree = o
            .cwd
            .as_deref()
            .map(worktree_root)
            .and_then(|w| w.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        out.push(Candidate {
            key: o.key,
            ports: o.ports.clone(),
            names: [tool_label(&o.command), worktree, String::new()],
        });
    }
    Ok(out)
}

pub fn run(dir: &Path, a: LogsArgs) -> Result<u8, Failure> {
    let none = || {
        Failure::User(format!(
            "no logs for {} (it was not started through btp run)",
            a.target
        ))
    };
    let all = candidates(dir)?;
    let matches: Vec<&Candidate> = all.iter().filter(|c| c.answers_to(&a.target)).collect();
    let server = match matches.as_slice() {
        [] => return Err(none()),
        [one] => *one,
        many => {
            let list: Vec<String> = many.iter().map(|c| c.describe()).collect();
            return Err(Failure::User(format!(
                "{} matches several servers, give the port of one:\n{}",
                a.target,
                list.join("\n")
            )));
        }
    };
    if !runlog::exists(dir, server.key) {
        return Err(none());
    }
    let path = runlog::log_path(dir, server.key);
    // Read once: the tail and the offset to follow from agree.
    let bytes = std::fs::read(&path).map_err(|_| none())?;
    let offset = bytes.len() as u64;
    let lines = runlog::tail_of(&bytes, a.lines);
    let shown = if a.errors {
        runlog::errors(&lines, ERROR_CONTEXT)
    } else {
        lines
    };
    if !emit(&shown) {
        return Ok(0);
    }
    if a.follow {
        follow(&path, offset, server.key, a.errors);
    }
    Ok(0)
}

/// Prints `lines`; false when the reader is gone (`btp logs | head`).
fn emit(lines: &[String]) -> bool {
    let mut out = io::stdout().lock();
    let mut text = String::new();
    for l in lines {
        text.push_str(l);
        text.push('\n');
    }
    out.write_all(text.as_bytes())
        .and_then(|_| out.flush())
        .is_ok()
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Prints what the log gains until the server is gone and the file stops
/// growing, or until Ctrl-C / SIGTERM.
fn follow(path: &Path, mut offset: u64, key: ServerKey, errors_only: bool) {
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    // The part of the last line not yet ended by a line break.
    let mut partial: Vec<u8> = Vec::new();
    let mut last_alive_check = Instant::now();
    let mut alive = true;
    while !STOP.load(Ordering::SeqCst) {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let mut grew = false;
        if len < offset {
            // Compacted: its newest half is already shown.
            offset = len;
            partial.clear();
        } else if len > offset {
            if let Ok(bytes) = read_from(path, offset) {
                grew = !bytes.is_empty();
                offset += bytes.len() as u64;
                partial.extend_from_slice(&bytes);
                let mut ready = Vec::new();
                while let Some(i) = partial.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = partial.drain(..=i).collect();
                    ready.push(String::from_utf8_lossy(&line[..line.len() - 1]).into_owned());
                }
                if errors_only {
                    ready.retain(|l| !runlog::errors(std::slice::from_ref(l), 0).is_empty());
                }
                if !emit(&ready) {
                    return;
                }
            }
        }
        if !alive && !grew {
            break;
        }
        if alive && last_alive_check.elapsed() > Duration::from_millis(500) {
            last_alive_check = Instant::now();
            // One more pass after it is gone, for what it wrote last.
            alive = crate::run::same_process(key);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if !partial.is_empty() && !STOP.load(Ordering::SeqCst) {
        let line = String::from_utf8_lossy(&partial).into_owned();
        let shown = if errors_only {
            runlog::errors(std::slice::from_ref(&line), 0)
        } else {
            vec![line]
        };
        emit(&shown);
    }
}

fn read_from(path: &Path, offset: u64) -> io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut out = Vec::new();
    f.read_to_end(&mut out)?;
    Ok(out)
}
