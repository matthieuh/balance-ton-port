//! `btp run -- <cmd>`: starts a dev server after settling its port. A
//! server already running the same command in this folder is reused; a
//! taken port is freed (only what the idle rules allow), rerouted, asked
//! about, or reported (exit 3). The child's output goes to the terminal
//! unchanged and to its `btp run` log. A command btp does not recognize
//! runs untouched (still logged and linked). Status lines go to stderr,
//! prefixed `btp: `.

use crate::{new_watcher, warn_config};
use btp_core::config::data_dir;
use btp_core::labels::package_name;
use btp_core::model::{KillTarget, ServerKey};
use btp_core::platform::{MacOs, Platform};
use btp_core::run::command::{recognize, same_command_key, with_port};
use btp_core::run::conflict::{next_free_port, resolve, Holder, Resolution, RunFlags};
use btp_core::runlog::{self, RunLog};
use btp_core::state::read_state_readonly;
use btp_core::watcher::{now_ms, ServerView, Watcher};
use clap::Args;
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set for the child: a `btp run` inside it runs its command untouched.
const ACTIVE: &str = "BTP_RUN_ACTIVE";

/// Exit code for a port conflict btp did not resolve.
const CONFLICT: u8 = 3;

#[derive(Args)]
pub struct RunArgs {
    /// Free the port when its holder may be stopped (an idle server of an
    /// ended session), whatever `port_conflict` says
    #[arg(long)]
    free: bool,
    /// Never touch the holder of the port: run on another one
    #[arg(long)]
    reroute: bool,
    /// Skip the port check and reuse: run the command as it is
    #[arg(long)]
    no_check: bool,
    /// Start a second copy even if the same command already runs here
    #[arg(long)]
    new: bool,
    /// The command, after `--`
    #[arg(required = true, trailing_var_arg = true, value_name = "CMD")]
    cmd: Vec<String>,
}

/// What to start: the argv and the environment variables to add.
struct Start {
    argv: Vec<String>,
    env: Vec<(String, String)>,
}

/// The outcome of the check.
enum Outcome {
    Start(Start),
    Exit(u8),
    /// The reused server was stopped (`r`): check again, without reuse.
    Again,
}

/// `btp run`. Returns the exit code.
pub fn run(a: RunArgs) -> u8 {
    if std::env::var_os(ACTIVE).is_some() {
        return exec_untouched(&a.cmd);
    }
    let dir = data_dir();
    // A bug of btp before the start never stops the command: it runs as
    // it is, with one line saying so (the panic message itself is muted).
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let prepared = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| prepare(&a, &dir)));
    std::panic::set_hook(hook);
    let start = match prepared {
        Ok(Prepared::Start(s)) => s,
        Ok(Prepared::Exit(code)) => return code,
        Err(_) => {
            eprintln!("btp: port check skipped (internal error), running the command as it is");
            Start {
                argv: a.cmd.clone(),
                env: Vec::new(),
            }
        }
    };
    spawn_and_wait(&dir, start)
}

enum Prepared {
    Start(Start),
    Exit(u8),
}

