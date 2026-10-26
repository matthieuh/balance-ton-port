//! What `btp run` and the hook ask the watcher before a server starts: who
//! holds the wanted port, freeing it when the idle rules allow, and a server
//! already running the same command in the same folder.

use super::*;
use crate::run::command::same_command_key;
use crate::run::conflict::{freeable, Holder};

impl<P: Platform> Watcher<P> {
    /// Who holds `port`; `None` when it is free. Only that port's
    /// listeners are looked at (no pass, no probe): a tracked server when
    /// every listener of the port belongs to its tree, `Unknown` when one of
    /// them cannot be attributed (unreadable, or another user's, which
    /// `listeners` does not even list), else an observed holder named by
    /// its tool label. The caller and its ancestors are never a holder:
    /// a port only they listen on (a socket inherited by accident) is
    /// `Unknown`, so it is rerouted around, never freed.
    pub fn port_holder(&mut self, port: u16, now_ms: u64) -> io::Result<Option<Holder>> {
        let listeners = self.platform.listeners();
        let own = own_pids(&self.platform);
        let all: Vec<i32> = listeners
            .iter()
            .filter(|l| l.port == port)
            .map(|l| l.pid)
            .collect();
        let on_port: Vec<i32> = all.iter().copied().filter(|p| !own.contains(p)).collect();
        if on_port.is_empty() {
            let taken = !all.is_empty() || self.platform.port_answers(port);
            return Ok(taken.then_some(Holder::Unknown));
        }
        let scan = self.scan();
        let p = &self.platform;
        with_state(&self.data_dir, |state| {
            let mut owners = HashSet::new();
            let mut untracked = None;
            for &pid in &on_port {
                let Some(info) = p.proc_info(pid) else {
                    return Some(Holder::Unknown);
                };
                match owner_of(p, pid, &state.servers, &scan.cfg) {
                    Some(i) => {
                        owners.insert(i);
                    }
                    None => {
                        untracked.get_or_insert(info);
                    }
                }
            }
            if untracked.is_none() && owners.len() == 1 {
                // Gone meanwhile: not something to vouch for either.
                return owners
                    .into_iter()
                    .next()
                    .and_then(|i| self.holder_of(&state.servers[i], &listeners, &scan, now_ms))
                    .or(Some(Holder::Unknown));
            }
            // Untracked, or shared with a tracked server: killing that one
            // would not free the port.
            let Some(info) = untracked.or_else(|| p.proc_info(on_port[0])) else {
                return Some(Holder::Unknown);
            };
            Some(Holder::Observed {
                label: tool_label(&info.command),
                excluded: is_excluded(&info.command, &scan.cfg)
                    || scan.cfg.exclude_ports.contains(&port),
            })
        })
    }

    /// Frees a port for `btp run` by killing the tracked server `key`, only
    /// if it is still `freeable` with the config read from disk now (a
    /// broken config frees nothing). The check and the SIGTERM happen under
    /// the state lock, so a Keep or a note saved meanwhile always wins; the
    /// grace wait and the SIGKILL follow outside it, every start time
    /// re-checked like any kill. Recorded with `KillReason::PortConflict`.
    /// `None`, sending nothing, when it no longer qualifies.
    pub fn free_for_run(&mut self, key: ServerKey, now_ms: u64) -> io::Result<Option<KillRecord>> {
        // A restart or a stop of it running: not ours to kill.
        let Some(_claim) = self.in_flight.claim(key) else {
            return Ok(None);
        };
        let scan = self.scan();
        if self.config_error.is_some() {
            return Ok(None);
        }
        let listeners = self.platform.listeners();
        let p = &self.platform;
        let started = with_state(&self.data_dir, |state| {
            let s = state
                .servers
                .iter()
                .find(|s| s.key == key && !s.session_proc_start.is_empty())?;
            let Some(Holder::Tracked {
                server,
                obs,
                idle_ms,
                in_use,
            }) = self.holder_of(s, &listeners, &scan, now_ms)
            else {
                return None;
            };
            if !freeable(&server, &obs, idle_ms, in_use, &scan.cfg, now_ms) {
                return None;
            }
            let targets = term_tree(p, key, &scan.cfg)?;
            Some((server, targets))
        })?;
        let Some((server, targets)) = started else {
            return Ok(None);
        };
        finish_kill(
            p,
            &targets,
            Instant::now() + Duration::from_secs(scan.cfg.kill_grace_s),
        );
        let r = record(&server, KillReason::PortConflict, now_ms);
        with_state(&self.data_dir, |state| {
            state.servers.retain(|s| s.key != key);
            state.record_kill(r.clone());
        })?;
        append_kill_log(&self.data_dir, &r)?;
        Ok(Some(r))
    }

