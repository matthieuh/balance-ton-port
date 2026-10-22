//! Pure decisions for the end of a Claude Code session process (SessionEnd
//! hook) and for the leftovers of an earlier conversation of a live process
//! (tier 3). No clock, no I/O: the caller supplies the observations.

use crate::config::Config;
use crate::model::{Observation, ServerKey, TrackedServer};
use crate::watcher::is_excluded;

/// A server counts as in use at session end while it had an established
/// connection less than this long ago (tier 1 needs at least 30 s of quiet).
pub const IN_USE_WINDOW_MS: u64 = 30_000;

/// A server of an earlier conversation is offered (tier 3) once it has been
/// idle for more than this.
pub const STALE_IDLE_MS: u64 = 300_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndAction {
    /// Idle: kill now, reason `session_ended`.
    Kill,
    /// A connection is open now or was seen less than 30 s ago: left alone,
    /// the normal dead-session rule applies later.
    KeepInUse,
    /// Kept by the user or by an agent note: left alone, the normal
    /// dead-session rule applies later.
    KeepNoted,
    /// Not this path's business: gone, excluded, or sharing its worktree
    /// with another live session. Their usual rules apply.
    Skip,
}

fn of_process(s: &TrackedServer, session_pid: i32, session_proc_start: &str) -> bool {
    s.session_pid == session_pid && s.session_proc_start == session_proc_start
}

fn live_obs(obs: &[Observation], key: ServerKey) -> Option<&Observation> {
    obs.iter().find(|o| o.key == key && o.alive)
}

/// What to do with each server of the session process that is quitting
/// (`session_pid` with start time `session_proc_start`, as Claude Code writes
/// `procStart`). Servers of any other process are never returned. In input
/// order, one entry per server of that process.
///
/// A `done` note only lifts the 30 s quiet requirement; a connection open
/// now always keeps the server. Exclusions checked here are the server's own
/// command and recorded port; the watcher re-checks the whole tree before
/// any signal.
pub fn session_end_actions(
    servers: &[TrackedServer],
    obs: &[Observation],
    session_pid: i32,
    session_proc_start: &str,
    now_ms: u64,
    cfg: &Config,
) -> Vec<(ServerKey, EndAction)> {
    servers
        .iter()
        .filter(|s| of_process(s, session_pid, session_proc_start))
        .map(|s| (s.key, end_action(s, live_obs(obs, s.key), now_ms, cfg)))
        .collect()
}

fn end_action(s: &TrackedServer, o: Option<&Observation>, now_ms: u64, cfg: &Config) -> EndAction {
    let Some(o) = o else {
        return EndAction::Skip;
    };
    if is_excluded(&s.command, cfg)
        || cfg.exclude_ports.contains(&s.port)
        || o.shared_with_live_session
    {
        return EndAction::Skip;
    }
    // Checked before any note: nothing makes a server with an open
    // connection killable here.
    if o.established > 0 {
        return EndAction::KeepInUse;
    }
    // A clock that went backwards reads as "just active": keep.
    let quiet_ms = now_ms.saturating_sub(s.last_active_ms);
    let recently_active = s.last_active_ms > now_ms || quiet_ms < IN_USE_WINDOW_MS;
    if recently_active && !s.is_done() {
        return EndAction::KeepInUse;
    }
    if s.is_kept() {
        return EndAction::KeepNoted;
    }
    EndAction::Kill
}

