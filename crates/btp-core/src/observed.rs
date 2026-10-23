//! Observed servers: TCP listeners of the user that no agent started. They
//! are shown next to the tracked ones and never stopped automatically; only
//! the app stops one, by hand, after a confirmation.

use crate::agents::is_agent_cli;
use crate::config::Config;
use crate::model::ServerKey;
use crate::platform::{ancestry, Listener, Platform, ProcInfo};
use crate::watcher::is_excluded;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A listener no agent started, grouped by process tree like a tracked
/// server: `key` is the topmost listening process of the tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ObservedServer {
    pub key: ServerKey,
    /// The lowest port the topmost process listens on.
    pub port: u16,
    /// Every port the tree listens on, sorted.
    pub ports: Vec<u16>,
    pub command: String,
    pub cwd: Option<PathBuf>,
    /// The command or a port matches the exclusions: shown, never stoppable.
    pub excluded: bool,
}

/// macOS system daemons and application bundles: never shown.
pub fn is_system_process(exe: &Path) -> bool {
    ["/System", "/usr/libexec", "/usr/sbin"]
        .iter()
        .any(|root| exe.starts_with(root))
        || exe.to_string_lossy().contains(".app/Contents/")
}

/// Whether each process is a system one, by (pid, start time), so a pass
/// reads the executable path of a process only the first time it listens.
#[derive(Debug, Default)]
pub struct SystemCache {
    system: HashMap<(i32, u64), bool>,
}

impl SystemCache {
    /// An unreadable executable path counts as system.
    fn is_system(&mut self, p: &dyn Platform, info: &ProcInfo) -> bool {
        *self
            .system
            .entry((info.pid, info.start_time))
            .or_insert_with(|| p.exe_path(info.pid).is_none_or(|e| is_system_process(&e)))
    }

    /// Forgets every process that is not among `pids`.
    pub fn retain(&mut self, pids: &HashSet<i32>) {
        self.system.retain(|(pid, _), _| pids.contains(pid));
    }
}

/// The observed servers among `listeners`: every listener outside the
/// tracked trees (`tracked_pids`) that is neither an agent CLI nor a system
/// process, grouped under its topmost listening ancestor. Sorted by port.
pub fn observed_servers(
    p: &dyn Platform,
    listeners: &[Listener],
    tracked_pids: &HashSet<i32>,
    cfg: &Config,
) -> Vec<ObservedServer> {
    observed_servers_cached(p, listeners, tracked_pids, cfg, &mut SystemCache::default())
}