/// Everything before the start: linking, log cleanup, the check.
fn prepare(a: &RunArgs, dir: &Path) -> Prepared {
    #[cfg(debug_assertions)]
    if std::env::var_os("BTP_TEST_PANIC_BEFORE_SPAWN").is_some() {
        panic!("test panic before spawn");
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut w = new_watcher(dir.to_path_buf());
    let me = std::process::id() as i32;
    // Read only: the answer holds in a read-only data dir too.
    let in_session = w.in_live_session(me);
    // Linking, keyed on this process: the child, and anything it starts
    // with `&`, descends from it. A failure only loses the link.
    let _ = w.register_run(me, now_ms());
    runlog::cleanup(dir, &|k| same_process(k), now_ms());
    let ctx = Ctx {
        dir: dir.to_path_buf(),
        cwd,
        agent: in_session || !(is_tty(0) && is_tty(2)),
        flags: RunFlags {
            free: a.free,
            reroute: a.reroute,
        },
    };
    let untouched = || Start {
        argv: a.cmd.clone(),
        env: Vec::new(),
    };
    if a.no_check {
        return Prepared::Start(untouched());
    }
    let mut skip_reuse = a.new;
    loop {
        match check(&mut w, &ctx, &a.cmd, skip_reuse) {
            Ok(Outcome::Start(s)) => return Prepared::Start(s),
            Ok(Outcome::Exit(code)) => return Prepared::Exit(code),
            Ok(Outcome::Again) => skip_reuse = true,
            Err(e) => {
                eprintln!("btp: port check skipped ({e}), running the command as it is");
                return Prepared::Start(untouched());
            }
        }
    }
}

struct Ctx {
    dir: PathBuf,
    cwd: PathBuf,
    /// No prompt, no following logs: from an agent session or without a TTY.
    agent: bool,
    flags: RunFlags,
}

/// The port check: reuse, then the holder of the intended port.
fn check(
    w: &mut Watcher<MacOs>,
    ctx: &Ctx,
    argv: &[String],
    skip_reuse: bool,
) -> io::Result<Outcome> {
    let env_port = std::env::var("PORT").ok();
    let untouched = || {
        Outcome::Start(Start {
            argv: argv.to_vec(),
            env: Vec::new(),
        })
    };
    let Some(dev) = recognize(argv, &ctx.cwd, env_port.as_deref()) else {
        return Ok(untouched());
    };
    // A port set with a value btp cannot read (`--port $PORT` in a script):
    // no check and no rewrite, as with `--no-check`.
    let Some(port) = dev.port else {
        return Ok(untouched());
    };
    let cfg = w.config();
    warn_config(w.config_error());
    let name = app_name(&ctx.cwd);
    let now = now_ms();

    if !skip_reuse {
        if let Some(key) = same_command_key(argv, &ctx.cwd) {
            if let Some(v) = w.find_reusable(&ctx.cwd, &key, now)? {
                return reuse(w, ctx, &name, &v);
            }
        }
    }

    let Some(holder) = w.port_holder(port, now)? else {
        eprintln!("btp: {name} on http://localhost:{}", port);
        return Ok(untouched());
    };
    let reroutable = dev.style.is_some();
    let who = holder_text(&holder);
    let mut res = resolve(&holder, reroutable, cfg.port_conflict, ctx.flags, &cfg, now);
    if let Resolution::Ask { can_kill } = res {
        if ctx.agent {
            eprintln!(
                "btp: port {} is used by {who}. Ask the user, then rerun with {}.",
                port,
                options(can_kill, reroutable)
            );
            return Ok(Outcome::Exit(CONFLICT));
        }
        res = match prompt(port, &who, can_kill, reroutable) {
            Choice::Kill => match &holder {
                Holder::Tracked { server, .. } => Resolution::Free(server.key),
                _ => Resolution::Block,
            },
            Choice::Other => Resolution::Reroute,
            Choice::Cancel => return Ok(Outcome::Exit(CONFLICT)),
        };
    }
    if let Resolution::Free(key) = res {
        let ended = session_ended_ago(&ctx.dir, &holder, now);
        match w.free_for_run(key, now)? {
            Some(_) => {
                if let Holder::Tracked {
                    server, idle_ms, ..
                } = &holder
                {
                    eprintln!(
                        "btp: freed :{} ({}, {ended}, idle {})",
                        port,
                        file_name(&server.worktree),
                        minutes(*idle_ms)
                    );
                }
                eprintln!("btp: {name} on http://localhost:{}", port);
                return Ok(untouched());
            }
            // No longer freeable (a Keep, a note, a connection meanwhile).
            None => {
                res = if reroutable {
                    Resolution::Reroute
                } else {
                    Resolution::Block
                }
            }
        }
    }
    if res == Resolution::Reroute {
        let cell = RefCell::new(&mut *w);
        let taken = |p: u16| {
            cell.borrow_mut()
                .port_holder(p, now)
                .map_or(true, |h| h.is_some())
        };
        if let Some(other) = next_free_port(port, &taken) {
            if let Some((argv, env)) = with_port(argv, &dev, other) {
                eprintln!("btp: {name} on http://localhost:{other} ({port} is used by {who})");
                return Ok(Outcome::Start(Start { argv, env }));
            }
        } else {
            eprintln!(
                "btp: port {} is used by {who}, and no free port was found above it.",
                port
            );
            return Ok(Outcome::Exit(CONFLICT));
        }
    }
    // Block: the port cannot change, or the holder could not be freed.
    if ctx.agent {
        eprintln!(
            "btp: port {} is used by {who}. Ask the user, then rerun with {}.",
            port,
            options(false, false)
        );
    } else {
        eprintln!(
            "btp: port {} is used by {who}, and btp cannot change the port of this command. Rerun it with a free port.",
            port
        );
    }
    Ok(Outcome::Exit(CONFLICT))
}

/// `--free, --reroute or a free port`, keeping only what would help.
fn options(can_kill: bool, reroutable: bool) -> String {
    let mut opts: Vec<&str> = Vec::new();
    if can_kill {
        opts.push("--free");
    }
    if reroutable {
        opts.push("--reroute");
    }
    match opts.len() {
        0 => "a free port".to_string(),
        _ => format!("{} or a free port", opts.join(", ")),
    }
}

/// The holder as the status lines name it.
pub(crate) fn holder_text(h: &Holder) -> String {
    match h {
        Holder::Tracked {
            server,
            obs,
            idle_ms,
            ..
        } => {
            let agent = server.agent.label();
            let session = if obs.session_alive {
                format!("{agent} session")
            } else {
                format!("{agent} session ended")
            };
            format!(
                "{} ({session}, idle {})",
                file_name(&server.worktree),
                minutes(*idle_ms)
            )
        }
        Holder::Observed {
            label,
            excluded: true,
        } => format!("{label} (excluded in btp's settings)"),
        Holder::Observed { label, .. } => format!("{label} (not started by an agent)"),
        Holder::Unknown => "an unknown process (btp cannot see it)".to_string(),
    }
}

/// `session ended 40 min ago` when btp saw that session end, else
/// `session ended`.
fn session_ended_ago(dir: &Path, h: &Holder, now: u64) -> String {
    let Holder::Tracked { server, .. } = h else {
        return "session ended".into();
    };
    let at = read_state_readonly(dir).ok().and_then(|s| {
        s.session_ended
            .iter()
            .filter(|g| {
                g.session_pid == server.session_pid
                    && g.session_proc_start == server.session_proc_start
            })
            .map(|g| g.at_ms)
            .max()
    });
    match at {
        Some(at) => format!("session ended {}", ago(now.saturating_sub(at))),
        None => "session ended".into(),
    }
}

/// `12 min` (whole minutes, `0 min` under one).
fn minutes(ms: u64) -> String {
    format!("{} min", ms / 60_000)
}

/// `just now`, `4 min ago`.
fn ago(ms: u64) -> String {
    if ms < 60_000 {
        "just now".into()
    } else {
        format!("{} min ago", ms / 60_000)
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// The app's name: its package.json `name`, else the folder's name.
fn app_name(cwd: &Path) -> String {
    std::fs::read_to_string(cwd.join("package.json"))
        .ok()
        .and_then(|t| package_name(&t))
        .unwrap_or_else(|| file_name(cwd))
}

/// A server already runs this command here. Agent: say so and exit 0.
/// Terminal: follow its log when it has one (`r` restarts it here).
fn reuse(w: &mut Watcher<MacOs>, ctx: &Ctx, name: &str, v: &ServerView) -> io::Result<Outcome> {
    let tracked = read_state_readonly(&ctx.dir)
        .map(|s| s.servers.iter().any(|t| t.key == v.key))
        .unwrap_or(false);
    let age = ago(now_ms().saturating_sub(v.started_ms));
    let who = if tracked {
        format!("started by {} {age}", v.agent.label())
    } else {
        format!("started {age}, not by an agent")
    };
    eprintln!(
        "btp: {name} already running on http://localhost:{} ({who}), reusing it",
        v.port
    );
    if ctx.agent || !v.has_logs {
        return Ok(Outcome::Exit(0));
    }
    eprintln!(
        "btp: Following its logs. Ctrl-C stops following; the server keeps running. Press r to restart it here."
    );
    let log_key = runlog::log_key_of(&MacOs, &ctx.dir, v.key).unwrap_or(v.key);
    let log = runlog::log_path(&ctx.dir, log_key);
    let mut offset = print_tail(&log);
    loop {
        match follow(&log, &mut offset, v.key) {
            Follow::Interrupted(sig) => {
                eprintln!(
                    "btp: stopped following; {name} keeps running on http://localhost:{}",
                    v.port
                );
                return Ok(Outcome::Exit(if sig == libc::SIGINT {
                    0
                } else {
                    128 + sig as u8
                }));
            }
            Follow::Gone => {
                eprintln!("btp: {name} on :{} has stopped", v.port);
                return Ok(Outcome::Exit(0));
            }
            Follow::Restart => {
                let stopped = if tracked {
                    w.kill(KillTarget::Key(v.key))?
                } else {
                    w.stop_observed(v.key)?
                };
                if stopped.is_some() || !same_process(v.key) {
                    eprintln!("btp: stopped it; starting {name} here");
                    return Ok(Outcome::Again);
                }
                eprintln!(
                    "btp: btp cannot stop {} (it is excluded in btp's settings); still following",
                    v.tool_label
                );
            }
        }
    }
}

/// Prints the last lines of the log; returns the offset to follow from.
fn print_tail(log: &Path) -> u64 {
    let len = std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(16 * 1024);
    if let Ok(bytes) = read_from(log, from) {
        // Start at a line start unless that is the file's start.
        let skip = if from > 0 {
            bytes.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1)
        } else {
            0
        };
        write_out(1, &bytes[skip..]);
    }
    len
}

fn read_from(path: &Path, offset: u64) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut out = Vec::new();
    f.read_to_end(&mut out)?;
    Ok(out)
}

enum Follow {
    /// A signal: stop following.
    Interrupted(i32),
    /// The server is gone.
    Gone,
    /// `r` was pressed.
    Restart,
}

/// Prints what the log gains until a key, a signal, or the server's end.
fn follow(log: &Path, offset: &mut u64, key: ServerKey) -> Follow {
    let _raw = RawMode::enter(0);
    let _signals = Signals::forwarded();
    let mut last_alive_check = Instant::now();
    loop {
        if let Some(sig) = take_signal() {
            return Follow::Interrupted(sig);
        }
        if let Some(k) = read_key(0) {
            if k == b'r' || k == b'R' {
                return Follow::Restart;
            }
        }
        let len = std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
        if len < *offset {
            // Compacted: its newest half is already shown.
            *offset = len;
        } else if len > *offset {
            if let Ok(bytes) = read_from(log, *offset) {
                write_out(1, &bytes);
                *offset += bytes.len() as u64;
            }
        }
        if last_alive_check.elapsed() > Duration::from_secs(1) {
            if !same_process(key) {
                return Follow::Gone;
            }
            last_alive_check = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

enum Choice {
    Kill,
    Other,
    Cancel,
}

/// `Port 3000 is used by admin (...). [k]ill it, [o]ther port, [c]ancel?`,
/// answered with one key, read in raw mode.
fn prompt(port: u16, who: &str, can_kill: bool, reroutable: bool) -> Choice {
    let mut opts = Vec::new();
    if can_kill {
        opts.push("[k]ill it");
    }
    if reroutable {
        opts.push("[o]ther port");
    }
    opts.push("[c]ancel");
    eprint!("Port {port} is used by {who}. {}? ", opts.join(", "));
    let _ = io::stderr().flush();
    let _raw = RawMode::enter(0);
    let _signals = Signals::forwarded();
    let choice = loop {
        if take_signal().is_some() {
            break Choice::Cancel;
        }
        match read_key(0) {
            Some(b'k' | b'K') if can_kill => break Choice::Kill,
            Some(b'o' | b'O') if reroutable => break Choice::Other,
            Some(b'c' | b'C' | b'q' | 0x1b | 0x03 | 0x04) => break Choice::Cancel,
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    eprintln!();
    choice
}

/// One byte from `fd` if one is waiting (`RawMode` makes reads return at
/// once). End of input cancels like `c`.
fn read_key(fd: i32) -> Option<u8> {
    let mut b = 0u8;
    // SAFETY: reading one byte into a local.
    let n = unsafe { libc::read(fd, (&mut b as *mut u8).cast(), 1) };
    (n == 1).then_some(b)
}

/// The terminal in raw-ish mode (no line buffering, no echo, signals kept)
/// while it lives; restored on drop. Nothing when `fd` is not a TTY.
struct RawMode {
    fd: i32,
    saved: Option<libc::termios>,
}

impl RawMode {
    fn enter(fd: i32) -> RawMode {
        // SAFETY: plain termios calls on an fd we do not own but only adjust.
        let saved = unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::isatty(fd) == 1 && libc::tcgetattr(fd, &mut t) == 0 {
                let saved = t;
                t.c_lflag &= !(libc::ICANON | libc::ECHO);
                t.c_cc[libc::VMIN] = 0;
                t.c_cc[libc::VTIME] = 0;
                (libc::tcsetattr(fd, libc::TCSANOW, &t) == 0).then_some(saved)
            } else {
                None
            }
        };
        RawMode { fd, saved }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(t) = self.saved {
            // SAFETY: restores the attributes read in `enter`.
            unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &t) };
        }
    }
}

/// The child to forward signals to (0: none yet or already reaped).
static CHILD: AtomicI32 = AtomicI32::new(0);
/// The last signal received and not yet handled (0: none).
static LAST_SIGNAL: AtomicI32 = AtomicI32::new(0);
/// The master of the child's pty, for SIGWINCH (-1: no pty).
static PTY_MASTER: AtomicI32 = AtomicI32::new(-1);

const FORWARDED: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

/// `_IOR('t', 104, struct winsize)` and `_IOW('t', 103, ...)` (sys/ttycom.h),
/// which the libc crate does not define for macOS.
const TIOCGWINSZ: libc::c_ulong = 0x4008_7468;
const TIOCSWINSZ: libc::c_ulong = 0x8008_7467;

/// How long the pipes are still read once the child has exited: something
/// it started in the background may hold them open forever.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(500);

/// Forwards the signal to `CHILD`, except one the terminal sent to its
/// whole foreground group, which the child (in btp's group) already got.
/// SIGWINCH first copies the terminal's size to the child's pty, then is
/// always forwarded: a pty with no session sends no SIGWINCH itself, and the
/// child may have read the old size on the terminal's own SIGWINCH.
/// Only async-signal-safe calls (atomics, kill, getpgrp, tcgetpgrp, ioctl).
extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let pid = CHILD.load(Ordering::SeqCst);
    if sig == libc::SIGWINCH {
        copy_window_size(PTY_MASTER.load(Ordering::SeqCst));
        if pid > 0 {
            // SAFETY: kill is async-signal-safe.
            unsafe { libc::kill(pid, sig) };
        }
        return;
    }
    // A signal sent by a process (`kill`, `sigqueue`) carries SI_USER or
    // SI_QUEUE. The terminal's has neither: on macOS its si_code is 0 but
    // si_pid is not (the process whose write to the tty raised it).
    // SAFETY: `info` is the kernel's siginfo for this delivery, or null.
    let from_terminal =
        !info.is_null() && unsafe { !matches!((*info).si_code, SI_USER | SI_QUEUE) };
    if pid > 0 && !(from_terminal && shares_foreground_group(pid)) {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
    LAST_SIGNAL.store(sig, Ordering::SeqCst);
}

/// `si_code` of a signal sent with `kill`, and with `sigqueue`
/// (sys/signal.h).
const SI_USER: libc::c_int = 0x10001;
const SI_QUEUE: libc::c_int = 0x10002;

/// btp and the child `pid` are in one process group, the foreground group
/// of one of btp's standard streams' terminal: the terminal's signal
/// reached them both. A child that moved to a group of its own did not get
/// it.
fn shares_foreground_group(pid: i32) -> bool {
    // SAFETY: getpgrp, getpgid and tcgetpgrp only read.
    unsafe {
        let me = libc::getpgrp();
        libc::getpgid(pid) == me && (0..3).any(|fd| libc::tcgetpgrp(fd) == me)
    }
}

/// Sets the pty `master` to the size of btp's terminal (stdout).
fn copy_window_size(master: i32) {
    if master < 0 {
        return;
    }
    // SAFETY: ioctl on fds we hold, into a local winsize.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, TIOCGWINSZ, &mut ws) == 0 {
            libc::ioctl(master, TIOCSWINSZ, &ws);
        }
    }
}

fn take_signal() -> Option<i32> {
    match LAST_SIGNAL.swap(0, Ordering::SeqCst) {
        0 => None,
        s => Some(s),
    }
}

/// `sigs` caught by `on_signal` while it lives; the previous handlers come
/// back on drop.
struct Signals {
    old: Vec<(i32, libc::sigaction)>,
}

impl Signals {
    fn install(sigs: &[i32]) -> Signals {
        LAST_SIGNAL.store(0, Ordering::SeqCst);
        let mut old = Vec::new();
        for &sig in sigs {
            // SAFETY: installs a handler that only does async-signal-safe
            // work.
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal
                    as extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void)
                    as usize;
                sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                let mut prev: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &sa, &mut prev) == 0 {
                    old.push((sig, prev));
                }
            }
        }
        Signals { old }
    }

    /// SIGINT, SIGTERM and SIGHUP.
    fn forwarded() -> Signals {
        Signals::install(&FORWARDED)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (sig, prev) in &self.old {
            // SAFETY: puts back the handler `install` replaced.
            unsafe { libc::sigaction(*sig, prev, std::ptr::null_mut()) };
        }
    }
}

