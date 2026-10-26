//! Settling a taken port before `btp run` starts a server.
//!
//! This is the gate that decides whether btp may kill another server to free
//! a port. It frees only what the idle rules of the design already allow to
//! be killed (dead session, idle long enough, not kept, not in use, not
//! postponed, not shared with a live session, not excluded). Anything btp
//! cannot vouch for is rerouted around, never killed.

use crate::config::{Config, PortConflict};
use crate::model::{Observation, ServerKey, TrackedServer};
use crate::watcher::is_excluded;

/// Idle time after which a holder with a dead session may be freed, at most.
const FREEABLE_IDLE_MS: u64 = 600_000;

/// Who holds the wanted port.
// Short-lived value built once per `btp run`: boxing buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum Holder {
    /// A server btp tracks, with its current observation.
    Tracked {
        server: TrackedServer,
        obs: Observation,
        idle_ms: u64,
        in_use: bool,
    },
    /// A listener btp sees but does not track (`label` names it for the
    /// message). `excluded` when it matches an exclusion rule.
    Observed { label: String, excluded: bool },
    /// A holder btp cannot see (another user, root): never killed.
    Unknown,
}

/// The command line flags of `btp run` that bear on the conflict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunFlags {
    /// `--free`: act as `free_idle` for this run, under any policy.
    pub free: bool,
    /// `--reroute`: never touch the holder, run on another port (or block
    /// when the port cannot change). Wins over `--free` when both are set.
    pub reroute: bool,
}

/// What `btp run` does about the taken port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Kill this server, then use the port.
    Free(ServerKey),
    /// Run on another port.
    Reroute,
    /// Ask the user. `can_kill` is whether freeing the holder is offered.
    Ask { can_kill: bool },
    /// The port is taken and rerouting is impossible: the caller prints who
    /// holds it and exits 3.
    Block,
}

/// Whether btp may kill this holder to free its port: the session is gone,
/// the server has been idle for `min(10 min, dead_session_idle_s)`, and
/// nothing protects it.
pub fn freeable(
    server: &TrackedServer,
    obs: &Observation,
    idle_ms: u64,
    in_use: bool,
    cfg: &Config,
    now_ms: u64,
) -> bool {
    let threshold = FREEABLE_IDLE_MS.min(cfg.dead_session_idle_s.saturating_mul(1000));
    !obs.session_alive
        && !obs.shared_with_live_session
        && !in_use
        && !server.is_kept()
        && !server.postponed_until_ms.is_some_and(|t| t > now_ms)
        && !is_excluded(&server.command, cfg)
        && !cfg.exclude_ports.contains(&server.port)
        && idle_ms >= threshold
}

/// Decides what to do about a taken port.
///
/// `reroutable` is false when the command's port cannot be changed. Only a
/// freeable tracked holder is ever `Free`; `flags.free` never extends that.
pub fn resolve(
    holder: &Holder,
    reroutable: bool,
    policy: PortConflict,
    flags: RunFlags,
    cfg: &Config,
    now_ms: u64,
) -> Resolution {
    let free_key = match holder {
        Holder::Tracked {
            server,
            obs,
            idle_ms,
            in_use,
        } if freeable(server, obs, *idle_ms, *in_use, cfg, now_ms) => Some(server.key),
        _ => None,
    };
    let effective = if flags.reroute {
        PortConflict::Reroute
    } else if flags.free {
        PortConflict::FreeIdle
    } else {
        policy
    };
    let fallback = if reroutable {
        Resolution::Reroute
    } else {
        Resolution::Block
    };
    match effective {
        PortConflict::Reroute => fallback,
        PortConflict::FreeIdle => free_key.map_or(fallback, Resolution::Free),
        PortConflict::Ask if free_key.is_none() && !reroutable => Resolution::Block,
        PortConflict::Ask => Resolution::Ask {
            can_kill: free_key.is_some(),
        },
    }
}