    /// A server already running the command `key` (`same_command_key`) in
    /// exactly the folder `cwd`: a tracked one of the same worktree first
    /// (its own process or one of its listeners has that key), else an
    /// observed one, whose view has no session (empty `session_name`).
    /// Never the caller or one of its ancestors: a `btp run` that
    /// inherited a listening socket would otherwise "reuse" itself.
    pub fn find_reusable(
        &mut self,
        cwd: &Path,
        key: &str,
        now_ms: u64,
    ) -> io::Result<Option<ServerView>> {
        let folder = canonical(cwd);
        let root = canonical(&worktree_root(cwd));
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let p = &self.platform;
        let own = own_pids(p);
        let same = |pid: i32| !own.contains(&pid) && runs_key(p, pid, &folder, cwd, key);
        let (found, tracked) = with_state(&self.data_dir, |state| {
            let trees: Vec<Tree> = state
                .servers
                .iter()
                .map(|s| {
                    if s.session_proc_start.is_empty() {
                        Tree::default()
                    } else {
                        tree_of(p, s, &listeners)
                    }
                })
                .collect();
            let found = state
                .servers
                .iter()
                .zip(&trees)
                .find(|(s, t)| {
                    !t.pids.is_empty()
                        && canonical(&s.worktree) == root
                        && (same(s.key.pid)
                            || listeners
                                .iter()
                                .filter(|l| t.pids.contains(&l.pid))
                                .any(|l| same(l.pid)))
                })
                .map(|(s, t)| {
                    let o = self.observe_one(s, t, &scan, now_ms);
                    let session = scan
                        .live
                        .iter()
                        .find(|l| {
                            l.session.pid == s.session_pid
                                && l.session.proc_start == s.session_proc_start
                        })
                        .map(|l| session_display_name(Some(&l.session.name), &l.session.cwd));
                    let mut v = view(s, &o, t, session.as_deref(), now_ms, &scan.cfg);
                    v.has_logs =
                        crate::runlog::log_key_of(&self.platform, &self.data_dir, v.key).is_some();
                    v
                });
            (found, tracked_pids(&trees))
        })?;
        if found.is_some() {
            return Ok(found);
        }
        let observed = self
            .observed(&listeners, &tracked, &scan.cfg)
            .into_iter()
            .find(|o| {
                !own.contains(&o.key.pid) && runs_key(&self.platform, o.key.pid, &folder, cwd, key)
            });
        Ok(observed.map(|o| {
            let established = self.observed_feed(&o, &[], now_ms).established;
            let mut v = observed_view(&o, established);
            v.has_logs = crate::runlog::log_key_of(&self.platform, &self.data_dir, v.key).is_some();
            v
        }))
    }

    /// A tracked server as a holder of its port: `Observed` (excluded) when
    /// it matches an exclusion now, `None` once its process is gone.
    fn holder_of(
        &self,
        s: &TrackedServer,
        listeners: &[Listener],
        scan: &Scan,
        now_ms: u64,
    ) -> Option<Holder> {
        let tree = tree_of(&self.platform, s, listeners);
        if tree.pids.is_empty() {
            return None;
        }
        if excluded_now(&self.platform, s, &tree, &scan.cfg) {
            return Some(Holder::Observed {
                label: tool_label(&s.command),
                excluded: true,
            });
        }
        let obs = self.observe_one(s, &tree, scan, now_ms);
        let v = view(s, &obs, &tree, None, now_ms, &scan.cfg);
        Some(Holder::Tracked {
            server: s.clone(),
            obs,
            idle_ms: v.idle_ms,
            in_use: v.in_use,
        })
    }
}

