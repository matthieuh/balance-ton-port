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

/// macOS system daemons and application bundles: never shown. A bundle
/// inside a language runtime framework (`Python.framework/.../Python.app`) is
/// not an application, unless that framework itself sits in an app bundle.
pub fn is_system_process(exe: &Path) -> bool {
    ["/System", "/usr/libexec", "/usr/sbin"]
        .iter()
        .any(|root| exe.starts_with(root))
        || in_app_bundle(exe)
}

fn in_app_bundle(exe: &Path) -> bool {
    let parts: Vec<_> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect();
    let mut in_framework = false;
    for (i, part) in parts.iter().enumerate() {
        if part.ends_with(".app") && parts.get(i + 1).is_some_and(|n| n == "Contents") {
            if !in_framework {
                return true;
            }
        } else if part.ends_with(".framework") {
            in_framework = true;
        }
    }
    false
}

/// Whether each process is a system one, by (pid, start time), so a pass
/// reads the executable path of a process only the first time it listens.
/// Same for the parents of listeners: siblings started by one shell share a
/// single read.
#[derive(Debug, Default)]
pub struct SystemCache {
    system: HashMap<(i32, u64), bool>,
    app_parent: HashMap<(i32, u64), bool>,
    parents_seen: HashSet<i32>,
}

impl SystemCache {
    /// An unreadable executable path counts as system.
    fn is_system(&mut self, p: &dyn Platform, info: &ProcInfo) -> bool {
        *self
            .system
            .entry((info.pid, info.start_time))
            .or_insert_with(|| p.exe_path(info.pid).is_none_or(|e| is_system_process(&e)))
    }

    /// Whether the direct parent of `info` runs from an application bundle
    /// (an app's helper, like a bundled node runtime). An unreadable parent
    /// does not count.
    fn has_app_parent(&mut self, p: &dyn Platform, info: &ProcInfo) -> bool {
        let Some(parent) = p.proc_info(info.ppid) else {
            return false;
        };
        self.parents_seen.insert(parent.pid);
        *self
            .app_parent
            .entry((parent.pid, parent.start_time))
            .or_insert_with(|| p.exe_path(parent.pid).is_some_and(|e| in_app_bundle(&e)))
    }

    /// Forgets every process that is not among `pids`, and every parent not
    /// looked at since the last call.
    pub fn retain(&mut self, pids: &HashSet<i32>) {
        self.system.retain(|(pid, _), _| pids.contains(pid));
        let seen = std::mem::take(&mut self.parents_seen);
        self.app_parent.retain(|(pid, _), _| seen.contains(pid));
    }
}

/// The observed servers among `listeners`: every listener outside the
/// tracked trees (`tracked_pids`) that is neither an agent CLI, nor a system
/// process, nor the helper of an application (its direct parent runs from an
/// app bundle), grouped under its topmost listening ancestor. Sorted by port.
/// A server whose parent is this process or one of `own_pids` (the menu bar
/// app, which restarts observed servers) is shown.
pub fn observed_servers(
    p: &dyn Platform,
    listeners: &[Listener],
    tracked_pids: &HashSet<i32>,
    own_pids: &[i32],
    cfg: &Config,
) -> Vec<ObservedServer> {
    observed_servers_cached(
        p,
        listeners,
        tracked_pids,
        own_pids,
        cfg,
        &mut SystemCache::default(),
    )
}