/// The first free port above `intended`, within 100 tries and never above
/// 65535. `taken` says whether a port is in use.
pub fn next_free_port(intended: u16, taken: &dyn Fn(u16) -> bool) -> Option<u16> {
    (1..=100u32)
        .filter_map(|i| u16::try_from(u32::from(intended) + i).ok())
        .find(|p| !taken(*p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::model::Note;

    const NOW: u64 = 10_000_000;

    fn key() -> ServerKey {
        ServerKey {
            pid: 7,
            start_time: 1,
        }
    }

    fn server() -> TrackedServer {
        TrackedServer {
            key: key(),
            port: 3000,
            command: "node server.js".into(),
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
            agent: AgentKind::ClaudeCode,
            postponed_until_ms: None,
            restarted_from: None,
            session_cwd: None,
        }
    }

    fn obs() -> Observation {
        Observation {
            key: key(),
            alive: true,
            established: 0,
            worktree_exists: true,
            session_alive: false,
            session_idle_ms: None,
            shared_with_live_session: false,
        }
    }

    fn ok(s: &TrackedServer, o: &Observation, cfg: &Config) -> bool {
        freeable(s, o, 600_000, false, cfg, NOW)
    }

    fn tracked(idle_ms: u64) -> Holder {
        Holder::Tracked {
            server: server(),
            obs: obs(),
            idle_ms,
            in_use: false,
        }
    }

    fn observed() -> Holder {
        Holder::Observed {
            label: "node".into(),
            excluded: false,
        }
    }

    const NONE: RunFlags = RunFlags {
        free: false,
        reroute: false,
    };

    fn go(h: &Holder, reroutable: bool, p: PortConflict, f: RunFlags) -> Resolution {
        resolve(h, reroutable, p, f, &Config::default(), NOW)
    }

    #[test]
    fn freeable_dead_and_idle() {
        let cfg = Config::default();
        assert!(freeable(&server(), &obs(), 600_000, false, &cfg, NOW));
        assert!(!freeable(&server(), &obs(), 599_999, false, &cfg, NOW));
    }

    #[test]
    fn freeable_follows_lower_dead_session_idle() {
        let cfg = Config {
            dead_session_idle_s: 120,
            ..Config::default()
        };
        assert!(freeable(&server(), &obs(), 120_000, false, &cfg, NOW));
        assert!(!freeable(&server(), &obs(), 119_999, false, &cfg, NOW));
    }

    #[test]
    fn never_frees_protected() {
        let cfg = Config::default();
        assert!(ok(&server(), &obs(), &cfg), "baseline is freeable");

        let mut o = obs();
        o.session_alive = true;
        assert!(!ok(&server(), &o, &cfg));

        assert!(!freeable(&server(), &obs(), 600_000, true, &cfg, NOW));

        let mut s = server();
        s.kept = true;
        assert!(!ok(&s, &obs(), &cfg));

        let mut s = server();
        s.note = Some(Note {
            keep: true,
            done: false,
            reason: None,
            set_at_ms: 0,
        });
        assert!(!ok(&s, &obs(), &cfg));

        let mut s = server();
        s.postponed_until_ms = Some(NOW + 1);
        assert!(!ok(&s, &obs(), &cfg));

        let mut o = obs();
        o.shared_with_live_session = true;
        assert!(!ok(&server(), &o, &cfg));

        let mut s = server();
        s.command = "docker compose up".into();
        let cfg_cmd = Config {
            exclude_commands: vec!["docker".into()],
            ..Config::default()
        };
        assert!(!ok(&s, &obs(), &cfg_cmd));

        let cfg_port = Config {
            exclude_ports: vec![3000],
            ..Config::default()
        };
        assert!(!ok(&server(), &obs(), &cfg_port));
    }

    #[test]
    fn expired_postpone_does_not_protect() {
        let mut s = server();
        s.postponed_until_ms = Some(NOW);
        assert!(ok(&s, &obs(), &Config::default()));
    }

    #[test]
    fn resolve_free_idle() {
        let p = PortConflict::FreeIdle;
        assert_eq!(
            go(&tracked(600_000), true, p, NONE),
            Resolution::Free(key())
        );
        assert_eq!(go(&tracked(0), true, p, NONE), Resolution::Reroute);
        assert_eq!(go(&observed(), true, p, NONE), Resolution::Reroute);
        assert_eq!(go(&Holder::Unknown, true, p, NONE), Resolution::Reroute);

        assert_eq!(
            go(&tracked(600_000), false, p, NONE),
            Resolution::Free(key())
        );
        assert_eq!(go(&tracked(0), false, p, NONE), Resolution::Block);
        assert_eq!(go(&observed(), false, p, NONE), Resolution::Block);
        assert_eq!(go(&Holder::Unknown, false, p, NONE), Resolution::Block);
    }

    #[test]
    fn unknown_holder_is_never_killed() {
        for policy in [
            PortConflict::FreeIdle,
            PortConflict::Ask,
            PortConflict::Reroute,
        ] {
            let flags = RunFlags {
                free: true,
                reroute: false,
            };
            for reroutable in [true, false] {
                let r = go(&Holder::Unknown, reroutable, policy, flags);
                assert!(!matches!(r, Resolution::Free(_)), "{policy:?}");
                assert_ne!(r, Resolution::Ask { can_kill: true });
            }
        }
    }

    #[test]
    fn resolve_ask() {
        let p = PortConflict::Ask;
        assert_eq!(
            go(&tracked(600_000), true, p, NONE),
            Resolution::Ask { can_kill: true }
        );
        assert_eq!(
            go(&tracked(0), true, p, NONE),
            Resolution::Ask { can_kill: false }
        );
        assert_eq!(
            go(&observed(), true, p, NONE),
            Resolution::Ask { can_kill: false }
        );
        assert_eq!(
            go(&Holder::Unknown, true, p, NONE),
            Resolution::Ask { can_kill: false }
        );
        // Nothing actionable to ask about: block.
        assert_eq!(go(&Holder::Unknown, false, p, NONE), Resolution::Block);
        assert_eq!(go(&observed(), false, p, NONE), Resolution::Block);
    }

    #[test]
    fn reroute_flag_never_frees() {
        let f = RunFlags {
            free: false,
            reroute: true,
        };
        assert_eq!(
            go(&tracked(600_000), false, PortConflict::FreeIdle, f),
            Resolution::Block
        );
        assert_eq!(
            go(&tracked(600_000), false, PortConflict::Ask, f),
            Resolution::Block
        );
        let both = RunFlags {
            free: true,
            reroute: true,
        };
        assert_eq!(
            go(&tracked(600_000), true, PortConflict::FreeIdle, both),
            Resolution::Reroute
        );
    }

    #[test]
    fn ask_policy_with_free_flag_is_decided() {
        let f = RunFlags {
            free: true,
            reroute: false,
        };
        let p = PortConflict::Ask;
        assert_eq!(go(&tracked(0), true, p, f), Resolution::Reroute);
        assert_eq!(go(&tracked(0), false, p, f), Resolution::Block);
    }

    #[test]
    fn resolve_reroute_policy() {
        let p = PortConflict::Reroute;
        assert_eq!(go(&tracked(600_000), true, p, NONE), Resolution::Reroute);
        assert_eq!(go(&tracked(600_000), false, p, NONE), Resolution::Block);
    }

    #[test]
    fn free_flag_never_extends() {
        let f = RunFlags {
            free: true,
            reroute: false,
        };
        for policy in [
            PortConflict::FreeIdle,
            PortConflict::Ask,
            PortConflict::Reroute,
        ] {
            for reroutable in [true, false] {
                assert!(!matches!(
                    go(&tracked(0), reroutable, policy, f),
                    Resolution::Free(_)
                ));
                assert!(!matches!(
                    go(&observed(), reroutable, policy, f),
                    Resolution::Free(_)
                ));
            }
            // It does free what free_idle would free.
            assert_eq!(
                go(&tracked(600_000), true, policy, f),
                Resolution::Free(key())
            );
        }
    }

    #[test]
    fn reroute_flag_overrides() {
        let f = RunFlags {
            free: false,
            reroute: true,
        };
        assert_eq!(
            go(&tracked(600_000), true, PortConflict::FreeIdle, f),
            Resolution::Reroute
        );
        // Not reroutable: the flag cannot be honoured, and nothing is freed.
        assert_eq!(
            go(&tracked(0), false, PortConflict::FreeIdle, f),
            Resolution::Block
        );
    }

    #[test]
    fn next_free_port_skips_taken() {
        assert_eq!(next_free_port(2999, &|p| p == 3000), Some(3001));
        assert_eq!(
            next_free_port(3000, &|p| p == 3000 || p == 3001),
            Some(3002)
        );
        assert_eq!(next_free_port(3000, &|_| true), None);
        assert_eq!(next_free_port(65535, &|_| false), None);
        assert_eq!(next_free_port(65534, &|_| false), Some(65535));
        assert_eq!(next_free_port(3000, &|p| p <= 3099), Some(3100));
        assert_eq!(next_free_port(3000, &|p| p <= 3100), None);
    }
}