/// The tracked server (index into `servers`) whose tree holds `pid`: the
/// topmost one among `pid` and its ancestors. The walk stops where
/// `term_tree` would stop signalling: at an agent CLI (a nested session's
/// servers are not the outer server's, as in `tree_of`) and at an untracked
/// excluded command (spared with its subtree, so killing the server would
/// not free its port).
fn owner_of(p: &dyn Platform, pid: i32, servers: &[TrackedServer], cfg: &Config) -> Option<usize> {
    let mut owner = None;
    for (i, proc) in ancestry(p, pid).iter().enumerate() {
        if i > 0 && is_agent_cli(&proc.command).is_some() {
            break;
        }
        match servers.iter().position(|s| {
            !s.session_proc_start.is_empty()
                && s.key.pid == proc.pid
                && s.key.start_time == proc.start_time
        }) {
            Some(j) => owner = Some(j),
            None if is_excluded(&proc.command, cfg) => break,
            None => {}
        }
    }
    owner
}

/// This process and its ancestors.
fn own_pids(p: &dyn Platform) -> HashSet<i32> {
    let me = std::process::id() as i32;
    let mut own: HashSet<i32> = ancestry(p, me).iter().map(|a| a.pid).collect();
    own.insert(me);
    own
}

/// `pid` runs in `folder` (resolved) a command whose `same_command_key`,
/// read in `cwd`, is `key`.
fn runs_key(p: &dyn Platform, pid: i32, folder: &Path, cwd: &Path, key: &str) -> bool {
    p.proc_info(pid)
        .is_some_and(|i| i.cwd.as_deref().map(canonical).as_deref() == Some(folder))
        && p.proc_argv(pid)
            .and_then(|argv| same_command_key(&argv, cwd))
            .is_some_and(|k| k == key)
}

