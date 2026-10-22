//! The Claude Code hook entry points: what to do when a session process
//! quits (`btp session-end`), the agent's intent notes (`btp note`) and the
//! leftovers of an earlier conversation of a live process (`btp stale`).
//! Every kill goes through the same pass and checks as the watcher's.

use super::*;
use crate::session_end::{
    session_end_actions, stale_conversation_servers, EndAction, IN_USE_WINDOW_MS,
};
use crate::state::{GroupServer, SessionEndedGroup};

/// Time limits of `session_end` (Claude Code gives SessionEnd hooks about
/// 1.5 s in all).
#[derive(Debug, Clone, Copy)]
pub struct EndBudget {
    /// How long to keep sampling connections of the servers about to be
    /// killed (any connection seen keeps the server).
    pub sample_window: Duration,
    pub sample_every: Duration,
    /// SIGTERM is sent to everything first; the survivors at this instant
    /// are returned for a detached SIGKILL stage.
    pub term_until: Instant,
}

#[derive(Debug, Default)]
pub struct SessionEndReport {
    /// What happened, `None` when nothing was stopped or left open.
    pub group: Option<SessionEndedGroup>,
    /// No app holds the watch lock: the caller posts the notification
    /// itself (the group is recorded as delivered).
    pub post_notification: bool,
    /// Processes still running at `term_until`, as (pid, start time).
    pub survivors: Vec<(i32, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteCmd {
    /// Spare it at session end and stop alerts, with an optional reason.
    Keep(Option<String>),
    /// The agent is done with it: killed at session end once idle now.
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteOutcome {
    Set {
        port: u16,
        tool_label: String,
    },
    /// The caller does not descend from a live Claude Code session.
    NoSession,
    /// No tracked server on that port.
    NotTracked,
    /// The server belongs to another session process.
    OtherSession,
}

/// A leftover of an earlier conversation (tier 3), as `btp stale` prints it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StaleServer {
    pub pid: i32,
    pub start_time: u64,
    pub port: u16,
    pub ports: Vec<u16>,
    pub tool_label: String,
    pub command: String,
    pub worktree: PathBuf,
    pub location_label: String,
    pub idle_ms: u64,
    /// The conversation the server was started in.
    pub conversation_id: String,
    /// The agent's note, if it left one (`btp note --keep` is never offered,
    /// so this is a `--done` note's reason at most).
    pub note: Option<String>,
}

impl<P: Platform> Watcher<P> {
    /// SessionEnd for the Claude Code process `session_pid`, called from
    /// `caller_pid` (the hook's own process), which must descend from it:
    /// a shell of another session can never end this one (`InvalidInput`).
    ///
    /// Runs the normal pass first (link, exclusions, forget the gone), then
    /// `session_end_actions` with two adjustments: the quitting process
    /// never counts as another live session in the same worktree, and a
    /// server first seen in this very pass has no activity history, so the
    /// sample decides for it instead of its link time. Servers about to be
    /// killed are sampled for connections until `sample_window`; any
    /// connection keeps the server. Kills go through the watcher's guarded
    /// path (fresh config, exclusions, start times), SIGTERM only; the
    /// survivors at `term_until` are returned for `finish_kill`.
    pub fn session_end(
        &mut self,
        caller_pid: i32,
        session_pid: i32,
        conversation: Option<&str>,
        now_ms: u64,
        budget: &EndBudget,
    ) -> io::Result<SessionEndReport> {
        let chain = ancestry(&self.platform, caller_pid);
        let Some(session_proc) = chain.iter().skip(1).find(|p| p.pid == session_pid) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {session_pid} is not an ancestor of this process"),
            ));
        };
        let proc_start = format_proc_start(session_proc.start_time);
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let app_running = watch_lock_held(&self.data_dir);

        // The quitting process may still look alive: never "another" session.
        let ending =
            |l: &LiveSession| l.session.pid == session_pid && l.session.proc_start == proc_start;
        let others = Scan {
            cfg: scan.cfg.clone(),
            live: scan.live.iter().filter(|l| !ending(l)).cloned().collect(),
        };

