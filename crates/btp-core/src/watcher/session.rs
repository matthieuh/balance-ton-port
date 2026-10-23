//! The Claude Code hook entry points: what to do when a session process
//! quits (`btp session-end` and its detached waiter `btp _sweep-later`), the
//! agent's intent notes (`btp note`) and the leftovers of an earlier
//! conversation of a live process (`btp stale`).
//!
//! A server still alive when its session ends is one the user chose to keep
//! (Claude Code stops its own background tasks first unless told to move
//! them to the background): the session end never stops anything. Its
//! servers follow the dead-session rule, applied by the watcher, or by the
//! waiter when no watcher runs. Every kill goes through the watcher's pass
//! and checks.

use super::*;
use crate::session_end::stale_conversation_servers;
use crate::state::{GroupServer, SessionEndedGroup};

#[derive(Debug, Default)]
pub struct SessionEndReport {
    /// The servers kept running, `None` when there are none.
    pub group: Option<SessionEndedGroup>,
    /// The app does not hold the watch lock (no holder, or `btp watch`):
    /// the caller posts the notification itself. The group is recorded as
    /// delivered under the state lock before that, so the app never posts
    /// it too.
    pub post_notification: bool,
    /// When the first kept server becomes due under the dead-session rule
    /// (Unix ms): the caller starts the waiter for then. `None` when no
    /// server was kept.
    pub sweep_at: Option<u64>,
    /// What was found and decided, one short line each (`BTP_HOOK_DEBUG`).
    pub trace: Vec<String>,
}

/// One pass of the waiter over one ended session's servers.
#[derive(Debug, Default)]
pub struct SweepOutcome {
    /// Servers stopped by this pass.
    pub killed: Vec<KillRecord>,
    /// When the next of the session's remaining servers becomes due; `None`
    /// when none is tracked any more.
    pub next_at: Option<u64>,
}