/// An observed server as a `ServerView`: no session, so never idle on the
/// session's account and nothing scheduled.
fn observed_view(o: &ObservedServer, established: u32) -> ServerView {
    let worktree = o.cwd.as_deref().map(worktree_root).unwrap_or_default();
    ServerView {
        key: o.key,
        port: o.port,
        ports: o.ports.clone(),
        command: o.command.clone(),
        tool_label: tool_label(&o.command),
        worktree_name: worktree_name(&worktree),
        location_label: location_label(&worktree),
        worktree,
        session_name: String::new(),
        agent: AgentKind::default(),
        started_ms: o.key.start_time / 1000,
        session_alive: true,
        idle_ms: 0,
        kill_in_ms: None,
        in_use: established > 0,
        postponed_until_ms: None,
        note_reason: None,
        kept_by_user: false,
        has_logs: false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    use crate::model::KillReason;
    use std::fs;

    fn key30() -> ServerKey {
        ServerKey {
            pid: 30,
            start_time: START + 30 * SEC,
        }
    }

    /// The fixture's server, linked at `T0`, its session gone since.
    fn dead_session_fx() -> Fx {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().remove_proc(20);
        f
    }

    #[test]
    fn holder_none_when_free() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        assert!(f.w.port_holder(4000, T0).unwrap().is_none());
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn holder_tracked_with_idle() {
        let mut f = dead_session_fx();
        // A port of a child of the server is the server's too.
        f.p().add_listener(31, 8787);
        for port in [3000, 8787] {
            match f.w.port_holder(port, T0 + 11 * MIN).unwrap() {
                Some(Holder::Tracked {
                    server,
                    obs,
                    idle_ms,
                    in_use,
                }) => {
                    assert_eq!(server.key, key30());
                    assert!(!obs.session_alive);
                    assert_eq!(idle_ms, 11 * MIN);
                    assert!(!in_use);
                }
                other => panic!("port {port}: {other:?}"),
            }
        }
        f.p().set_established(30, 3000, 1);
        match f.w.port_holder(3000, T0 + 11 * MIN).unwrap() {
            Some(Holder::Tracked { in_use, .. }) => assert!(in_use),
            other => panic!("{other:?}"),
        }
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn holder_unknown_for_unattributed_listener() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        // A listener whose process cannot be read.
        f.p().add_listener(500, 4000);
        assert!(matches!(
            f.w.port_holder(4000, T0).unwrap(),
            Some(Holder::Unknown)
        ));
        // A listener `listeners` cannot see at all (another user's).
        f.p().add_foreign_listener(4001);
        assert!(matches!(
            f.w.port_holder(4001, T0).unwrap(),
            Some(Holder::Unknown)
        ));
        // A tracked server and an unknown process on one port: killing the
        // tracked one would not free it.
        f.p().add_listener(500, 3000);
        assert!(matches!(
            f.w.port_holder(3000, T0).unwrap(),
            Some(Holder::Unknown)
        ));
    }

    #[test]
    fn holder_observed_for_untracked_listener() {
        let mut f = fx();
        f.w.tick(T0).unwrap();
        f.p().spawn(50, 10, START + 50 * SEC, "next dev", "/w");
        f.p().add_listener(50, 4000);
        match f.w.port_holder(4000, T0).unwrap() {
            Some(Holder::Observed { label, excluded }) => {
                assert_eq!(label, tool_label("next dev"));
                assert!(!excluded);
            }
            other => panic!("{other:?}"),
        }
        let mut cfg = f.cfg();
        cfg.exclude_ports.push(4000);
        crate::config::save_config(f.data.path(), &cfg).unwrap();
        assert!(matches!(
            f.w.port_holder(4000, T0).unwrap(),
            Some(Holder::Observed { excluded: true, .. })
        ));
    }

    #[test]
    fn excluded_child_listener_is_not_the_servers() {
        let mut f = dead_session_fx();
        let web = f.wt.join("apps/web");
        let mut cfg = f.cfg();
        cfg.exclude_commands.push("postgres".into());
        crate::config::save_config(f.data.path(), &cfg).unwrap();
        // `bun run dev` starts postgres, which `term_tree` spares.
        f.p().spawn(
            40,
            30,
            START + 40 * SEC,
            "postgres -D db",
            web.to_str().unwrap(),
        );
        f.p().add_listener(40, 5432);
        let now = T0 + 11 * MIN;
        assert!(matches!(
            f.w.port_holder(5432, now).unwrap(),
            Some(Holder::Observed { excluded: true, .. })
        ));
        let postgres = ServerKey {
            pid: 40,
            start_time: START + 40 * SEC,
        };
        assert_eq!(f.w.free_for_run(postgres, now).unwrap(), None);
        assert!(f.p().signals().is_empty());
        assert!(f.p().is_alive(40));
        // The server's own port is still its own.
        assert!(matches!(
            f.w.port_holder(3000, now).unwrap(),
            Some(Holder::Tracked { .. })
        ));
    }

    #[test]
    fn free_for_run_rechecks() {
        let mut f = dead_session_fx();
        let now = T0 + 11 * MIN;
        assert!(matches!(
            f.w.port_holder(3000, now).unwrap(),
            Some(Holder::Tracked { .. })
        ));
        // Kept between the question and the kill.
        f.w.keep(key30()).unwrap();
        assert_eq!(f.w.free_for_run(key30(), now).unwrap(), None);
        assert!(f.p().signals().is_empty());
        assert!(f.p().is_alive(30));
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn free_for_run_refuses_what_is_not_freeable() {
        let mut f = dead_session_fx();
        // Idle 9 min only.
        assert_eq!(f.w.free_for_run(key30(), T0 + 9 * MIN).unwrap(), None);
        // In use.
        f.p().set_established(31, 3000, 1);
        assert_eq!(f.w.free_for_run(key30(), T0 + 11 * MIN).unwrap(), None);
        f.p().set_established(31, 3000, 0);
        // Excluded since.
        let mut cfg = f.cfg();
        cfg.exclude_commands.push("bun run".into());
        crate::config::save_config(f.data.path(), &cfg).unwrap();
        assert_eq!(f.w.free_for_run(key30(), T0 + 11 * MIN).unwrap(), None);
        // Not tracked.
        let other = ServerKey {
            pid: 30,
            start_time: START,
        };
        assert_eq!(f.w.free_for_run(other, T0 + 11 * MIN).unwrap(), None);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn free_for_run_frees_nothing_on_a_broken_config() {
        let mut f = dead_session_fx();
        fs::write(f.data.path().join("config.toml"), "not = [toml").unwrap();
        assert_eq!(f.w.free_for_run(key30(), T0 + 11 * MIN).unwrap(), None);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn free_for_run_records_reason() {
        let mut f = dead_session_fx();
        let now = T0 + 11 * MIN;
        let rec = f.w.free_for_run(key30(), now).unwrap().expect("freed");
        assert_eq!(
            rec,
            KillRecord {
                port: 3000,
                command: "bun run dev".into(),
                worktree: f.wt.clone(),
                reason: KillReason::PortConflict,
                at_ms: now,
            }
        );
        assert_eq!(f.p().signals(), vec![(30, TERM), (31, TERM)]);
        assert!(!f.p().is_alive(30));
        assert_eq!(f.state_kills(), vec![rec.clone()]);
        assert_eq!(f.kill_log(), vec![rec]);
        assert!(f.state_servers().is_empty());
        assert!(f.w.port_holder(3000, now).unwrap().is_none());
    }

    fn scripts(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reuse_same_folder_only() {
        let mut f = fx();
        let web = f.wt.join("apps/web");
        let admin = f.wt.join("apps/admin");
        scripts(&web);
        scripts(&admin);
        f.w.tick(T0).unwrap();
        let key = same_command_key(&args(&["vite"]), &web).unwrap();
        // The folder-free key is the same in the other folder.
        assert_eq!(
            same_command_key(&args(&["vite"]), &admin),
            Some(key.clone())
        );

        let found = f.w.find_reusable(&web, &key, T0).unwrap().expect("reused");
        assert_eq!((found.key, found.port), (key30(), 3000));
        assert!(!found.has_logs);
        assert!(f.w.find_reusable(&admin, &key, T0).unwrap().is_none());
        assert!(f.w.find_reusable(&f.wt, &key, T0).unwrap().is_none());
        let other = same_command_key(&args(&["vite", "--host"]), &web).unwrap();
        assert!(f.w.find_reusable(&web, &other, T0).unwrap().is_none());
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn reuse_finds_an_observed_server() {
        let mut f = fx();
        let admin = f.wt.join("apps/admin");
        scripts(&admin);
        f.w.tick(T0).unwrap();
        f.p()
            .spawn(50, 10, START + 50 * SEC, "vite", admin.to_str().unwrap());
        f.p().set_exe(50, "/opt/homebrew/bin/node");
        f.p().add_listener(50, 5173);
        let key = same_command_key(&args(&["npm", "run", "dev"]), &admin).unwrap();
        let found =
            f.w.find_reusable(&admin, &key, T0)
                .unwrap()
                .expect("reused");
        assert_eq!((found.key.pid, found.port), (50, 5173));
        assert_eq!(found.worktree, f.wt);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn the_caller_never_reuses_or_holds_for_itself() {
        let mut f = fx();
        let admin = f.wt.join("apps/admin");
        scripts(&admin);
        f.w.tick(T0).unwrap();
        let me = std::process::id() as i32;
        let dir = admin.to_str().unwrap();
        // An ancestor of this process listens, and so does this process
        // (a socket it inherited), both running the same command here.
        f.p().spawn(51, 10, START + 51 * SEC, "vite", dir);
        f.p().spawn(me, 51, START + 52 * SEC, "vite", dir);
        for pid in [51, me] {
            f.p().set_exe(pid, "/opt/homebrew/bin/node");
        }
        f.p().add_listener(51, 5173);
        f.p().add_listener(me, 5174);
        let key = same_command_key(&args(&["vite"]), &admin).unwrap();
        assert!(f.w.find_reusable(&admin, &key, T0).unwrap().is_none());
        // Taken, but by no one btp may touch.
        for port in [5173, 5174] {
            assert!(matches!(
                f.w.port_holder(port, T0).unwrap(),
                Some(Holder::Unknown)
            ));
        }
        // Another process of the same command is still reused.
        f.p().spawn(60, 10, START + 60 * SEC, "vite", dir);
        f.p().set_exe(60, "/opt/homebrew/bin/node");
        f.p().add_listener(60, 5175);
        let found =
            f.w.find_reusable(&admin, &key, T0)
                .unwrap()
                .expect("reused");
        assert_eq!(found.key.pid, 60);
        assert!(f.p().signals().is_empty());
    }
}