        struct Planned {
            server: TrackedServer,
            tree: Tree,
            action: EndAction,
        }
        let plan = with_state(&self.data_dir, |state| {
            let before: HashSet<ServerKey> = state.servers.iter().map(|s| s.key).collect();
            let mut trees = self.refresh(state, &scan, &listeners, now_ms);
            let mut obs: Vec<Observation> = state
                .servers
                .iter()
                .zip(&trees)
                .map(|(s, t)| self.observe_one(s, t, &others, now_ms))
                .collect();
            // Forget the gone, as a pass does.
            let mut i = 0;
            while i < state.servers.len() {
                if obs[i].alive {
                    touch(&mut state.servers[i], &obs[i], now_ms);
                    i += 1;
                } else {
                    state.servers.remove(i);
                    trees.remove(i);
                    obs.remove(i);
                }
            }
            let judged: Vec<TrackedServer> = state
                .servers
                .iter()
                .map(|s| {
                    let mut s = s.clone();
                    if !before.contains(&s.key) && s.last_active_ms == now_ms {
                        s.last_active_ms = now_ms.saturating_sub(IN_USE_WINDOW_MS);
                    }
                    s
                })
                .collect();
            let actions =
                session_end_actions(&judged, &obs, session_pid, &proc_start, now_ms, &scan.cfg);
            actions
                .into_iter()
                .filter_map(|(key, action)| {
                    let i = state.servers.iter().position(|s| s.key == key)?;
                    Some(Planned {
                        server: state.servers[i].clone(),
                        tree: trees[i].clone(),
                        action,
                    })
                })
                .collect::<Vec<_>>()
        })?;

        // Sample the candidates outside the state lock.
        let mut plan = plan;
        let mut seen_active = HashSet::new();
        let sample_end = Instant::now() + budget.sample_window;
        while plan.iter().any(|p| p.action == EndAction::Kill) && Instant::now() < sample_end {
            std::thread::sleep(budget.sample_every.min(sample_end - Instant::now()));
            for p in plan.iter_mut().filter(|p| p.action == EndAction::Kill) {
                let established: u32 = p
                    .tree
                    .activity
                    .iter()
                    .map(|&(pid, port)| self.platform.established(pid, port))
                    .sum();
                if established > 0 {
                    p.action = EndAction::KeepInUse;
                    seen_active.insert(p.server.key);
                }
            }
        }

        let targets: Vec<(TrackedServer, KillReason)> = plan
            .iter()
            .filter(|p| p.action == EndAction::Kill)
            .map(|p| (p.server.clone(), KillReason::SessionEnded))
            .collect();
        let done = self.execute_staged(
            targets,
            &scan.cfg,
            now_ms,
            Stage::TermUntil(budget.term_until),
        )?;

