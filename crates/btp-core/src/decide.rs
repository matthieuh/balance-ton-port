use crate::config::Config;
use crate::model::{Action, KillReason, Observation, TrackedServer};

/// Decides what to do with each tracked server. Pure: no clock, no I/O.
/// Emits at most one action per tracked server, in input order. Observations
/// matching no tracked server are ignored.
pub fn decide(
    servers: &[TrackedServer],
    obs: &[Observation],
    now_ms: u64,
    cfg: &Config,
) -> Vec<Action> {
    let dead_idle_ms = cfg.dead_session_idle_s.saturating_mul(1000);
    let alert_ms = cfg.alert_after_s.saturating_mul(1000);
    let mut actions = Vec::new();
    for s in servers {
        let key = s.key;
        let Some(o) = obs.iter().find(|o| o.key == key && o.alive) else {
            actions.push(Action::Forget { key });
            continue;
        };
        if !o.worktree_exists {
            actions.push(Action::Kill {
                key,
                reason: KillReason::WorktreeDeleted,
            });
            continue;
        }
        if o.shared_with_live_session {
            continue;
        }
        let port_idle = now_ms.saturating_sub(s.last_active_ms);
        if !o.session_alive {
            // `btp note --done` (and no Keep): eligible as soon as idle,
            // never while a connection is open.
            let done_and_idle = s.is_done() && !s.is_kept() && o.established == 0;
            if port_idle >= dead_idle_ms || done_and_idle {
                actions.push(Action::Kill {
                    key,
                    reason: KillReason::DeadSessionIdle { idle_ms: port_idle },
                });
            }
            continue;
        }
        if !s.is_kept() && !s.alerted {
            let idle = port_idle.max(o.session_idle_ms.unwrap_or(0));
            if idle >= alert_ms {
                actions.push(Action::Alert { key, idle_ms: idle });
            }
        }
    }
    actions
}

