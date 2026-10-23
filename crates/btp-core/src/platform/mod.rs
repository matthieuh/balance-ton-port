//! OS access behind a trait: listening ports, process info, connection
//! counts, process trees and signals. `MacOs` reads them through libproc
//! (no subprocess); `FakePlatform` backs the core's tests.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacOs;

/// A process listening on a TCP port (IPv4 or IPv6, reported once).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Listener {
    pub pid: i32,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    /// Process start time, microseconds since the Unix epoch.
    pub start_time: u64,
    pub cwd: Option<PathBuf>,
    /// Full command line (argv joined by spaces), or the process name when
    /// the arguments cannot be read.
    pub command: String,
}

pub trait Platform {
    /// TCP listeners of every inspectable process, one entry per pid + port.
    fn listeners(&self) -> Vec<Listener>;
    /// `None` when the process is gone or cannot be inspected.
    fn proc_info(&self, pid: i32) -> Option<ProcInfo>;
    /// Established TCP connections held by `pid` whose local port is `port`.
    fn established(&self, pid: i32, port: u16) -> u32;
    /// Every descendant of `pid` (children first), `pid` excluded.
    fn descendants(&self, pid: i32) -> Vec<i32>;
    /// Sends `sig` to `pid`; `true` when delivered. Never targets pid <= 0.
    fn signal(&self, pid: i32, sig: i32) -> bool;
    /// Start time formatted like Claude Code's `procStart`.
    fn proc_start_string(&self, pid: i32) -> Option<String>;
    /// Environment of one of the current user's processes, `None` when
    /// unreadable or owned by someone else. Entries without `=` are skipped.
    fn proc_env(&self, pid: i32) -> Option<Vec<(String, String)>>;
    /// Paths of the vnode file descriptors `pid` holds open.
    fn open_paths(&self, pid: i32) -> Vec<PathBuf>;
    /// Current-user pids whose process name equals `name`.
    fn pids_named(&self, name: &str) -> Vec<i32>;
}

const MAX_ANCESTRY: usize = 64;

/// `pid`, its parent, and so on up to pid 1. Stops at the first process that
/// cannot be inspected, on a cycle, or after 64 entries.
pub fn ancestry(p: &dyn Platform, pid: i32) -> Vec<ProcInfo> {
    let mut chain: Vec<ProcInfo> = Vec::new();
    let mut next = pid;
    while chain.len() < MAX_ANCESTRY && next > 0 {
        if chain.iter().any(|c| c.pid == next) {
            break;
        }
        let Some(info) = p.proc_info(next) else {
            break;
        };
        next = if info.pid == 1 { 0 } else { info.ppid };
        chain.push(info);
    }
    chain
}

/// Formats a start time (microseconds since the epoch) exactly like Claude
/// Code's `procStart`, which is `LC_ALL=C TZ=UTC ps -o lstart= -p <pid>`
/// trimmed: `%a %b %e %H:%M:%S %Y` in UTC, day of month space-padded
/// (`"Thu Oct  1 09:06:45 2026"`).
pub fn format_proc_start(start_time_us: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = start_time_us / 1_000_000;
    let days = secs / 86_400;
    let tod = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        day,
        tod / 3600,
        tod / 60 % 60,
        tod % 60,
        year
    )
}

#[cfg(any(test, feature = "fake"))]
pub use fake::FakePlatform;

#[cfg(any(test, feature = "fake"))]
mod fake {
    use super::{format_proc_start, Listener, Platform, ProcInfo};
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct State {
        procs: BTreeMap<i32, ProcInfo>,
        listeners: Vec<Listener>,
        established: HashMap<(i32, u16), u32>,
        ignores_sigterm: HashSet<i32>,
        fails_signals: HashSet<i32>,
        signals: Vec<(i32, i32)>,
        env: HashMap<i32, Vec<(String, String)>>,
        open_paths: HashMap<i32, Vec<PathBuf>>,
    }

    /// In-memory platform for tests. SIGTERM and SIGKILL remove a process
    /// (and its listeners) unless it was told to ignore SIGTERM; signal 0
    /// only checks existence. Every signal sent is recorded.
    #[derive(Default)]
    pub struct FakePlatform {
        state: Mutex<State>,
    }

    impl FakePlatform {
        pub fn new() -> Self {
            Self::default()
        }

        fn st(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(|e| e.into_inner())
        }

        pub fn add_proc(&self, info: ProcInfo) {
            self.st().procs.insert(info.pid, info);
        }

        /// Shorthand for `add_proc` with a command and cwd.
        pub fn spawn(&self, pid: i32, ppid: i32, start_time: u64, command: &str, cwd: &str) {
            self.add_proc(ProcInfo {
                pid,
                ppid,
                start_time,
                cwd: Some(cwd.into()),
                command: command.to_string(),
            });
        }

