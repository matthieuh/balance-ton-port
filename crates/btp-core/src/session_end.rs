//! Pure decision for the leftovers of an earlier conversation of a live
//! Claude Code process (tier 3). No clock, no I/O: the caller supplies the
//! observations. (The end of a session stops nothing at once: its servers
//! follow the dead-session rule, see `Watcher::session_end`.)

use crate::model::{Observation, ServerKey, TrackedServer};

/// A server of an earlier conversation is offered (tier 3) once it has been
/// idle for more than this.
pub const STALE_IDLE_MS: u64 = 300_000;

fn of_process(s: &TrackedServer, session_pid: i32, session_proc_start: &str) -> bool {
    s.session_pid == session_pid && s.session_proc_start == session_proc_start
}

fn live_obs(obs: &[Observation], key: ServerKey) -> Option<&Observation> {
    obs.iter().find(|o| o.key == key && o.alive)
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
            agent: crate::agents::AgentKind::ClaudeCode,
            postponed_until_ms: None,
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