/// `observed_servers` with the system check cached across passes.
pub fn observed_servers_cached(
    p: &dyn Platform,
    listeners: &[Listener],
    tracked_pids: &HashSet<i32>,
    own_pids: &[i32],
    cfg: &Config,
    cache: &mut SystemCache,
) -> Vec<ObservedServer> {
    let me = std::process::id() as i32;
    let ours = |pid: i32| pid == me || own_pids.contains(&pid);
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
        if is_agent_cli(&info.command).is_some()
            || cache.is_system(p, &info)
            || (!ours(info.ppid) && cache.has_app_parent(p, &info))
        {
            rejected.insert(l.pid);
            continue;
        }
        found.insert(l.pid, (info, vec![l.port]));
    }
    // Each listener belongs to its topmost ancestor that is also observed
    // (same process, by start time), else to itself. The search stops at an
    // agent CLI or an excluded command: a Stop spares those and whatever
    // they run, so it would never reach the listener.
    let mut groups: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for (&pid, (info, _)) in &found {
        let owner = ancestry(p, info.ppid)
            .iter()
            .take_while(|a| is_agent_cli(&a.command).is_none() && !is_excluded(&a.command, cfg))
            .filter(|a| {
                found
                    .get(&a.pid)
                    .is_some_and(|(i, _)| i.start_time == a.start_time)
            })
            .last()
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
        observed_servers(p, &p.listeners(), &tracked, &[], &cfg())
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
        for exe in [
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper.app/Contents/MacOS/Code Helper",
            "/Users/me/Applications/X.app/Contents/MacOS/x",
            "/Applications/Foo.app/Contents/Frameworks/Python.framework/Versions/3.12/Resources/Python.app/Contents/MacOS/Python",
        ] {
            assert!(is_system_process(Path::new(exe)), "{exe}");
        }
        for exe in [
            "/opt/homebrew/Cellar/python@3.14/3.14.7/Frameworks/Python.framework/Versions/3.14/Resources/Python.app/Contents/MacOS/Python",
            "/Library/Frameworks/Python.framework/Versions/3.12/Resources/Python.app/Contents/MacOS/Python",
        ] {
            assert!(!is_system_process(Path::new(exe)), "{exe}");
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
    fn grouping_stops_at_an_agent_cli_or_an_excluded_command() {
        let p = base();
        listen(&p, 20, 10, "node tools.js", NODE, 3000);
        // tools.js started an agent CLI, which started a dev server.
        p.spawn(21, 20, 21, "claude", "/w");
        listen(&p, 22, 21, "vite", NODE, 5173);
        // And a database that runs a listener of its own.
        p.spawn(23, 20, 23, "postgres -D /db", "/w");
        listen(&p, 24, 23, "node exporter.js", NODE, 9187);
        let o = scan(&p, &[]);
        let keys: Vec<(i32, Vec<u16>)> = o.iter().map(|o| (o.key.pid, o.ports.clone())).collect();
        assert_eq!(keys, [(20, vec![3000]), (22, vec![5173]), (24, vec![9187])]);
    }

    #[test]
    fn a_system_parent_does_not_absorb_its_child() {
        let p = base();
        // A daemon outside any app bundle: an app bundle parent would hide
        // the child altogether (see `an_app_helper_is_hidden`).
        listen(&p, 20, 10, "daemon", "/usr/libexec/daemon", 2375);
        listen(&p, 21, 20, "node app.js", NODE, 3000);
        let o = scan(&p, &[]);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].key.pid, 21);
    }

    const RAYCAST: &str = "/Applications/Raycast.app/Contents/MacOS/Raycast";
    const BUNDLED_NODE: &str =
        "/Users/me/Library/Application Support/com.raycast.macos/node/runtime/22/bin/node";

    #[test]
    fn an_app_helper_is_hidden() {
        let p = base();
        // The app itself (not listening) runs a bundled node outside the
        // bundle: that helper listener is the app's, not a dev server.
        p.spawn(20, 1, 20, "Raycast", "/");
        p.set_exe(20, RAYCAST);
        listen(&p, 21, 20, "Raycast Backend", BUNDLED_NODE, 7265);
        // The same runtime started from a terminal is a dev server.
        listen(&p, 22, 10, "node server.js", BUNDLED_NODE, 3000);
        let o = scan(&p, &[]);
        let pids: Vec<i32> = o.iter().map(|o| o.key.pid).collect();
        assert_eq!(pids, [22]);
    }

    #[test]
    fn a_framework_python_parent_is_not_an_app() {
        let p = base();
        let python = "/Library/Frameworks/Python.framework/Versions/3.12/Resources/Python.app/Contents/MacOS/Python";
        p.spawn(20, 10, 20, "python3 manage.py runserver", "/w");
        p.set_exe(20, python);
        listen(&p, 21, 20, "python3 manage.py runserver", python, 8000);
        let o = scan(&p, &[]);
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!(o[0].port, 8000);
    }

    #[test]
    fn a_server_restarted_by_the_menu_bar_app_is_shown() {
        // The app restarts an observed server as its own child; the app runs
        // from its bundle, yet that server must stay visible.
        let me = std::process::id() as i32;
        for (app, own) in [(me, vec![]), (50, vec![50])] {
            let p = base();
            p.spawn(app, 1, 5, "Balance ton port", "/");
            p.set_exe(
                app,
                "/Applications/Balance ton port.app/Contents/MacOS/Balance ton port",
            );
            listen(&p, 60, app, "node server.js", NODE, 3000);
            let tracked = HashSet::new();
            let o = observed_servers(&p, &p.listeners(), &tracked, &own, &cfg());
            assert_eq!(o.len(), 1, "app {app}: {o:?}");
            assert_eq!(o[0].key.pid, 60);
        }
    }

    #[test]
    fn a_reparented_helper_is_checked_again() {
        let p = base();
        p.spawn(20, 1, 20, "Raycast", "/");
        p.set_exe(20, RAYCAST);
        listen(&p, 21, 20, "Raycast Backend", BUNDLED_NODE, 7265);
        let mut cache = SystemCache::default();
        let tracked = HashSet::new();
        let run = |cache: &mut SystemCache| {
            observed_servers_cached(&p, &p.listeners(), &tracked, &[], &cfg(), cache)
        };
        assert!(run(&mut cache).is_empty());
        // The app quit and launchd adopted the listener: now it is a stray
        // listener of the user like any other.
        p.remove_proc(20);
        p.remove_proc(21);
        p.spawn(21, 1, 21, "Raycast Backend", "/w");
        p.set_exe(21, BUNDLED_NODE);
        p.add_listener(21, 7265);
        assert_eq!(run(&mut cache).len(), 1);
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
        let o = observed_servers(&p, &p.listeners(), &tracked, &[], &c);
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
            observed_servers_cached(&p, &p.listeners(), &tracked, &[], &cfg(), cache)
        };
        assert_eq!(run(&mut cache).len(), 1);
        p.remove_proc(20);
        p.spawn(20, 10, 99, "rapportd", "/");
        p.set_exe(20, "/usr/libexec/rapportd");
        p.add_listener(20, 3000);
        assert!(run(&mut cache).is_empty());
    }
}