/// `observed_servers` with the system check cached across passes.
pub fn observed_servers_cached(
    p: &dyn Platform,
    listeners: &[Listener],
    tracked_pids: &HashSet<i32>,
    cfg: &Config,
    cache: &mut SystemCache,
) -> Vec<ObservedServer> {
    let mut found: BTreeMap<i32, (ProcInfo, Vec<u16>)> = BTreeMap::new();
    let mut rejected = HashSet::new();
    for l in listeners {
        if tracked_pids.contains(&l.pid) || rejected.contains(&l.pid) {
            continue;
        }
        if let Some((_, ports)) = found.get_mut(&l.pid) {
            ports.push(l.port);
            continue;
        }
        let Some(info) = p.proc_info(l.pid) else {
            rejected.insert(l.pid);
            continue;
        };
        if is_agent_cli(&info.command).is_some() || cache.is_system(p, &info) {
            rejected.insert(l.pid);
            continue;
        }
        found.insert(l.pid, (info, vec![l.port]));
    }
    // Each listener belongs to its topmost ancestor that is also observed
    // (same process, by start time), else to itself.
    let mut groups: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for (&pid, (info, _)) in &found {
        let owner = ancestry(p, info.ppid)
            .iter()
            .rev()
            .find(|a| {
                found
                    .get(&a.pid)
                    .is_some_and(|(i, _)| i.start_time == a.start_time)
            })
            .map_or(pid, |a| a.pid);
        groups.entry(owner).or_default().push(pid);
    }
    let mut out: Vec<ObservedServer> = groups
        .into_iter()
        .map(|(owner, members)| {
            let (info, own_ports) = &found[&owner];
            let mut ports: Vec<u16> = members
                .iter()
                .flat_map(|m| found[m].1.iter().copied())
                .collect();
            ports.sort_unstable();
            ports.dedup();
            let excluded = is_excluded(&info.command, cfg)
                || ports.iter().any(|port| cfg.exclude_ports.contains(port));
            ObservedServer {
                key: ServerKey {
                    pid: info.pid,
                    start_time: info.start_time,
                },
                port: own_ports.iter().copied().min().unwrap_or_default(),
                ports,
                command: info.command.clone(),
                cwd: info.cwd.clone(),
                excluded,
            }
        })
        .collect();
    out.sort_by_key(|o| (o.port, o.key.pid));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FakePlatform;

    const NODE: &str = "/opt/homebrew/bin/node";

    fn cfg() -> Config {
        Config::default()
    }

    /// launchd(1) > zsh(10).
    fn base() -> FakePlatform {
        let p = FakePlatform::new();
        p.spawn(1, 0, 1, "launchd", "/");
        p.spawn(10, 1, 10, "zsh", "/");
        p
    }

    fn listen(p: &FakePlatform, pid: i32, ppid: i32, command: &str, exe: &str, port: u16) {
        p.spawn(pid, ppid, pid as u64, command, "/w");
        p.set_exe(pid, exe);
        p.add_listener(pid, port);
    }

    fn scan(p: &FakePlatform, tracked: &[i32]) -> Vec<ObservedServer> {
        let tracked = tracked.iter().copied().collect();
        observed_servers(p, &p.listeners(), &tracked, &cfg())
    }

    #[test]
    fn system_paths_are_hidden() {
        for exe in [
            "/System/Library/CoreServices/rapportd.app/Contents/MacOS/rapportd",
            "/System/Library/PrivateFrameworks/X.framework/rapportd",
            "/usr/libexec/x",
            "/usr/sbin/sshd",
            "/Applications/Docker.app/Contents/MacOS/com.docker.backend",
        ] {
            assert!(is_system_process(Path::new(exe)), "{exe}");
        }
        assert!(!is_system_process(Path::new(NODE)));
        assert!(!is_system_process(Path::new("/usr/bin/perl")));
        assert!(!is_system_process(Path::new("/Systemx/node")));
        let p = base();
        listen(&p, 20, 1, "sshd", "/usr/sbin/sshd", 22);
        listen(&p, 21, 10, "node app.js", NODE, 3000);
        // No readable executable path: hidden too.
        p.spawn(22, 10, 22, "mystery", "/w");
        p.add_listener(22, 4000);
        let o = scan(&p, &[]);
        assert_eq!(o.len(), 1);
        assert_eq!((o[0].key.pid, o[0].port), (21, 3000));
    }

    #[test]
    fn observed_groups_by_tree() {
        let p = base();
        listen(&p, 20, 10, "next dev", NODE, 3000);
        p.spawn(21, 20, 21, "node worker", "/w");
        listen(&p, 22, 21, "node --inspect", NODE, 9229);
        let o = scan(&p, &[]);
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(
            o[0].key,
            ServerKey {
                pid: 20,
                start_time: 20
            }
        );
        assert_eq!(o[0].port, 3000);
        assert_eq!(o[0].ports, [3000, 9229]);
        assert_eq!(o[0].command, "next dev");
        assert_eq!(o[0].cwd.as_deref(), Some(Path::new("/w")));
        assert!(!o[0].excluded);
    }

    #[test]
    fn a_system_parent_does_not_absorb_its_child() {
        let p = base();
        listen(
            &p,
            20,
            10,
            "Docker",
            "/Applications/Docker.app/Contents/MacOS/x",
            2375,
        );
        listen(&p, 21, 20, "node app.js", NODE, 3000);
        let o = scan(&p, &[]);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].key.pid, 21);
    }

    #[test]
    fn tracked_trees_and_agent_clis_are_not_observed() {
        let p = base();
        listen(&p, 20, 10, "claude", NODE, 3333);
        listen(&p, 21, 10, "codex", NODE, 1455);
        listen(&p, 30, 10, "bun run dev", NODE, 3000);
        listen(&p, 31, 30, "workerd serve", NODE, 8787);
        listen(&p, 40, 10, "node other.js", NODE, 4000);
        let o = scan(&p, &[30, 31]);
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(o[0].key.pid, 40);
    }

    #[test]
    fn excluded_command_or_port_is_marked_excluded() {
        let p = base();
        listen(
            &p,
            20,
            10,
            "postgres -D /db",
            "/opt/homebrew/bin/postgres",
            5432,
        );
        listen(&p, 21, 10, "node app.js", NODE, 3000);
        listen(&p, 22, 10, "node api.js", NODE, 4000);
        let tracked = HashSet::new();
        let c = Config {
            exclude_ports: vec![4000],
            ..Config::default()
        };
        let o = observed_servers(&p, &p.listeners(), &tracked, &c);
        let excluded: Vec<(u16, bool)> = o.iter().map(|o| (o.port, o.excluded)).collect();
        assert_eq!(excluded, [(3000, false), (4000, true), (5432, true)]);
    }

    #[test]
    fn a_reused_pid_is_checked_again() {
        let p = base();
        listen(&p, 20, 10, "node app.js", NODE, 3000);
        let mut cache = SystemCache::default();
        let tracked = HashSet::new();
        let run = |cache: &mut SystemCache| {
            observed_servers_cached(&p, &p.listeners(), &tracked, &cfg(), cache)
        };
        assert_eq!(run(&mut cache).len(), 1);
        p.remove_proc(20);
        p.spawn(20, 10, 99, "rapportd", "/");
        p.set_exe(20, "/usr/libexec/rapportd");
        p.add_listener(20, 3000);
        assert!(run(&mut cache).is_empty());
    }
}