fn is_tty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the fd.
    unsafe { libc::isatty(fd) == 1 }
}

/// A process with this pid and start time is running.
pub(crate) fn same_process(k: ServerKey) -> bool {
    MacOs
        .proc_info(k.pid)
        .is_some_and(|i| i.start_time == k.start_time)
}

/// How a write to btp's own output ended.
#[derive(PartialEq)]
enum Wrote {
    All,
    /// The reader is gone (EPIPE).
    Closed,
    /// Any other error: the rest of these bytes is dropped.
    Failed,
}

/// Writes all of `bytes` to `fd`, unbuffered, waiting while a non-blocking
/// `fd` is full (EAGAIN).
fn write_out(fd: i32, mut bytes: &[u8]) -> Wrote {
    while !bytes.is_empty() {
        // SAFETY: writes from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
            continue;
        }
        if n == 0 {
            // Nothing taken of a non-empty buffer: it would loop forever.
            return Wrote::Failed;
        }
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EAGAIN) => std::thread::sleep(Duration::from_millis(10)),
            Some(libc::EPIPE) => return Wrote::Closed,
            _ => return Wrote::Failed,
        }
    }
    Wrote::All
}

/// Nested `btp run`: becomes the command itself.
fn exec_untouched(cmd: &[String]) -> u8 {
    let err = Command::new(&cmd[0]).args(&cmd[1..]).exec();
    cannot_run(&cmd[0], &err)
}