/// Why the waiter stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaiterEnd {
    /// No server of that session is tracked any more.
    NothingLeft,
    /// A watcher (the app or `btp watch`) holds the lock: it applies the
    /// rules, the waiter does nothing.
    WatcherRunning,
    /// The time cap was reached with servers still in use.
    Cap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteCmd {
    /// Never stopped early, no alerts, with an optional reason.
    Keep(Option<String>),
    /// The agent is done with it: stopped as soon as it is idle once its
    /// session has ended.
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

/// When `s` becomes due under the dead-session rule once its session is
/// gone: idle `dead_session_idle_s`, or at once for a `--done` note (it is
/// then stopped only if no connection is open).
fn due_at(s: &TrackedServer, cfg: &Config, now_ms: u64) -> u64 {
    if s.is_done() && !s.is_kept() {
        now_ms
    } else {
        s.last_active_ms
            .saturating_add(cfg.dead_session_idle_s.saturating_mul(1000))
    }
}

impl<P: Platform> Watcher<P> {
    /// SessionEnd for the Claude Code process `session_pid`, called from
    /// `caller_pid` (the hook's own process), which must descend from it
    /// and be a Claude Code process (`InvalidInput` otherwise): a shell of
    /// another session can never end this one. Signals nothing.
    ///
    /// Runs the normal pass (link, exclusions, forget the gone). Claude Code
    /// may have removed its session file already: the proven process then
    /// stands in for it, so its untracked listeners still link. Records the
    /// servers kept running (not those protected by another live session in
    /// the same worktree) and when the first becomes due.
    pub fn session_end(
        &mut self,
        caller_pid: i32,
        session_pid: i32,
        conversation: Option<&str>,
        now_ms: u64,
    ) -> io::Result<SessionEndReport> {
        let chain = ancestry(&self.platform, caller_pid);
        let Some(session_proc) = chain.iter().skip(1).find(|p| p.pid == session_pid) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {session_pid} is not an ancestor of this process"),
            ));
        };
        if !is_claude_cli(&session_proc.command) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("pid {session_pid} is not a Claude Code process"),
            ));
        }
        let proc_start = format_proc_start(session_proc.start_time);
        let mut trace = vec![format!(
            "session pid {session_pid}: Claude Code ancestor, started {proc_start}"
        )];
        let mut scan = self.scan();
        let listeners = self.platform.listeners();
        // Only the app delivers groups; with `btp watch` the hook posts.
        let app_running = app_owns_watch(watch_lock_owner(&self.data_dir).as_deref());

        // The quitting process may still look alive: never "another" session.
        let ending =
            |l: &LiveSession| l.session.pid == session_pid && l.session.proc_start == proc_start;
        // Claude Code may remove its session file before the hook runs. The
        // process itself is proven (a Claude Code ancestor of this very
        // process, start time from the platform): it stands in for the
        // file, so its untracked listeners still link, with the same rules.
        let cwd_unknown = session_proc.cwd.is_none();
        let cwd = session_proc.cwd.clone().unwrap_or_default();
        let session_root = canonical(&worktree_root(&cwd));
        if scan.live.iter().any(ending) {
            trace.push("session file: present".into());
        } else {
            trace.push("session file: missing, linking with the process identity".into());
            scan.live.push(LiveSession {
                session: ClaudeSession {
                    pid: session_pid,
                    session_id: conversation.unwrap_or_default().to_string(),
                    cwd,
                    status: crate::sessions::SessionStatus::Unknown,
                    status_updated_at_ms: 0,
                    proc_start: proc_start.clone(),
                    name: String::new(),
                },
                root: session_root.clone(),
            });
        }
        let others = Scan {
            cfg: scan.cfg.clone(),
            live: scan.live.iter().filter(|l| !ending(l)).cloned().collect(),
        };

        let (kept, lines) = with_state(&self.data_dir, |state| {
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
            let mut lines = Vec::new();
            for s in state.servers.iter().filter(|s| !before.contains(&s.key)) {
                lines.push(format!(
                    "linked :{} pid {} ({})",
                    s.port, s.key.pid, s.command
                ));
            }
            // An untracked listener working inside the session's worktree
            // whose ancestry no longer reaches the session (a background
            // task moved out of it): nothing proves it was this session's,
            // so it is left; the trace says so.
            let in_trees: HashSet<i32> = trees.iter().flat_map(|t| t.pids.clone()).collect();
            for l in &listeners {
                if in_trees.contains(&l.pid) || state.servers.iter().any(|s| s.key.pid == l.pid) {
                    continue;
                }
                let Some(info) = self.platform.proc_info(l.pid) else {
                    continue;
                };
                // An unknown session cwd proves nothing: no line.
                let inside = !cwd_unknown
                    && info
                        .cwd
                        .as_deref()
                        .is_some_and(|c| canonical(c).starts_with(&session_root));
                let reaches = ancestry(&self.platform, l.pid)
                    .iter()
                    .any(|p| p.pid == session_pid);
                if inside && !reaches && l.pid != session_pid {
                    lines.push(format!(
                        "left :{} pid {} ({}): in the session worktree but not under the session process",
                        l.port, l.pid, info.command
                    ));
                }
            }
            let mut kept = Vec::new();
            for (s, o) in state.servers.iter().zip(&obs) {
                if s.session_pid != session_pid || s.session_proc_start != proc_start {
                    continue;
                }
                if o.shared_with_live_session {
                    lines.push(format!(
                        ":{} pid {}: shared with another live session, its rules apply",
                        s.port, s.key.pid
                    ));
                    continue;
                }
                let due = due_at(s, &scan.cfg, now_ms);
                lines.push(format!(
                    ":{} pid {} ({}): kept running{}, due at {due}",
                    s.port,
                    s.key.pid,
                    s.command,
                    if o.established > 0 { ", in use" } else { "" }
                ));
                kept.push((s.clone(), o.established > 0, due));
            }
            (kept, lines)
        })?;
        trace.extend(lines);
        if kept.is_empty() {
            trace.push("no server kept running".into());
            return Ok(SessionEndReport {
                trace,
                ..SessionEndReport::default()
            });
        }
        let group = SessionEndedGroup {
            session_pid,
            session_proc_start: proc_start.clone(),
            conversation_id: conversation.unwrap_or_default().to_string(),
            session_name: kept[0].0.session_name.clone(),
            at_ms: now_ms,
            stopped: Vec::new(),
            still_open: kept
                .iter()
                .map(|(s, in_use, _)| GroupServer {
                    key: s.key,
                    port: s.port,
                    tool_label: tool_label(&s.command),
                    in_use: *in_use,
                })
                .collect(),
            delivered: !app_running,
        };
        with_state(&self.data_dir, |state| {
            state.record_session_ended(group.clone())
        })?;
        Ok(SessionEndReport {
            post_notification: !app_running,
            sweep_at: kept.iter().map(|(_, _, due)| *due).min(),
            group: Some(group),
            trace,
        })
    }

    pub fn sweep_session(
        &mut self,
        session_pid: i32,
        proc_start: &str,
        now_ms: u64,
    ) -> io::Result<SweepOutcome> {
        let scan = self.scan();
        let listeners = self.platform.listeners();
        let mine =
            |s: &TrackedServer| s.session_pid == session_pid && s.session_proc_start == proc_start;
        let (to_kill, remaining) = with_state(&self.data_dir, |state| {
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
            let mut to_kill = Vec::new();
            let mut forget = HashSet::new();
            for action in decide(&state.servers, &obs, now_ms, &scan.cfg) {
                match action {
                    Action::Forget { key } => {
                        if state.servers.iter().any(|s| s.key == key && mine(s)) {
                            forget.insert(key);
                        }
                    }
                    Action::Kill { key, reason } => {
                        if let Some(s) = state.servers.iter().find(|s| s.key == key && mine(s)) {
                            to_kill.push((s.clone(), reason));
                        }
                    }
                    Action::Alert { .. } => {}
                }
            }
            state.servers.retain(|s| !forget.contains(&s.key));
            let remaining: Vec<TrackedServer> = state
                .servers
                .iter()
                .filter(|s| mine(s) && !to_kill.iter().any(|(k, _)| k.key == s.key))
                .cloned()
                .collect();
            (to_kill, remaining)
        })?;
        let (killed, gone) = self.execute(to_kill, &scan.cfg, now_ms)?;
        let next_at = remaining
            .iter()
            .filter(|s| !gone.contains(&s.key))
            .map(|s| due_at(s, &scan.cfg, now_ms))
            .min();
        Ok(SweepOutcome { killed, next_at })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sweep_later(
        &mut self,
        session_pid: i32,
        proc_start: &str,
        first_at: u64,
        cap_at: u64,
        poll: Duration,
        clock: &dyn Fn() -> u64,
        sleep: &mut dyn FnMut(Duration),
        notify: &mut dyn FnMut(&[KillRecord]),
    ) -> io::Result<WaiterEnd> {
        let poll_ms = (poll.as_millis() as u64).max(1);
        let mut next = first_at;
        loop {
            let now = clock();
            if now < next {
                if now >= cap_at {
                    return Ok(WaiterEnd::Cap);
                }
                let wait = (next - now).min(poll_ms).min(cap_at - now).max(1);
                sleep(Duration::from_millis(wait));
            }
            if watch_lock_held(&self.data_dir) {
                return Ok(WaiterEnd::WatcherRunning);
            }
            let out = self.sweep_session(session_pid, proc_start, clock())?;
            if !out.killed.is_empty() {
                notify(&out.killed);
            }
            match out.next_at {
                None => return Ok(WaiterEnd::NothingLeft),
                Some(n) => {
                    // Still due (a connection is open): look again after a poll.
                    let now = clock();
                    if now >= cap_at {
                        return Ok(WaiterEnd::Cap);
                    }
                    next = if n <= now { now + poll_ms } else { n };
                }
            }
        }
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
    use crate::platform::format_proc_start;
    use std::cell::Cell;
    use std::fs;

    /// The hook: bash (39) > btp (40), children of the claude process (20).
    const HOOK: i32 = 40;
    const DEAD_IDLE: u64 = 20 * MIN;

    fn hook(f: &Fx) {
        f.p().spawn(39, 20, START + 39 * SEC, "bash hook.sh", "/");
        f.p()
            .spawn(HOOK, 39, START + 40 * SEC, "btp session-end", "/");
    }

    fn end(f: &mut Fx, now: u64) -> SessionEndReport {
        f.w.session_end(HOOK, 20, Some("sess-a"), now).unwrap()
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

    fn has_line(r: &SessionEndReport, parts: &[&str]) -> bool {
        r.trace.iter().any(|l| parts.iter().all(|p| l.contains(p)))
    }

    fn proc_start20() -> String {
        format_proc_start(START + 20 * SEC)
    }

    /// The claude process (and the hook) exited after the session end.
    fn quit(f: &Fx) {
        for pid in [HOOK, 39, 20] {
            f.p().remove_proc(pid);
        }
    }

    fn sweep(f: &mut Fx, now: u64) -> SweepOutcome {
        f.w.sweep_session(20, &proc_start20(), now).unwrap()
    }

    // Session end: never stops anything.

    #[test]
    fn session_end_signals_nothing_and_records_the_kept_servers() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert!(r.post_notification);
        let g = r.group.expect("a group");
        assert!(g.stopped.is_empty());
        assert_eq!(g.still_open, vec![gs(3000, "Bun dev", false)]);
        assert_eq!(g.session_pid, 20);
        assert_eq!(g.session_proc_start, proc_start20());
        assert_eq!(g.conversation_id, "sess-a");
        assert_eq!(g.session_name, "wt");
        assert!(g.delivered, "posted by the hook itself");
        let state = with_state(f.data.path(), |s| s.session_ended.clone()).unwrap();
        assert_eq!(state, vec![g]);
        // Due once idle for dead_session_idle_s since its last activity.
        assert_eq!(r.sweep_at, Some(T0 - 10 * MIN + DEAD_IDLE));
        assert_eq!(f.state_servers().len(), 1);
        assert!(f.state_kills().is_empty());
    }

    #[test]
    fn a_server_in_use_at_session_end_is_due_later() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().set_established(30, 3000, 1);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert_eq!(r.group.unwrap().still_open, vec![gs(3000, "Bun dev", true)]);
        assert_eq!(r.sweep_at, Some(T0 + DEAD_IDLE));
    }

    #[test]
    fn a_done_note_is_due_at_once() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.w.note(HOOK, 3000, NoteCmd::Done, T0).unwrap();
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert_eq!(r.sweep_at, Some(T0));
    }

    #[test]
    fn without_a_session_file_the_process_identity_links_the_servers() {
        // Claude Code removed its session file before the hook ran, and no
        // watcher ever saw the server.
        let mut f = fx();
        hook(&f);
        fs::remove_file(f.claude.path().join("sessions/20.json")).unwrap();
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        let g = r.group.clone().unwrap();
        assert_eq!(g.still_open, vec![gs(3000, "Bun dev", false)]);
        assert_eq!(g.session_name, "wt");
        assert!(has_line(&r, &["session file", "missing"]), "{:?}", r.trace);
        assert!(has_line(&r, &["linked", ":3000"]), "{:?}", r.trace);
        let s = f.state_servers();
        assert_eq!((s[0].session_pid, s[0].session_id.as_str()), (20, "sess-a"));
    }

    #[test]
    fn without_a_session_file_other_process_trees_are_never_linked() {
        let mut f = fx();
        hook(&f);
        fs::remove_file(f.claude.path().join("sessions/20.json")).unwrap();
        // Another claude process, no session file either, same worktree.
        let web = f.wt.join("apps/web");
        f.p()
            .spawn(50, 10, START + 50 * SEC, "claude", f.wt.to_str().unwrap());
        f.p()
            .spawn(60, 50, START + 60 * SEC, "vite", web.to_str().unwrap());
        f.p().add_listener(60, 5173);
        end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert!(f.state_servers().iter().all(|s| s.key.pid != 60));
    }

    #[test]
    fn the_session_pid_must_be_a_claude_ancestor() {
        let mut f = fx();
        hook(&f);
        // The hook's bash is an ancestor too, but not Claude Code.
        let e = f.w.session_end(HOOK, 39, None, T0).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        // A shell of the user's terminal, not under claude (20).
        f.p()
            .spawn(41, 10, START + 41 * SEC, "btp session-end", "/");
        let e = f.w.session_end(41, 20, None, T0).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn a_listener_already_reparented_is_left_and_traced() {
        // A background task moved out of the session before the hook ran:
        // its shell now hangs off launchd, nothing proves it was ours.
        let mut f = fx();
        hook(&f);
        let web = f.wt.join("apps/web");
        f.p().spawn(
            70,
            1,
            START + 70 * SEC,
            "zsh -c python3",
            web.to_str().unwrap(),
        );
        f.p().spawn(
            71,
            70,
            START + 71 * SEC,
            "python3 -m http.server 8790",
            web.to_str().unwrap(),
        );
        f.p().add_listener(71, 8790);
        let r = end(&mut f, T0);
        assert!(f.p().signals().is_empty());
        assert!(f.state_servers().iter().all(|s| s.key.pid != 71));
        assert!(has_line(&r, &["left", ":8790", "71"]), "{:?}", r.trace);
    }

    #[test]
    fn shared_excluded_or_nothing_records_no_group() {
        // Another live session in the same worktree protects the server.
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.p().spawn(50, 10, START + 50 * SEC, "claude", "/");
        f.session(50, "sess-b", &f.wt.clone(), START + 50 * SEC);
        let r = end(&mut f, T0);
        assert!(r.group.is_none() && r.sweep_at.is_none() && !r.post_notification);
        // An exclusion saved after tracking.
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let mut cfg = f.cfg();
        cfg.exclude_ports = vec![3000];
        save_config(f.data.path(), &cfg).unwrap();
        let r = end(&mut f, T0);
        assert!(r.group.is_none() && r.sweep_at.is_none());
        // No server at all.
        let mut f = fx();
        hook(&f);
        f.p().remove_proc(31);
        f.p().remove_proc(30);
        let r = end(&mut f, T0);
        assert!(r.group.is_none() && r.sweep_at.is_none());
        let groups = with_state(f.data.path(), |s| s.session_ended.len()).unwrap();
        assert_eq!(groups, 0);
        assert!(f.p().signals().is_empty());
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
    fn with_btp_watch_holding_the_lock_the_hook_posts() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let _lock = acquire_watch_lock(f.data.path(), "btp watch").unwrap();
        let r = end(&mut f, T0);
        assert!(r.post_notification);
        assert!(r.group.unwrap().delivered);
        let stored = with_state(f.data.path(), |s| s.session_ended.clone()).unwrap();
        assert!(stored[0].delivered, "marked before the hook posts");
    }

    #[test]
    fn only_the_app_as_lock_owner_takes_the_group() {
        assert!(app_owns_watch(Some(APP_OWNER)));
        assert!(!app_owns_watch(Some("btp watch")));
        assert!(!app_owns_watch(Some("")));
        assert!(!app_owns_watch(None));
    }

    // The waiter.

    #[test]
    fn the_waiter_stops_an_idle_server_once_due() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        end(&mut f, T0);
        quit(&f);
        let early = sweep(&mut f, T0);
        assert!(early.killed.is_empty());
        assert_eq!(early.next_at, Some(T0 + 10 * MIN));
        assert!(f.p().signals().is_empty());
        let due = sweep(&mut f, T0 + 10 * MIN);
        assert_eq!(due.killed.len(), 1);
        assert_eq!(due.killed[0].port, 3000);
        assert_eq!(
            due.killed[0].reason,
            KillReason::DeadSessionIdle { idle_ms: DEAD_IDLE }
        );
        assert_eq!(due.next_at, None);
        assert_eq!(f.p().signals(), vec![(30, TERM), (31, TERM)]);
        assert_eq!(f.kill_log().len(), 1);
    }

    #[test]
    fn the_waiter_leaves_a_server_used_meanwhile_and_rearms() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        end(&mut f, T0);
        quit(&f);
        f.p().set_established(31, 3000, 1);
        let used = sweep(&mut f, T0 + 5 * MIN);
        assert!(used.killed.is_empty());
        assert_eq!(used.next_at, Some(T0 + 5 * MIN + DEAD_IDLE));
        f.p().set_established(31, 3000, 0);
        let r = sweep(&mut f, T0 + 10 * MIN);
        assert!(r.killed.is_empty());
        assert_eq!(r.next_at, Some(T0 + 25 * MIN));
        assert!(f.p().signals().is_empty());
        assert_eq!(sweep(&mut f, T0 + 25 * MIN).killed.len(), 1);
    }

    #[test]
    fn the_waiter_never_touches_another_sessions_server() {
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
        f.w.tick(T0 - 60 * MIN).unwrap();
        assert_eq!(f.state_servers().len(), 2);
        // Both sessions dead, both servers idle for an hour.
        quit(&f);
        f.p().remove_proc(50);
        let r = sweep(&mut f, T0);
        assert_eq!(r.killed.len(), 1);
        assert_eq!(r.killed[0].port, 3000);
        assert!(f.p().is_alive(60));
        assert!(!f.p().signals().iter().any(|&(pid, _)| pid == 60));
        assert!(f.state_servers().iter().any(|s| s.key.pid == 60));
    }

    #[test]
    fn the_waiter_waits_for_the_session_to_be_gone() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        end(&mut f, T0);
        // Still running (a session end without a quit): never killed.
        let r = sweep(&mut f, T0 + 60 * MIN);
        assert!(r.killed.is_empty());
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn the_waiter_stops_a_done_server_as_soon_as_idle() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.w.note(HOOK, 3000, NoteCmd::Done, T0).unwrap();
        f.p().set_established(30, 3000, 1);
        end(&mut f, T0);
        quit(&f);
        let r = sweep(&mut f, T0 + SEC);
        assert!(r.killed.is_empty(), "a connection is open");
        f.p().set_established(30, 3000, 0);
        assert_eq!(sweep(&mut f, T0 + 2 * SEC).killed.len(), 1);
    }

    #[test]
    fn a_kept_server_still_follows_the_dead_session_rule() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        f.w.keep(key30()).unwrap();
        end(&mut f, T0);
        quit(&f);
        assert!(sweep(&mut f, T0).killed.is_empty());
        assert_eq!(sweep(&mut f, T0 + 10 * MIN).killed.len(), 1);
    }

    /// A clock the fake sleep advances.
    fn run_waiter(
        f: &mut Fx,
        first_at: u64,
        cap_at: u64,
        on_sleep: &dyn Fn(u64),
    ) -> (WaiterEnd, Vec<u64>, Vec<u16>) {
        let now = Cell::new(T0);
        let mut slept = Vec::new();
        let mut stopped = Vec::new();
        let end =
            f.w.sweep_later(
                20,
                &proc_start20(),
                first_at,
                cap_at,
                Duration::from_secs(60),
                &|| now.get(),
                &mut |d| {
                    now.set(now.get() + d.as_millis() as u64);
                    slept.push(now.get());
                    on_sleep(now.get());
                },
                &mut |k| stopped.extend(k.iter().map(|r| r.port)),
            )
            .unwrap();
        (end, slept, stopped)
    }

    #[test]
    fn the_waiter_loop_polls_then_stops_the_server_and_exits() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        let at = end(&mut f, T0).sweep_at.unwrap();
        quit(&f);
        let (how, slept, stopped) = run_waiter(&mut f, at, T0 + 24 * 60 * MIN, &|_| {});
        assert_eq!(how, WaiterEnd::NothingLeft);
        assert_eq!(stopped, vec![3000]);
        // One wake per minute at most, the last one at the deadline.
        assert_eq!(*slept.last().unwrap(), T0 + 10 * MIN);
        assert_eq!(slept.len(), 10);
    }

    #[test]
    fn the_waiter_does_nothing_while_a_watcher_runs() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        end(&mut f, T0);
        quit(&f);
        let _lock = acquire_watch_lock(f.data.path(), "btp watch").unwrap();
        let (how, _, stopped) = run_waiter(&mut f, T0, T0 + 24 * 60 * MIN, &|_| {});
        assert_eq!(how, WaiterEnd::WatcherRunning);
        assert!(stopped.is_empty());
        assert!(f.p().signals().is_empty());
    }

    #[test]
    fn the_waiter_gives_up_at_the_cap_with_a_server_in_use() {
        let mut f = fx();
        hook(&f);
        tracked(&mut f);
        end(&mut f, T0);
        quit(&f);
        f.p().set_established(30, 3000, 1);
        let (how, slept, stopped) = run_waiter(&mut f, T0, T0 + 3 * 60 * MIN, &|_| {});
        assert_eq!(how, WaiterEnd::Cap);
        assert!(stopped.is_empty());
        assert!(f.p().signals().is_empty());
        assert!(*slept.last().unwrap() <= T0 + 3 * 60 * MIN);
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
        // The restarted server is a new row: no keep note, its own clock.
        let r = end(&mut f, T0 + 10 * MIN);
        // Linked by the second pass, at T0 + 1.
        assert_eq!(r.sweep_at, Some(T0 + 1 + DEAD_IDLE));
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