        pub fn remove_proc(&self, pid: i32) {
            let mut st = self.st();
            st.procs.remove(&pid);
            st.listeners.retain(|l| l.pid != pid);
        }

        pub fn add_listener(&self, pid: i32, port: u16) {
            let mut st = self.st();
            let l = Listener { pid, port };
            if !st.listeners.contains(&l) {
                st.listeners.push(l);
            }
        }

        pub fn set_established(&self, pid: i32, port: u16, n: u32) {
            self.st().established.insert((pid, port), n);
        }

        pub fn ignore_sigterm(&self, pid: i32) {
            self.st().ignores_sigterm.insert(pid);
        }

        /// Signals to `pid` fail as if refused (EPERM): recorded, no effect,
        /// `false`.
        pub fn fail_signals(&self, pid: i32) {
            self.st().fails_signals.insert(pid);
        }

        pub fn set_env(&self, pid: i32, env: &[(&str, &str)]) {
            let env = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            self.st().env.insert(pid, env);
        }

        pub fn set_open_paths(&self, pid: i32, paths: &[&str]) {
            let paths = paths.iter().map(PathBuf::from).collect();
            self.st().open_paths.insert(pid, paths);
        }

        /// Every `(pid, sig)` sent so far, in order.
        pub fn signals(&self) -> Vec<(i32, i32)> {
            self.st().signals.clone()
        }

        pub fn is_alive(&self, pid: i32) -> bool {
            self.st().procs.contains_key(&pid)
        }
    }

    impl Platform for FakePlatform {
        fn listeners(&self) -> Vec<Listener> {
            self.st().listeners.clone()
        }

        fn proc_info(&self, pid: i32) -> Option<ProcInfo> {
            self.st().procs.get(&pid).cloned()
        }

        fn established(&self, pid: i32, port: u16) -> u32 {
            let st = self.st();
            if !st.procs.contains_key(&pid) {
                return 0;
            }
            st.established.get(&(pid, port)).copied().unwrap_or(0)
        }

        fn descendants(&self, pid: i32) -> Vec<i32> {
            let mut out = Vec::new();
            if pid <= 0 {
                return out;
            }
            let st = self.st();
            let mut seen = HashSet::from([pid]);
            let mut queue = vec![pid];
            while let Some(parent) = queue.pop() {
                for p in st.procs.values() {
                    if p.ppid == parent && seen.insert(p.pid) {
                        out.push(p.pid);
                        queue.insert(0, p.pid);
                    }
                }
            }
            out
        }

        fn signal(&self, pid: i32, sig: i32) -> bool {
            if pid <= 0 {
                return false;
            }
            let mut st = self.st();
            st.signals.push((pid, sig));
            if !st.procs.contains_key(&pid) || st.fails_signals.contains(&pid) {
                return false;
            }
            let dies = sig == libc::SIGKILL
                || (sig == libc::SIGTERM && !st.ignores_sigterm.contains(&pid));
            if dies {
                st.procs.remove(&pid);
                st.listeners.retain(|l| l.pid != pid);
            }
            true
        }

        fn proc_start_string(&self, pid: i32) -> Option<String> {
            self.proc_info(pid).map(|p| format_proc_start(p.start_time))
        }

        fn proc_env(&self, pid: i32) -> Option<Vec<(String, String)>> {
            let st = self.st();
            if !st.procs.contains_key(&pid) {
                return None;
            }
            st.env.get(&pid).cloned()
        }

        fn open_paths(&self, pid: i32) -> Vec<PathBuf> {
            let st = self.st();
            if !st.procs.contains_key(&pid) {
                return Vec::new();
            }
            st.open_paths.get(&pid).cloned().unwrap_or_default()
        }