/// Servers that belong to an earlier conversation of the same live process
/// (tier 3), idle for more than 5 minutes, never offered before and not
/// kept. In input order.
pub fn stale_conversation_servers(
    servers: &[TrackedServer],
    obs: &[Observation],
    session_pid: i32,
    session_proc_start: &str,
    current_conversation: &str,
    now_ms: u64,
) -> Vec<ServerKey> {
    servers
        .iter()
        .filter(|s| of_process(s, session_pid, session_proc_start))
        .filter(|s| s.session_id != current_conversation && !s.asked_stale && !s.is_kept())
        .filter(|s| {
            live_obs(obs, s.key).is_some_and(|o| {
                o.established == 0
                    && !o.shared_with_live_session
                    && s.last_active_ms <= now_ms
                    && now_ms - s.last_active_ms > STALE_IDLE_MS
            })
        })
        .map(|s| s.key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Note;

    const SEC: u64 = 1000;
    const MIN: u64 = 60 * SEC;
    const NOW: u64 = 100 * MIN;
    const PID: i32 = 50;
    const START: &str = "Thu Oct 22 10:00:00 2026";

    fn key(pid: i32) -> ServerKey {
        ServerKey { pid, start_time: 1 }
    }

    /// A server of the quitting process, idle for 10 minutes.
    fn server(pid: i32) -> TrackedServer {
        TrackedServer {
            key: key(pid),
            port: 3000,
            command: "node vite".into(),
            worktree: "/tmp/wt".into(),
            session_id: "conv-a".into(),
            session_pid: PID,
            session_proc_start: START.into(),
            session_name: "wt".into(),
            first_seen_ms: 0,
            last_active_ms: NOW - 10 * MIN,
            kept: false,
            alerted: false,
            note: None,
            asked_stale: false,
        }
    }

    fn obs(pid: i32) -> Observation {
        Observation {
            key: key(pid),
            alive: true,
            established: 0,
            worktree_exists: true,
            session_alive: true,
            session_idle_ms: None,
            shared_with_live_session: false,
        }
    }

    fn note(keep: bool, done: bool) -> Option<Note> {
        Some(Note {
            keep,
            done,
            reason: Some("why".into()),
            set_at_ms: 0,
        })
    }

    fn end(s: &TrackedServer, o: &Observation) -> Vec<(ServerKey, EndAction)> {
        session_end_actions(
            std::slice::from_ref(s),
            std::slice::from_ref(o),
            PID,
            START,
            NOW,
            &Config::default(),
        )
    }

    fn one(s: &TrackedServer, o: &Observation) -> EndAction {
        let a = end(s, o);
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].0, s.key);
        a[0].1
    }

    #[test]
    fn idle_server_is_killed() {
        assert_eq!(one(&server(1), &obs(1)), EndAction::Kill);
    }

    #[test]
    fn open_connection_keeps_in_use() {
        let mut o = obs(1);
        o.established = 1;
        assert_eq!(one(&server(1), &o), EndAction::KeepInUse);
    }

    #[test]
    fn activity_less_than_30s_ago_keeps_in_use() {
        let mut s = server(1);
        s.last_active_ms = NOW - 29 * SEC;
        assert_eq!(one(&s, &obs(1)), EndAction::KeepInUse);
        s.last_active_ms = NOW - 30 * SEC;
        assert_eq!(one(&s, &obs(1)), EndAction::Kill);
    }

    #[test]
    fn activity_in_the_future_keeps_in_use() {
        let mut s = server(1);
        s.last_active_ms = NOW + MIN;
        assert_eq!(one(&s, &obs(1)), EndAction::KeepInUse);
    }

    #[test]
    fn kept_server_is_kept_noted() {
        let mut s = server(1);
        s.kept = true;
        assert_eq!(one(&s, &obs(1)), EndAction::KeepNoted);
    }

    #[test]
    fn keep_note_is_kept_noted() {
        let mut s = server(1);
        s.note = note(true, false);
        assert_eq!(one(&s, &obs(1)), EndAction::KeepNoted);
    }

    #[test]
    fn kept_server_in_use_is_in_use() {
        let mut s = server(1);
        s.kept = true;
        let mut o = obs(1);
        o.established = 2;
        assert_eq!(one(&s, &o), EndAction::KeepInUse);
    }

    #[test]
    fn done_note_lifts_the_30s_wait() {
        let mut s = server(1);
        s.last_active_ms = NOW - SEC;
        s.note = note(false, true);
        assert_eq!(one(&s, &obs(1)), EndAction::Kill);
    }

    #[test]
    fn done_note_never_kills_an_open_connection() {
        let mut s = server(1);
        s.note = note(false, true);
        let mut o = obs(1);
        o.established = 1;
        assert_eq!(one(&s, &o), EndAction::KeepInUse);
        s.last_active_ms = NOW;
        assert_eq!(one(&s, &o), EndAction::KeepInUse);
    }

    #[test]
    fn keep_wins_over_done() {
        let mut s = server(1);
        s.note = note(true, true);
        assert_eq!(one(&s, &obs(1)), EndAction::KeepNoted);
        let mut s = server(1);
        s.kept = true;
        s.note = note(false, true);
        assert_eq!(one(&s, &obs(1)), EndAction::KeepNoted);
    }

    #[test]
    fn excluded_command_is_skipped() {
        let mut s = server(1);
        s.command = "node my-mcp-server".into();
        assert_eq!(one(&s, &obs(1)), EndAction::Skip);
        s.note = note(false, true);
        assert_eq!(one(&s, &obs(1)), EndAction::Skip);
    }

    #[test]
    fn excluded_port_is_skipped() {
        let s = server(1);
        let cfg = Config {
            exclude_ports: vec![3000],
            ..Config::default()
        };
        let a = session_end_actions(&[s], &[obs(1)], PID, START, NOW, &cfg);
        assert_eq!(a, vec![(key(1), EndAction::Skip)]);
    }

    #[test]
    fn shared_worktree_is_skipped() {
        let mut s = server(1);
        s.note = note(false, true);
        let mut o = obs(1);
        o.shared_with_live_session = true;
        assert_eq!(one(&s, &o), EndAction::Skip);
    }

    #[test]
    fn gone_server_is_skipped() {
        let mut o = obs(1);
        o.alive = false;
        assert_eq!(one(&server(1), &o), EndAction::Skip);
        let a = session_end_actions(&[server(1)], &[], PID, START, NOW, &Config::default());
        assert_eq!(a, vec![(key(1), EndAction::Skip)]);
        // An observation of another process never stands in for this one.
        let a = session_end_actions(&[server(1)], &[obs(2)], PID, START, NOW, &Config::default());
        assert_eq!(a, vec![(key(1), EndAction::Skip)]);
    }

    #[test]
    fn other_session_servers_are_never_returned() {
        let mut other_pid = server(2);
        other_pid.session_pid = PID + 1;
        other_pid.note = note(false, true);
        let mut reused_pid = server(3);
        reused_pid.session_proc_start = "Thu Oct 22 09:00:00 2026".into();
        let mine = server(4);
        let a = session_end_actions(
            &[other_pid, reused_pid, mine],
            &[obs(2), obs(3), obs(4)],
            PID,
            START,
            NOW,
            &Config::default(),
        );
        assert_eq!(a, vec![(key(4), EndAction::Kill)]);
    }

    #[test]
    fn every_conversation_of_the_process_counts_at_end() {
        let mut older = server(1);
        older.session_id = "conv-old".into();
        let a = session_end_actions(
            &[older, server(2)],
            &[obs(1), obs(2)],
            PID,
            START,
            NOW,
            &Config::default(),
        );
        assert_eq!(
            a,
            vec![(key(1), EndAction::Kill), (key(2), EndAction::Kill)]
        );
    }

    // Tier 3.

    /// A server of an earlier conversation of the live process, idle 10 min.
    fn old(pid: i32) -> TrackedServer {
        let mut s = server(pid);
        s.session_id = "conv-old".into();
        s
    }

    fn stale(servers: &[TrackedServer], obs: &[Observation]) -> Vec<ServerKey> {
        stale_conversation_servers(servers, obs, PID, START, "conv-new", NOW)
    }

    #[test]
    fn earlier_conversation_idle_server_is_offered() {
        assert_eq!(stale(&[old(1)], &[obs(1)]), vec![key(1)]);
    }

    #[test]
    fn current_conversation_is_not_offered() {
        let mut s = old(1);
        s.session_id = "conv-new".into();
        assert!(stale(&[s], &[obs(1)]).is_empty());
    }

    #[test]
    fn already_asked_is_not_offered() {
        let mut s = old(1);
        s.asked_stale = true;
        assert!(stale(&[s], &[obs(1)]).is_empty());
    }

    #[test]
    fn kept_is_not_offered() {
        let mut s = old(1);
        s.kept = true;
        assert!(stale(std::slice::from_ref(&s), &[obs(1)]).is_empty());
        let mut s = old(1);
        s.note = note(true, false);
        assert!(stale(&[s], &[obs(1)]).is_empty());
    }

    #[test]
    fn idle_5_min_or_less_is_not_offered() {
        let mut s = old(1);
        s.last_active_ms = NOW - 5 * MIN;
        assert!(stale(std::slice::from_ref(&s), &[obs(1)]).is_empty());
        s.last_active_ms = NOW - 5 * MIN - 1;
        assert_eq!(stale(&[s], &[obs(1)]), vec![key(1)]);
    }

    #[test]
    fn open_connection_is_not_offered() {
        let mut o = obs(1);
        o.established = 1;
        assert!(stale(&[old(1)], &[o]).is_empty());
    }

    #[test]
    fn gone_or_shared_is_not_offered() {
        let mut o = obs(1);
        o.alive = false;
        assert!(stale(&[old(1)], &[o]).is_empty());
        assert!(stale(&[old(1)], &[]).is_empty());
        let mut o = obs(1);
        o.shared_with_live_session = true;
        assert!(stale(&[old(1)], &[o]).is_empty());
    }

    #[test]
    fn other_process_is_not_offered() {
        let mut s = old(1);
        s.session_pid = PID + 1;
        let mut t = old(2);
        t.session_proc_start = "Thu Oct 22 09:00:00 2026".into();
        assert!(stale(&[s, t], &[obs(1), obs(2)]).is_empty());
    }
}