fn cannot_run(program: &str, e: &io::Error) -> u8 {
    eprintln!("btp: cannot run {program}: {e}");
    if e.kind() == io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

/// A pty for the child's stdout and stderr, so it sees a terminal as it
/// would without btp. Output post-processing is off on it, so the bytes the
/// child writes reach the master unchanged (btp's terminal does its own).
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

fn open_pty() -> io::Result<Pty> {
    let (mut m, mut s) = (-1, -1);
    // SAFETY: openpty fills two fds; a winsize read from our own stdout.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        let wp = if libc::ioctl(1, TIOCGWINSZ, &mut ws) == 0 {
            &mut ws as *mut libc::winsize
        } else {
            std::ptr::null_mut()
        };
        if libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            wp,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: both fds were just opened and are owned here only.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    // SAFETY: fcntl and termios calls on the fds above.
    unsafe {
        libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(s, libc::F_SETFD, libc::FD_CLOEXEC);
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(s, &mut t) == 0 {
            t.c_oflag &= !libc::OPOST;
            libc::tcsetattr(s, libc::TCSANOW, &t);
        }
    }
    Ok(Pty { master, slave })
}

/// Starts the child and copies its output, byte for byte, to btp's own
/// stdout and stderr and to its log: through pipes (each stream to its
/// own), or, when btp's stdout is a terminal, through a pty for stdout
/// (and for stderr when it is that same terminal, else stderr is piped to
/// btp's stderr), so the child still sees a terminal.
/// stdin is the child's as it is. SIGINT, SIGTERM and SIGHUP are forwarded
/// (SIGWINCH too with a pty). Returns the child's exit code; when a signal
/// killed the child, btp dies of that same signal.
fn spawn_and_wait(dir: &Path, s: Start) -> u8 {
    let pty = if is_tty(1) { open_pty().ok() } else { None };
    // stderr shares the pty only when it is the same terminal as stdout:
    // `2>file` or `2>/dev/null` keeps going where it was sent.
    let stderr_on_pty = pty.is_some() && same_terminal(1, 2);
    let mut sigs = FORWARDED.to_vec();
    if pty.is_some() {
        sigs.push(libc::SIGWINCH);
    }
    let signals = Signals::install(&sigs);
    let mut cmd = Command::new(&s.argv[0]);
    cmd.args(&s.argv[1..])
        .envs(s.env)
        .env(ACTIVE, "1")
        .stdin(Stdio::inherit());
    match &pty {
        Some(p) => {
            match p.slave.try_clone() {
                Ok(o) => cmd.stdout(Stdio::from(o)),
                Err(_) => cmd.stdout(Stdio::piped()),
            };
            match p.slave.try_clone() {
                Ok(e) if stderr_on_pty => cmd.stderr(Stdio::from(e)),
                _ => cmd.stderr(Stdio::piped()),
            };
        }
        None => {
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
    }
    let spawned = cmd.spawn();
    // The slave copies the command holds: the child has its own now, and
    // the master reads end of file once the child's side is closed.
    drop(cmd);
    let pty = pty.map(|p| p.master);
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => return cannot_run(&s.argv[0], &e),
    };
    let pid = child.id() as i32;
    CHILD.store(pid, Ordering::SeqCst);
    // A signal that came between the spawn and now.
    if let Some(sig) = take_signal() {
        // SAFETY: our own unreaped child.
        unsafe { libc::kill(pid, sig) };
    }
    // The child's own start time names the log. A child already gone
    // cannot be read: its log is named with the time now.
    let start_time = MacOs
        .proc_info(pid)
        .map_or_else(|| now_ms() * 1000, |i| i.start_time);
    let log = RunLog::create(dir, ServerKey { pid, start_time }).ok();
    let log = Arc::new(Mutex::new(log));
    // (source, btp's fd, is a pty master)
    let mut sources: Vec<(OwnedFd, i32, bool)> = Vec::new();
    if let Some(o) = child.stdout.take() {
        sources.push((OwnedFd::from(o), 1, false));
    }
    if let Some(e) = child.stderr.take() {
        sources.push((OwnedFd::from(e), 2, false));
    }
    if let Some(m) = &pty {
        PTY_MASTER.store(m.as_raw_fd(), Ordering::SeqCst);
        if let Ok(dup) = m.try_clone() {
            sources.push((dup, 1, true));
        }
    }
    let exited = Arc::new(Mutex::new(None::<Instant>));
    let pumps: Vec<_> = sources
        .into_iter()
        .map(|(src, fd, is_pty)| {
            let log = log.clone();
            let exited = exited.clone();
            std::thread::spawn(move || pump(src, fd, is_pty, &log, &exited))
        })
        .collect();
    let status = loop {
        match child.wait() {
            Ok(s) => break Some(s),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break None,
        }
    };
    CHILD.store(0, Ordering::SeqCst);
    let _ = take_signal();
    // Each pump delivers all the child left, then waits for the end of its
    // stream at most `DRAIN_AFTER_EXIT` (see `pump`).
    if let Ok(mut e) = exited.lock() {
        *e = Some(Instant::now());
    }
    for p in pumps {
        let _ = p.join();
    }
    PTY_MASTER.store(-1, Ordering::SeqCst);
    drop(signals);
    match status {
        Some(s) => match (s.code(), s.signal()) {
            (Some(c), _) => c as u8,
            (None, Some(sig)) => die_of(sig),
            _ => 1,
        },
        None => 1,
    }
}