        fn pids_named(&self, name: &str) -> Vec<i32> {
            let st = self.st();
            st.procs
                .values()
                .filter(|p| {
                    let argv0 = p.command.split(' ').next().unwrap_or("");
                    argv0.rsplit('/').next() == Some(name)
                })
                .map(|p| p.pid)
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-10-15 12:51:40 UTC, from a real session file.
    const OCT_15: u64 = 1_792_068_700;

    #[test]
    fn formats_like_claude_proc_start() {
        assert_eq!(
            format_proc_start(OCT_15 * 1_000_000 + 750_000),
            "Thu Oct 15 12:51:40 2026"
        );
    }

    #[test]
    fn pads_single_digit_day_with_a_space() {
        // 2026-10-01 09:06:05 UTC
        assert_eq!(
            format_proc_start(1_790_845_565 * 1_000_000),
            "Thu Oct  1 09:06:05 2026"
        );
    }

    #[test]
    fn formats_epoch_and_leap_day() {
        assert_eq!(format_proc_start(0), "Thu Jan  1 00:00:00 1970");
        // 2024-02-29 23:59:59 UTC
        assert_eq!(
            format_proc_start(1_709_251_199 * 1_000_000),
            "Thu Feb 29 23:59:59 2024"
        );
    }

    fn chain_platform() -> FakePlatform {
        let f = FakePlatform::new();
        f.spawn(1, 0, 1, "launchd", "/");
        f.spawn(10, 1, 2, "zsh", "/");
        f.spawn(20, 10, 3, "claude", "/w");
        f.spawn(30, 20, 4, "bun dev", "/w");
        f.spawn(40, 30, 5, "workerd", "/w");
        f.spawn(41, 30, 6, "esbuild", "/w");
        f.spawn(50, 40, 7, "node", "/w");
        f
    }

    #[test]
    fn ancestry_walks_up_to_pid_1() {
        let f = chain_platform();
        let pids: Vec<i32> = ancestry(&f, 30).iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![30, 20, 10, 1]);
    }

    #[test]
    fn ancestry_stops_at_uninspectable_parent() {
        let f = chain_platform();
        f.remove_proc(10);
        let pids: Vec<i32> = ancestry(&f, 30).iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![30, 20]);
        assert!(ancestry(&f, 999).is_empty());
    }

    #[test]
    fn ancestry_is_cycle_safe() {
        let f = FakePlatform::new();
        f.spawn(5, 6, 1, "a", "/");
        f.spawn(6, 5, 1, "b", "/");
        let pids: Vec<i32> = ancestry(&f, 5).iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![5, 6]);
    }

    #[test]
    fn ancestry_is_capped_at_64() {
        let f = FakePlatform::new();
        for pid in 2..200 {
            f.spawn(pid, pid + 1, 1, "x", "/");
        }
        assert_eq!(ancestry(&f, 2).len(), 64);
    }

    #[test]
    fn fake_descendants_are_breadth_first() {
        let f = chain_platform();
        assert_eq!(f.descendants(30), vec![40, 41, 50]);
        assert!(f.descendants(50).is_empty());
    }

    #[test]
    fn fake_descendants_empty_for_nonpositive_pid() {
        let f = chain_platform();
        assert!(f.descendants(0).is_empty());
        assert!(f.descendants(-1).is_empty());
    }

    #[test]
    fn fake_signal_semantics() {
        let f = chain_platform();
        f.add_listener(30, 3000);
        f.set_established(30, 3000, 2);
        assert_eq!(f.established(30, 3000), 2);
        f.ignore_sigterm(30);
        assert!(f.signal(30, 0));
        assert!(f.signal(30, libc::SIGTERM));
        assert!(f.is_alive(30));
        assert!(f.signal(30, libc::SIGKILL));
        assert!(!f.is_alive(30));
        assert!(f.listeners().is_empty());
        assert_eq!(f.established(30, 3000), 0);
        assert!(!f.signal(30, 0));
        assert!(!f.signal(0, libc::SIGTERM));
        assert!(f.signal(40, libc::SIGTERM));
        assert!(!f.is_alive(40));
        assert_eq!(
            f.signals(),
            vec![
                (30, 0),
                (30, libc::SIGTERM),
                (30, libc::SIGKILL),
                (30, 0),
                (40, libc::SIGTERM)
            ]
        );
    }

    #[test]
    fn fake_env_open_paths_and_names() {
        let f = FakePlatform::new();
        f.spawn(7, 1, 1, "/usr/local/bin/codex --flag", "/w");
        f.spawn(8, 1, 1, "codex-helper", "/w");
        f.set_env(7, &[("A", "1")]);
        f.set_open_paths(7, &["/x/y.lock"]);
        assert_eq!(f.proc_env(7), Some(vec![("A".into(), "1".into())]));
        assert_eq!(f.proc_env(8), None);
        assert_eq!(f.open_paths(7), vec![PathBuf::from("/x/y.lock")]);
        assert!(f.open_paths(8).is_empty());
        assert_eq!(f.pids_named("codex"), vec![7]);
    }

    #[test]
    fn fake_proc_start_string_uses_the_formatter() {
        let f = FakePlatform::new();
        f.spawn(7, 1, OCT_15 * 1_000_000, "x", "/");
        assert_eq!(
            f.proc_start_string(7).as_deref(),
            Some("Thu Oct 15 12:51:40 2026")
        );
        assert_eq!(f.proc_start_string(8), None);
    }
}
