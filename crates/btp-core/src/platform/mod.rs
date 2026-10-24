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

/// Memory and accumulated CPU time of one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcResources {
    /// Physical footprint, as Activity Monitor's "Memory" column (resident
    /// size when the footprint cannot be read). Kept as `rss_bytes` for the
    /// callers and the FFI.
    pub rss_bytes: u64,
    /// User + system CPU time since the process started, nanoseconds.
    pub cpu_time_ns: u64,
}

/// Machine-wide memory, in bytes. Free is `total_bytes - used_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineMemory {
    pub total_bytes: u64,
    /// As Activity Monitor's "Memory Used": app memory (internal minus
    /// purgeable) + wired + compressed.
    pub used_bytes: u64,
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
    /// Memory (physical footprint) and CPU time of `pid`, `None` when gone or not ours.
    fn proc_resources(&self, pid: i32) -> Option<ProcResources>;
    /// Total and used memory of the machine.
    fn machine_memory(&self) -> Option<MachineMemory>;
    /// The argv vector of `pid` (what `ProcInfo::command` joins).
    fn proc_argv(&self, pid: i32) -> Option<Vec<String>>;
    /// Path of the executable `pid` runs.
    fn exe_path(&self, pid: i32) -> Option<PathBuf>;
    /// Like `established`, but a connection whose remote end is a loopback
    /// socket owned by one of `peer_pids` does not count. Only those pids
    /// are scanned. The default ignores a non-empty `peer_pids`: a platform
    /// that can read peer sockets must override it.
    fn established_excluding(&self, pid: i32, port: u16, peer_pids: &[i32]) -> u32 {
        let _ = peer_pids;
        self.established(pid, port)
    }
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
    use super::{format_proc_start, Listener, MachineMemory, Platform, ProcInfo, ProcResources};
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
        resources: HashMap<i32, ProcResources>,
        machine_memory: Option<MachineMemory>,
        exe: HashMap<i32, PathBuf>,
        exe_calls: usize,
        /// `(pid, port)` -> `(peer pid, connections from it)`.
        established_from: Vec<(i32, u16, i32, u32)>,
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

        /// The environment of `pid` can no longer be read.
        pub fn clear_env(&self, pid: i32) {
            self.st().env.remove(&pid);
        }

        pub fn set_open_paths(&self, pid: i32, paths: &[&str]) {
            let paths = paths.iter().map(PathBuf::from).collect();
            self.st().open_paths.insert(pid, paths);
        }

        pub fn set_resources(&self, pid: i32, rss: u64, cpu_ns: u64) {
            self.st().resources.insert(
                pid,
                ProcResources {
                    rss_bytes: rss,
                    cpu_time_ns: cpu_ns,
                },
            );
        }

        pub fn set_machine_memory(&self, total: u64, used: u64) {
            self.st().machine_memory = Some(MachineMemory {
                total_bytes: total,
                used_bytes: used,
            });
        }

        pub fn set_exe(&self, pid: i32, path: &str) {
            self.st().exe.insert(pid, PathBuf::from(path));
        }

        /// `n` of the connections counted by `established(pid, port)` come
        /// from `peer_pid`: `established_excluding` drops them when that pid
        /// is listed.
        pub fn set_established_from(&self, pid: i32, port: u16, peer_pid: i32, n: u32) {
            let mut st = self.st();
            st.established_from
                .retain(|e| (e.0, e.1, e.2) != (pid, port, peer_pid));
            st.established_from.push((pid, port, peer_pid, n));
        }

        /// How many times `exe_path` was called.
        pub fn exe_calls(&self) -> usize {
            self.st().exe_calls
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

        fn proc_resources(&self, pid: i32) -> Option<ProcResources> {
            let st = self.st();
            if !st.procs.contains_key(&pid) {
                return None;
            }
            st.resources.get(&pid).copied()
        }

        fn machine_memory(&self) -> Option<MachineMemory> {
            self.st().machine_memory
        }

        fn proc_argv(&self, pid: i32) -> Option<Vec<String>> {
            let st = self.st();
            let p = st.procs.get(&pid)?;
            Some(p.command.split(' ').map(str::to_string).collect())
        }

        fn exe_path(&self, pid: i32) -> Option<PathBuf> {
            let mut st = self.st();
            st.exe_calls += 1;
            if !st.procs.contains_key(&pid) {
                return None;
            }
            st.exe.get(&pid).cloned()
        }

        fn established_excluding(&self, pid: i32, port: u16, peer_pids: &[i32]) -> u32 {
            let total = self.established(pid, port);
            let st = self.st();
            let from_peers: u32 = st
                .established_from
                .iter()
                .filter(|e| e.0 == pid && e.1 == port && peer_pids.contains(&e.2))
                .map(|e| e.3)
                .sum();
            total.saturating_sub(from_peers)
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
    fn fake_resources_memory_argv_and_exe() {
        let f = chain_platform();
        f.spawn(60, 1, 8, "node vite --port 5173", "/w");
        assert_eq!(f.proc_resources(60), None);
        f.set_resources(60, 4096, 7);
        assert_eq!(
            f.proc_resources(60),
            Some(ProcResources {
                rss_bytes: 4096,
                cpu_time_ns: 7
            })
        );
        f.remove_proc(60);
        assert_eq!(f.proc_resources(60), None);
        assert_eq!(f.machine_memory(), None);
        f.set_machine_memory(100, 40);
        assert_eq!(
            f.machine_memory(),
            Some(MachineMemory {
                total_bytes: 100,
                used_bytes: 40
            })
        );
        assert_eq!(
            f.proc_argv(30),
            Some(vec!["bun".to_string(), "dev".to_string()])
        );
        assert_eq!(f.proc_argv(999), None);
        assert_eq!(f.exe_path(30), None);
        f.set_exe(30, "/usr/bin/bun");
        assert_eq!(f.exe_path(30), Some(PathBuf::from("/usr/bin/bun")));
    }

    #[test]
    fn fake_established_excluding_drops_listed_peers_only() {
        let f = chain_platform();
        f.set_established(30, 3000, 5);
        f.set_established_from(30, 3000, 20, 2);
        f.set_established_from(30, 3000, 21, 1);
        assert_eq!(f.established_excluding(30, 3000, &[]), 5);
        assert_eq!(f.established_excluding(30, 3000, &[20]), 3);
        assert_eq!(f.established_excluding(30, 3000, &[20, 21]), 2);
        assert_eq!(f.established_excluding(30, 3000, &[99]), 5);
        // Replacing a pair does not stack.
        f.set_established_from(30, 3000, 20, 1);
        assert_eq!(f.established_excluding(30, 3000, &[20]), 4);
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