/// Ends btp with the signal that killed the child, so a shell sees the
/// same death. Returns 128 + `sig` if btp is still alive after that.
fn die_of(sig: i32) -> u8 {
    // SAFETY: back to the default action, unblocked, then raised.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, sig);
        libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(sig);
    }
    128 + sig as u8
}

/// Copies `src` to `fd` and to the log, byte for byte, as it arrives.
///
/// Once the child has exited (`exited` set), the pump first delivers in
/// full what was waiting in `src` at that moment (what the child left,
/// however slow the reader of `fd`), then keeps reading only until the end
/// of the stream or `DRAIN_AFTER_EXIT` after the exit, whichever comes
/// first: a server the child started in the background may hold the
/// stream as long as it runs. A write in progress is never cut.
///
/// When `fd` is a closed pipe (`btp run -- yes | head -1`), the pump stops
/// and closes `src`, so the child gets its SIGPIPE as without btp (not for
/// a pty, whose child would then block on a full buffer). Any other write
/// error drops that chunk and the pump goes on.
fn pump(
    src: OwnedFd,
    fd: i32,
    is_pty: bool,
    log: &Mutex<Option<RunLog>>,
    exited: &Mutex<Option<Instant>>,
) {
    let raw = src.as_raw_fd();
    let mut src = std::fs::File::from(src);
    let mut buf = vec![0u8; 64 * 1024];
    // Bytes still to read of what was waiting at the exit; `None` before.
    let mut left_at_exit: Option<usize> = None;
    loop {
        let exit = exited.lock().ok().and_then(|e| *e);
        if let Some(at) = exit {
            let left = *left_at_exit.get_or_insert_with(|| pending_bytes(raw));
            if left == 0 && at.elapsed() >= DRAIN_AFTER_EXIT {
                return;
            }
        }
        // Wait for data, waking up to notice the exit and the deadline.
        if !readable(raw, 100) {
            if exit.is_some() {
                left_at_exit = Some(0);
            }
            continue;
        }
        let n = match src.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // EIO from a pty master: the child's side is closed.
            Err(_) => return,
        };
        if let Some(left) = left_at_exit.as_mut() {
            *left = left.saturating_sub(n);
        }
        let wrote = write_out(fd, &buf[..n]);
        if let Ok(mut l) = log.lock() {
            if let Some(f) = l.as_mut() {
                // A log that fails stops being written; the output goes on.
                if f.write(&buf[..n]).is_err() {
                    *l = None;
                }
            }
        }
        if wrote == Wrote::Closed && !is_pty {
            return;
        }
    }
}

/// `fd` has data or its end (or an error) to read, within `ms`.
fn readable(fd: i32, ms: i32) -> bool {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: polls one fd we hold.
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

/// Bytes waiting to be read on `fd` (0 when it cannot tell). On macOS
/// FIONREAD reads 0 on a pty master whatever is waiting, so for the pty the
/// drain after the exit is governed by the 500 ms cap alone (still never
/// cutting a write in progress).
fn pending_bytes(fd: i32) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD into a local int.
    if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut n) } == 0 {
        n.max(0) as usize
    } else {
        0
    }
}

/// `a` and `b` are the same terminal device.
fn same_terminal(a: i32, b: i32) -> bool {
    if !is_tty(a) || !is_tty(b) {
        return false;
    }
    // SAFETY: fstat into locals.
    unsafe {
        let (mut x, mut y): (libc::stat, libc::stat) = (std::mem::zeroed(), std::mem::zeroed());
        libc::fstat(a, &mut x) == 0 && libc::fstat(b, &mut y) == 0 && x.st_rdev == y.st_rdev
    }
}