/// Records port activity: any established connection resets the idle clock.
pub fn touch(server: &mut TrackedServer, o: &Observation, now_ms: u64) {
    if o.established > 0 {
        server.last_active_ms = now_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{KillReason, ServerKey};

    const MIN: u64 = 60_000;

    fn key(pid: i32) -> ServerKey {
        ServerKey { pid, start_time: 1 }
    }

    fn server(pid: i32) -> TrackedServer {
        TrackedServer {
            key: key(pid),
            port: 3000,
            command: "node".into(),
            worktree: "/tmp/wt".into(),
            session_id: "s".into(),
            session_pid: 99,
            session_proc_start: "x".into(),
            session_name: "x".into(),
            first_seen_ms: 0,
            last_active_ms: 0,
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

    fn run(s: &TrackedServer, o: &Observation, now: u64) -> Vec<Action> {
        decide(
            std::slice::from_ref(s),
            std::slice::from_ref(o),
            now,
            &Config::default(),
        )
    }

    #[test]
    fn dead_process_is_forgotten_not_killed() {
        let mut o = obs(1);
        o.alive = false;
        o.worktree_exists = false;
        assert_eq!(
            run(&server(1), &o, 99 * MIN),
            vec![Action::Forget { key: key(1) }]
        );
    }

    #[test]
    fn pid_reused_is_forgotten() {
        let a = decide(&[server(1)], &[], 5 * MIN, &Config::default());
        assert_eq!(a, vec![Action::Forget { key: key(1) }]);
        // observation for a different key does not match the tracked server
        let a = decide(&[server(1)], &[obs(2)], 5 * MIN, &Config::default());
        assert_eq!(a, vec![Action::Forget { key: key(1) }]);
    }

    #[test]
    fn worktree_deleted_kills_immediately_even_if_session_alive() {
        let mut o = obs(1);
        o.worktree_exists = false;
        assert_eq!(
            run(&server(1), &o, 0),
            vec![Action::Kill {
                key: key(1),
                reason: KillReason::WorktreeDeleted
            }]
        );
    }

    #[test]
    fn dead_session_kills_after_20_min_port_idle() {
        let mut o = obs(1);
        o.session_alive = false;
        assert!(run(&server(1), &o, 19 * MIN).is_empty());
        assert_eq!(
            run(&server(1), &o, 20 * MIN),
            vec![Action::Kill {
                key: key(1),
                reason: KillReason::DeadSessionIdle { idle_ms: 20 * MIN }
            }]
        );
    }

    #[test]
    fn dead_session_active_port_is_kept() {
        let mut s = server(1);
        let mut o = obs(1);
        o.session_alive = false;
        o.established = 1;
        touch(&mut s, &o, 15 * MIN);
        assert_eq!(s.last_active_ms, 15 * MIN);
        assert!(run(&s, &o, 30 * MIN).is_empty());
        o.established = 0;
        touch(&mut s, &o, 31 * MIN);
        assert_eq!(s.last_active_ms, 15 * MIN);
        assert_eq!(run(&s, &o, 35 * MIN).len(), 1);
    }

    #[test]
    fn resumed_session_same_worktree_never_killed() {
        let mut o = obs(1);
        o.session_alive = false;
        o.shared_with_live_session = true;
        assert!(run(&server(1), &o, 120 * MIN).is_empty());
    }

    #[test]
    fn live_session_idle_30_min_alerts_once() {
        let o = obs(1);
        assert!(run(&server(1), &o, 29 * MIN).is_empty());
        assert_eq!(
            run(&server(1), &o, 30 * MIN),
            vec![Action::Alert {
                key: key(1),
                idle_ms: 30 * MIN
            }]
        );
        let mut s = server(1);
        s.alerted = true;
        assert!(run(&s, &o, 60 * MIN).is_empty());
    }

    #[test]
    fn session_idle_alone_triggers_alert() {
        let mut s = server(1);
        s.last_active_ms = 100 * MIN;
        let mut o = obs(1);
        o.session_idle_ms = Some(31 * MIN);
        assert_eq!(
            run(&s, &o, 101 * MIN),
            vec![Action::Alert {
                key: key(1),
                idle_ms: 31 * MIN
            }]
        );
    }

    #[test]
    fn kept_server_never_alerts() {
        let mut s = server(1);
        s.kept = true;
        assert!(run(&s, &obs(1), 500 * MIN).is_empty());
    }

    #[test]
    fn keep_note_never_alerts() {
        let mut s = server(1);
        s.note = Some(crate::model::Note {
            keep: true,
            done: false,
            reason: None,
            set_at_ms: 0,
        });
        assert!(run(&s, &obs(1), 500 * MIN).is_empty());
    }

    #[test]
    fn keep_notes_never_change_the_dead_session_rule() {
        let mut o = obs(1);
        o.session_alive = false;
        let mut s = server(1);
        s.note = Some(crate::model::Note {
            keep: true,
            done: false,
            reason: None,
            set_at_ms: 0,
        });
        assert!(run(&s, &o, 19 * MIN).is_empty());
        assert_eq!(run(&s, &o, 20 * MIN).len(), 1);
    }

    #[test]
    fn a_done_note_is_eligible_as_soon_as_idle_once_the_session_is_dead() {
        let mut o = obs(1);
        o.session_alive = false;
        let mut s = server(1);
        s.note = Some(crate::model::Note {
            keep: false,
            done: true,
            reason: None,
            set_at_ms: 0,
        });
        assert_eq!(run(&s, &o, MIN).len(), 1);
        // Never with a connection open, never while the session lives,
        // never over the user's Keep.
        o.established = 1;
        assert!(run(&s, &o, MIN).is_empty());
        o.established = 0;
        o.session_alive = true;
        assert!(run(&s, &o, MIN).is_empty());
        o.session_alive = false;
        s.kept = true;
        assert!(run(&s, &o, MIN).is_empty());
    }

    #[test]
    fn new_pid_after_keep_is_a_new_server() {
        let mut old = server(1);
        old.kept = true;
        let new = server(2);
        let a = decide(&[old, new], &[obs(1), obs(2)], 40 * MIN, &Config::default());
        assert_eq!(
            a,
            vec![Action::Alert {
                key: key(2),
                idle_ms: 40 * MIN
            }]
        );
    }

    #[test]
    fn delays_follow_config() {
        let cfg = Config {
            alert_after_s: 60,
            ..Config::default()
        };
        let a = decide(&[server(1)], &[obs(1)], MIN, &cfg);
        assert_eq!(
            a,
            vec![Action::Alert {
                key: key(1),
                idle_ms: MIN
            }]
        );
        let cfg = Config {
            dead_session_idle_s: 60,
            ..Config::default()
        };
        let mut o = obs(1);
        o.session_alive = false;
        assert_eq!(decide(&[server(1)], &[o], MIN, &cfg).len(), 1);
    }

    #[test]
    fn clock_skew_does_not_panic() {
        let mut s = server(1);
        s.last_active_ms = 10 * MIN;
        assert!(run(&s, &obs(1), MIN).is_empty());
    }
}
