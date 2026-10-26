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
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut w = new_watcher(dir.clone());
    // Linking first, keyed on this process: the child, and anything it
    // starts with `&`, descends from it.
    let in_session = w
        .register_run(std::process::id() as i32, now_ms())
        .ok()
        .flatten()
        .is_some();
    runlog::cleanup(&dir, &|k| same_process(k), now_ms());
    let ctx = Ctx {
        dir,
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
    let start = if a.no_check {
        untouched()
    } else {
        let mut skip_reuse = a.new;
        loop {
            match check(&mut w, &ctx, &a.cmd, skip_reuse) {
                Ok(Outcome::Start(s)) => break s,
                Ok(Outcome::Exit(code)) => return code,
                Ok(Outcome::Again) => skip_reuse = true,
                Err(e) => {
                    eprintln!("btp: port check skipped ({e}), running the command as it is");
                    break untouched();
                }
            }
        }
    };
    spawn_and_wait(&ctx.dir, start)
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

    let Some(holder) = w.port_holder(dev.port, now)? else {
        eprintln!("btp: {name} on http://localhost:{}", dev.port);
        return Ok(untouched());
    };
    let reroutable = dev.style.is_some();
    let who = holder_text(&holder);
    let mut res = resolve(&holder, reroutable, cfg.port_conflict, ctx.flags, &cfg, now);
    if let Resolution::Ask { can_kill } = res {
        if ctx.agent {
            eprintln!(
                "btp: port {} is used by {who}. Ask the user, then rerun with {}.",
                dev.port,
                options(can_kill, reroutable)
            );
            return Ok(Outcome::Exit(CONFLICT));
        }
        res = match prompt(dev.port, &who, can_kill, reroutable) {
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
                        dev.port,
                        file_name(&server.worktree),
                        minutes(*idle_ms)
                    );
                }
                eprintln!("btp: {name} on http://localhost:{}", dev.port);
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
        if let Some(port) = next_free_port(dev.port, &taken) {
            if let Some((argv, env)) = with_port(argv, &dev, port) {
                eprintln!(
                    "btp: {name} on http://localhost:{port} ({} is used by {who})",
                    dev.port
                );
                return Ok(Outcome::Start(Start { argv, env }));
            }
        } else {
            eprintln!(
                "btp: port {} is used by {who}, and no free port was found above it.",
                dev.port
            );
            return Ok(Outcome::Exit(CONFLICT));
        }
    }
    // Block: the port cannot change, or the holder could not be freed.
    if ctx.agent {
        eprintln!(
            "btp: port {} is used by {who}. Ask the user, then rerun with {}.",
            dev.port,
            options(false, false)
        );
    } else {
        eprintln!(
            "btp: port {} is used by {who}, and btp cannot change the port of this command. Rerun it with a free port.",
            dev.port
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
fn holder_text(h: &Holder) -> String {
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
    let log = runlog::log_path(&ctx.dir, v.key);
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
    let _signals = Signals::install();
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
    let _signals = Signals::install();
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

const FORWARDED: [i32; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

extern "C" fn on_signal(sig: libc::c_int) {
    let pid = CHILD.load(Ordering::SeqCst);
    if pid > 0 {
        // SAFETY: kill is async-signal-safe.
        unsafe { libc::kill(pid, sig) };
    }
    LAST_SIGNAL.store(sig, Ordering::SeqCst);
}

fn take_signal() -> Option<i32> {
    match LAST_SIGNAL.swap(0, Ordering::SeqCst) {
        0 => None,
        s => Some(s),
    }
}

/// SIGINT, SIGTERM and SIGHUP caught (forwarded to `CHILD` when set) while
/// it lives; the previous handlers come back on drop.
struct Signals {
    old: Vec<(i32, libc::sigaction)>,
}

impl Signals {
    fn install() -> Signals {
        LAST_SIGNAL.store(0, Ordering::SeqCst);
        let mut old = Vec::new();
        for sig in FORWARDED {
            // SAFETY: installs a handler that only does async-signal-safe
            // work (an atomic load and store, kill).
            unsafe {
                let mut sa: libc::sigaction = std::mem::zeroed();
                sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
                sa.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut sa.sa_mask);
                let mut prev: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, &sa, &mut prev) == 0 {
                    old.push((sig, prev));
                }
            }
        }
        Signals { old }
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
fn same_process(k: ServerKey) -> bool {
    MacOs
        .proc_info(k.pid)
        .is_some_and(|i| i.start_time == k.start_time)
}

/// Writes all of `bytes` to `fd`, unbuffered. A closed terminal is ignored:
/// the child keeps running and its log keeps growing.
fn write_out(fd: i32, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // SAFETY: writes from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
        } else if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        } else {
            return;
        }
    }
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

/// Starts the child with its stdout and stderr piped to the terminal and to
/// its log, forwards SIGINT, SIGTERM and SIGHUP to it, and returns its exit
/// code (128 + the signal when a signal killed it).
fn spawn_and_wait(dir: &Path, s: Start) -> u8 {
    let signals = Signals::install();
    let mut cmd = Command::new(&s.argv[0]);
    cmd.args(&s.argv[1..])
        .envs(s.env)
        .env(ACTIVE, "1")
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
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
    let (done_tx, done) = std::sync::mpsc::channel::<()>();
    let pumps = [
        (
            child
                .stdout
                .take()
                .map(|o| Box::new(o) as Box<dyn Read + Send>),
            1,
        ),
        (
            child
                .stderr
                .take()
                .map(|e| Box::new(e) as Box<dyn Read + Send>),
            2,
        ),
    ];
    let mut open = 0;
    for (src, fd) in pumps {
        let Some(src) = src else { continue };
        open += 1;
        let log = log.clone();
        let tx = done_tx.clone();
        std::thread::spawn(move || {
            pump(src, fd, &log);
            let _ = tx.send(());
        });
    }
    drop(done_tx);
    let status = loop {
        match child.wait() {
            Ok(s) => break Some(s),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break None,
        }
    };
    CHILD.store(0, Ordering::SeqCst);
    let _ = take_signal();
    let code = match status {
        Some(s) => s
            .code()
            .map(|c| c as u8)
            .or_else(|| s.signal().map(|sig| 128 + sig as u8))
            .unwrap_or(1),
        None => 1,
    };
    // Drain what is left. Something the child started in the background may
    // hold the pipes: keep copying until it closes them, or until a signal
    // asks btp run to go.
    while open > 0 {
        match done.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => open -= 1,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if take_signal().is_some() {
                    break;
                }
            }
        }
    }
    drop(signals);
    code
}

/// Copies `src` to `fd` and to the log, byte for byte, as it arrives.
fn pump(mut src: Box<dyn Read + Send>, fd: i32, log: &Mutex<Option<RunLog>>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        write_out(fd, &buf[..n]);
        if let Ok(mut l) = log.lock() {
            if let Some(f) = l.as_mut() {
                // A log that fails stops being written; the output goes on.
                if f.write(&buf[..n]).is_err() {
                    *l = None;
                }
            }
        }
    }
}