        let entry = |p: &Planned, in_use: bool| GroupServer {
            key: p.server.key,
            port: p.server.port,
            tool_label: tool_label(&p.server.command),
            in_use,
        };
        let stopped: Vec<GroupServer> = plan
            .iter()
            .filter(|p| done.killed.iter().any(|(k, _)| *k == p.server.key))
            .map(|p| entry(p, false))
            .collect();
        let still_open: Vec<GroupServer> = plan
            .iter()
            .filter_map(|p| match p.action {
                EndAction::KeepInUse => Some(entry(p, true)),
                EndAction::KeepNoted => Some(entry(p, false)),
                _ => None,
            })
            .collect();
        let group = (!stopped.is_empty() || !still_open.is_empty()).then(|| SessionEndedGroup {
            session_pid,
            session_proc_start: proc_start.clone(),
            conversation_id: conversation.unwrap_or_default().to_string(),
            session_name: plan
                .first()
                .map(|p| p.server.session_name.clone())
                .unwrap_or_default(),
            at_ms: now_ms,
            stopped,
            still_open,
            delivered: !app_running,
        });
        if group.is_some() || !seen_active.is_empty() {
            with_state(&self.data_dir, |state| {
                for s in state
                    .servers
                    .iter_mut()
                    .filter(|s| seen_active.contains(&s.key))
                {
                    s.last_active_ms = s.last_active_ms.max(now_ms);
                }
                if let Some(g) = &group {
                    state.record_session_ended(g.clone());
                }
            })?;
        }
        Ok(SessionEndReport {
            post_notification: group.is_some() && !app_running,
            group,
            survivors: done.survivors,
        })
    }

    /// The live session process the caller descends from: its nearest
    /// ancestor that matches a live session file (pid and start time).
    fn caller_session<'a>(&self, caller_pid: i32, scan: &'a Scan) -> Option<&'a ClaudeSession> {
        ancestry(&self.platform, caller_pid)
            .iter()
            .skip(1)
            .find_map(|p| {
                let started = format_proc_start(p.start_time);
                scan.live
                    .iter()
                    .find(|l| l.session.pid == p.pid && l.session.proc_start == started)
            })
            .map(|l| &l.session)
    }

    /// Sets the agent's note on the server listening on `port`, when it
    /// belongs to the caller's own live session process. The latest note
    /// replaces the previous one; the user's Keep is never touched.
    pub fn note(
        &mut self,
        caller_pid: i32,
        port: u16,
        cmd: NoteCmd,
        now_ms: u64,
    ) -> io::Result<NoteOutcome> {
        let scan = self.scan();
        let Some(caller) = self.caller_session(caller_pid, &scan).cloned() else {
            return Ok(NoteOutcome::NoSession);
        };
        let listeners = self.platform.listeners();
        with_state(&self.data_dir, |state| {
            let trees = self.refresh(state, &scan, &listeners, now_ms);
            let live = || {
                state
                    .servers
                    .iter()
                    .zip(&trees)
                    .enumerate()
                    .filter(|(_, (_, t))| !t.pids.is_empty())
            };
            let hit = live()
                .find(|(_, (_, t))| t.ports.contains(&port))
                .or_else(|| live().find(|(_, (s, _))| s.port == port))
                .map(|(i, _)| i);
            let Some(i) = hit else {
                return NoteOutcome::NotTracked;
            };
            let s = &mut state.servers[i];
            if s.session_pid != caller.pid || s.session_proc_start != caller.proc_start {
                return NoteOutcome::OtherSession;
            }
            let (keep, done, reason) = match cmd {
                NoteCmd::Keep(reason) => (true, false, reason),
                NoteCmd::Done => (false, true, None),
            };
            s.note = Some(Note {
                keep,
                done,
                reason,
                set_at_ms: now_ms,
            });
            NoteOutcome::Set {
                port: s.port,
                tool_label: tool_label(&s.command),
            }
        })
    }

    /// The leftovers of earlier conversations of the caller's live session
    /// process (tier 3), each returned once: they are marked as asked.
    /// Excluded servers are never returned. `None` when the caller does not
    /// descend from a live session.
    pub fn stale(
        &mut self,
        caller_pid: i32,
        conversation: &str,
        now_ms: u64,
    ) -> io::Result<Option<Vec<StaleServer>>> {
        let scan = self.scan();
        let Some(caller) = self.caller_session(caller_pid, &scan).cloned() else {
            return Ok(None);
        };
        let listeners = self.platform.listeners();
        with_state(&self.data_dir, |state| {
            let trees = self.refresh(state, &scan, &listeners, now_ms);
            let obs: Vec<Observation> = state
                .servers
                .iter()
                .zip(&trees)
                .map(|(s, t)| self.observe_one(s, t, &scan, now_ms))
                .collect();
            for (s, o) in state.servers.iter_mut().zip(&obs) {
                if o.alive {
                    touch(s, o, now_ms);
                }
            }
            let keys = stale_conversation_servers(
                &state.servers,
                &obs,
                caller.pid,
                &caller.proc_start,
                conversation,
                now_ms,
            );
            let mut out = Vec::new();
            for (s, t) in state.servers.iter_mut().zip(&trees) {
                // `refresh` already forgot excluded servers; checked again
                // so the list can never offer one.
                if !keys.contains(&s.key) || self.excluded_now(s, t, &scan.cfg) {
                    continue;
                }
                s.asked_stale = true;
                out.push(StaleServer {
                    pid: s.key.pid,
                    start_time: s.key.start_time,
                    port: s.port,
                    ports: t.ports.clone(),
                    tool_label: tool_label(&s.command),
                    command: s.command.clone(),
                    worktree: s.worktree.clone(),
                    location_label: location_label(&s.worktree),
                    idle_ms: now_ms.saturating_sub(s.last_active_ms),
                    conversation_id: s.session_id.clone(),
                    note: s.note.as_ref().and_then(|n| n.reason.clone()),
                });
            }
            Some(out)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::*;
    use super::*;
    use crate::config::save_config;
    use crate::platform::{format_proc_start, FakePlatform};
    use std::fs;

    /// The hook: bash (39) > btp (40), children of the claude process (20).
    const HOOK: i32 = 40;

    fn hook(f: &Fx) {
        f.p().spawn(39, 20, START + 39 * SEC, "bash hook.sh", "/");
        f.p()
            .spawn(HOOK, 39, START + 40 * SEC, "btp session-end", "/");
    }

    fn now_budget() -> EndBudget {
        EndBudget {
            sample_window: Duration::ZERO,
            sample_every: Duration::from_millis(10),
            term_until: Instant::now(),
        }
    }

    fn end(f: &mut Fx, now: u64) -> SessionEndReport {
        f.w.session_end(HOOK, 20, Some("sess-a"), now, &now_budget())
            .unwrap()
    }

    /// Tracks the server ten minutes before `T0`, as a watcher would have.
    fn tracked(f: &mut Fx) {
        f.w.tick(T0 - 10 * MIN).unwrap();
        assert_eq!(f.state_servers().len(), 1);
    }

    fn gs(port: u16, label: &str, in_use: bool) -> GroupServer {
        GroupServer {
            key: key30(),
            port,
            tool_label: label.into(),
            in_use,
        }
    }

    #[test]
    fn idle_server_of_the_ending_session_is_stopped_and_recorded() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let r = end(&mut f, T0);
        assert_eq!(f.p().signals(), vec![(30, TERM), (31, TERM)]);
        assert!(r.survivors.is_empty());
        assert!(r.post_notification);
        let g = r.group.expect("a group");
        assert_eq!(g.stopped, vec![gs(3000, "Bun dev", false)]);
        assert!(g.still_open.is_empty());
        assert_eq!(g.session_pid, 20);
        assert_eq!(g.session_proc_start, format_proc_start(START + 20 * SEC));
        assert_eq!(g.conversation_id, "sess-a");
        assert_eq!(g.session_name, "wt");
        assert!(g.delivered, "posted by the hook itself");
        let state = with_state(f.data.path(), |s| s.session_ended.clone()).unwrap();
        assert_eq!(state, vec![g]);
        assert!(f.state_servers().is_empty());
        assert_eq!(f.state_kills()[0].reason, KillReason::SessionEnded);
        assert_eq!(f.kill_log()[0].reason, KillReason::SessionEnded);
    }

    #[test]
    fn a_server_first_seen_at_session_end_is_stopped_when_idle() {
        // No watcher ever saw it: the sample decides, not the link time.
        let mut f = fx();
        hook(&f);
        let r = end(&mut f, T0);
        assert_eq!(r.group.unwrap().stopped.len(), 1);
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn open_connection_is_left_and_listed_as_still_open() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().set_established(30, 3000, 1);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        let g = r.group.unwrap();
        assert!(g.stopped.is_empty());
        assert_eq!(g.still_open, vec![gs(3000, "Bun dev", true)]);
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn activity_seen_less_than_30s_ago_keeps_the_server() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().set_established(30, 3000, 1);
        f.w.tick(T0 - 10_000).unwrap();
        f.p().set_established(30, 3000, 0);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert_eq!(r.group.unwrap().still_open.len(), 1);
    }

    #[test]
    fn a_done_note_never_kills_an_open_connection() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let set = f.w.note(HOOK, 3000, NoteCmd::Done, T0).unwrap();
        assert!(matches!(set, NoteOutcome::Set { port: 3000, .. }));
        f.p().set_established(31, 3000, 2);
        end(&mut f, T0);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn a_done_note_lifts_the_30s_wait() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().set_established(30, 3000, 1);
        f.w.tick(T0 - 5_000).unwrap();
        f.p().set_established(30, 3000, 0);
        f.w.note(HOOK, 3000, NoteCmd::Done, T0).unwrap();
        let r = end(&mut f, T0);
        assert_eq!(r.group.unwrap().stopped.len(), 1);
    }

    #[test]
    fn kept_servers_are_left_and_listed_as_kept() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.w.keep(key30()).unwrap();
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert_eq!(
            r.group.unwrap().still_open,
            vec![gs(3000, "Bun dev", false)]
        );

        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let out =
            f.w.note(HOOK, 3000, NoteCmd::Keep(Some("form demo".into())), T0)
                .unwrap();
        assert!(matches!(out, NoteOutcome::Set { .. }));
        end(&mut f, T0);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn the_ending_session_never_counts_as_another_live_session() {
        // Its session file is still there (and read twice, from two config
        // dirs): the server is still killed.
        let mut f = fx();
        hook(&f);
        let second = tempfile::tempdir().unwrap();
        write_session(second.path(), 20, "sess-a", &f.wt, START + 20 * SEC);
        let mut cfg = f.cfg();
        cfg.claude_config_dirs.push(second.path().to_path_buf());
        save_config(f.data.path(), &cfg).unwrap();
        tracked(&mut f);
        end(&mut f, T0);
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn another_live_session_in_the_same_worktree_protects_the_server() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().spawn(50, 10, START + 50 * SEC, "claude", "/");
        f.session(50, "sess-b", &f.wt.clone(), START + 50 * SEC);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert!(r.group.is_none());
    }

    #[test]
    fn exclusions_saved_after_tracking_are_honoured() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().add_listener(31, 8787);
        let mut cfg = f.cfg();
        cfg.exclude_ports = vec![8787];
        save_config(f.data.path(), &cfg).unwrap();
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert!(r.group.is_none());
    }

    #[test]
    fn a_caller_outside_the_session_ends_nothing() {
        let mut f = fx();
        tracked(&mut f);
        // A shell of the user's terminal, not under claude (20).
        f.p()
            .spawn(HOOK, 10, START + 40 * SEC, "btp session-end", "/");
        let e =
            f.w.session_end(HOOK, 20, None, T0, &now_budget())
                .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(f.p().signals().is_empty());
        assert_eq!(f.state_servers().len(), 1);
    }

    #[test]
    fn servers_of_another_session_are_never_touched() {
        let mut f = fx();
        hook(&f);
        let other = f.repo.path().join("other");
        fs::create_dir_all(&other).unwrap();
        f.p()
            .spawn(50, 10, START + 50 * SEC, "claude", other.to_str().unwrap());
        f.p()
            .spawn(60, 50, START + 60 * SEC, "vite", other.to_str().unwrap());
        f.p().add_listener(60, 5173);
        f.session(50, "sess-b", &other, START + 50 * SEC);
        tracked_both(&mut f);
        end(&mut f, T0);
        assert!(f.p().is_alive(60));
        assert!(!f.p().signals().iter().any(|&(pid, _)| pid == 60));
        assert!(!f.p().is_alive(30));
    }

    fn tracked_both(f: &mut Fx) {
        f.w.tick(T0 - 10 * MIN).unwrap();
        assert_eq!(f.state_servers().len(), 2);
    }

    #[test]
    fn with_the_app_running_the_group_waits_for_it() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let _lock = acquire_watch_lock(f.data.path(), "Balance ton port.app").unwrap();
        let r = end(&mut f, T0);
        assert!(!r.post_notification);
        assert!(!r.group.unwrap().delivered);
    }

    #[test]
    fn survivors_are_handed_back_then_finished() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().ignore_sigterm(30);
        let r = end(&mut f, T0);
        assert_eq!(r.survivors, vec![(30, START + 30 * SEC)]);
        assert!(!f.p().signals().contains(&(30, KILL)));
        finish_kill(f.p(), &r.survivors, Instant::now());
        assert!(f.p().signals().contains(&(30, KILL)));
        assert!(!f.p().is_alive(30));
    }

    #[test]
    fn nothing_to_report_records_no_group() {
        let mut f = fx();
        hook(&f);
        f.p().remove_proc(30);
        f.p().remove_proc(31);
        let r = end(&mut f, T0);
        assert!(r.group.is_none());
        assert!(!r.post_notification);
        let groups = with_state(f.data.path(), |s| s.session_ended.len()).unwrap();
        assert_eq!(groups, 0);
    }

    /// Reports a connection on every query after the first `quiet` ones,
    /// as a browser tab reconnecting during the sample.
    struct Reconnects {
        inner: FakePlatform,
        quiet: u32,
        queries: std::sync::atomic::AtomicU32,
    }

    impl Platform for Reconnects {
        fn listeners(&self) -> Vec<Listener> {
            self.inner.listeners()
        }
        fn proc_info(&self, pid: i32) -> Option<ProcInfo> {
            self.inner.proc_info(pid)
        }
        fn established(&self, _pid: i32, _port: u16) -> u32 {
            let n = self
                .queries
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            u32::from(n >= self.quiet)
        }
        fn descendants(&self, pid: i32) -> Vec<i32> {
            self.inner.descendants(pid)
        }
        fn signal(&self, pid: i32, sig: i32) -> bool {
            self.inner.signal(pid, sig)
        }
        fn proc_start_string(&self, pid: i32) -> Option<String> {
            self.inner.proc_start_string(pid)
        }
    }

    #[test]
    fn a_connection_seen_while_sampling_keeps_the_server() {
        let f = fx();
        hook(&f);
        let p = FakePlatform::new();
        for pid in [1, 10, 20, 30, 31, 39, HOOK] {
            p.add_proc(f.p().proc_info(pid).unwrap());
        }
        p.add_listener(30, 3000);
        // The first pass queries each (pid, port) of the tree once (2 of
        // them): quiet then, connected during the sample.
        let flaky = Reconnects {
            inner: p,
            quiet: 2,
            queries: Default::default(),
        };
        let mut w = Watcher::new(flaky, f.data.path().to_path_buf()).with_discovery(Vec::new);
        let budget = EndBudget {
            sample_window: Duration::from_millis(50),
            sample_every: Duration::from_millis(10),
            term_until: Instant::now(),
        };
        let r = w.session_end(HOOK, 20, None, T0, &budget).unwrap();
        assert!(w.platform().inner.signals().is_empty());
        let g = r.group.unwrap();
        assert!(g.stopped.is_empty());
        assert!(g.still_open[0].in_use);
        assert_eq!(f.state_servers()[0].last_active_ms, T0);
    }

    // Notes.

    #[test]
    fn a_note_is_stored_for_the_callers_own_server() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let out =
            f.w.note(HOOK, 3000, NoteCmd::Keep(Some("form demo".into())), T0)
                .unwrap();
        assert_eq!(
            out,
            NoteOutcome::Set {
                port: 3000,
                tool_label: "Bun dev".into()
            }
        );
        let n = f.state_servers()[0].note.clone().unwrap();
        assert!(n.keep && !n.done);
        assert_eq!(n.reason.as_deref(), Some("form demo"));
        assert_eq!(n.set_at_ms, T0);
        // The latest intent replaces the previous one.
        f.w.note(HOOK, 3000, NoteCmd::Done, T0 + 1).unwrap();
        let n = f.state_servers()[0].note.clone().unwrap();
        assert!(!n.keep && n.done);
    }

    #[test]
    fn a_note_from_another_session_is_refused() {
        let mut f = fx();
        tracked(&mut f);
        f.p().spawn(50, 10, START + 50 * SEC, "claude", "/");
        f.session(50, "sess-b", Path::new("/elsewhere"), START + 50 * SEC);
        f.p().spawn(51, 50, START + 51 * SEC, "btp note", "/");
        let out = f.w.note(51, 3000, NoteCmd::Done, T0).unwrap();
        assert_eq!(out, NoteOutcome::OtherSession);
        assert_eq!(f.state_servers()[0].note, None);
    }

    #[test]
    fn a_note_outside_any_session_or_on_an_unknown_port_is_refused() {
        let mut f = fx();
        tracked(&mut f);
        f.p().spawn(51, 10, START + 51 * SEC, "btp note", "/");
        assert_eq!(
            f.w.note(51, 3000, NoteCmd::Done, T0).unwrap(),
            NoteOutcome::NoSession
        );
        hook(&f);
        assert_eq!(
            f.w.note(HOOK, 4444, NoteCmd::Done, T0).unwrap(),
            NoteOutcome::NotTracked
        );
        assert_eq!(f.state_servers()[0].note, None);
    }

    #[test]
    fn a_note_does_not_survive_a_server_restart() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.w.note(HOOK, 3000, NoteCmd::Keep(None), T0).unwrap();
        // Same pid, new process (pid reuse), then a fresh pid.
        f.p().remove_proc(30);
        f.p().remove_proc(31);
        let web = f.wt.join("apps/web");
        f.p().spawn(
            30,
            20,
            START + 90 * SEC,
            "bun run dev",
            web.to_str().unwrap(),
        );
        f.p().add_listener(30, 3000);
        f.w.tick(T0).unwrap();
        f.w.tick(T0 + 1).unwrap();
        let s = f.state_servers();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].key.start_time, START + 90 * SEC);
        assert_eq!(s[0].note, None);
        // And the restarted server is no longer spared at session end.
        let r = end(&mut f, T0 + 10 * MIN);
        assert_eq!(r.group.unwrap().stopped.len(), 1);
    }

    // Tier 3.

    /// The same claude process moved on to conversation `sess-b`.
    fn cleared(f: &Fx) {
        f.session(20, "sess-b", &f.wt.clone(), START + 20 * SEC);
    }

    #[test]
    fn stale_servers_are_offered_once() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        cleared(&f);
        let first = f.w.stale(HOOK, "sess-b", T0).unwrap().unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].port, 3000);
        assert_eq!(first[0].conversation_id, "sess-a");
        assert_eq!(first[0].tool_label, "Bun dev");
        assert!(f.state_servers()[0].asked_stale);
        let again = f.w.stale(HOOK, "sess-b", T0 + MIN).unwrap().unwrap();
        assert!(again.is_empty());
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn stale_leaves_out_recent_and_current_conversation_servers() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        // Current conversation: never offered.
        assert!(f.w.stale(HOOK, "sess-a", T0).unwrap().unwrap().is_empty());
        cleared(&f);
        // Idle 5 min only: not yet, and not marked.
        let r = f.w.stale(HOOK, "sess-b", T0 - 5 * MIN).unwrap().unwrap();
        assert!(r.is_empty());
        assert!(!f.state_servers()[0].asked_stale);
    }

    #[test]
    fn stale_leaves_out_excluded_servers() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        cleared(&f);
        let mut cfg = f.cfg();
        cfg.exclude_commands.push("bun run".into());
        save_config(f.data.path(), &cfg).unwrap();
        assert!(f.w.stale(HOOK, "sess-b", T0).unwrap().unwrap().is_empty());
    }

    #[test]
    fn stale_needs_a_live_session_caller() {
        let mut f = fx();
        tracked(&mut f);
        f.p().spawn(51, 10, START + 51 * SEC, "btp stale", "/");
        assert_eq!(f.w.stale(51, "sess-b", T0).unwrap(), None);
    }
}
