//! Labels shared by the CLI and the app (through the FFI), so both show a
//! server's state and idle time the same way.

use crate::agents::AgentKind;
use crate::history::{Leak, LeakCause, MB};
use crate::model::ServerKey;
use crate::observed::ObservedServer;
use crate::state::SessionEndedGroup;
use crate::watcher::ServerView;
use std::collections::{HashMap, HashSet};

/// A server with no port activity for less than this counts as `active`.
pub const ACTIVE_WINDOW_MS: u64 = 60_000;

/// `active`, `idle` or `session dead`.
pub fn server_state(s: &ServerView) -> &'static str {
    if !s.session_alive {
        "session dead"
    } else if s.idle_ms < ACTIVE_WINDOW_MS {
        "active"
    } else {
        "idle"
    }
}

/// `12 min`, `2 h`, `3 d`; under a minute `<1 min`.
pub fn format_idle(ms: u64) -> String {
    let min = ms / 60_000;
    if min < 1 {
        "<1 min".to_string()
    } else if min < 60 {
        format!("{min} min")
    } else if min < 60 * 24 {
        format!("{} h", min / 60)
    } else {
        format!("{} d", min / (60 * 24))
    }
}

/// `started just now`, `started 4 min ago`, `started 2 d ago`.
pub fn started_label(started_ms: u64, now_ms: u64) -> String {
    let age = now_ms.saturating_sub(started_ms);
    if age < 60_000 {
        "started just now".to_string()
    } else {
        format!("started {} ago", format_idle(age))
    }
}

/// At most this many servers are named per list in a notification.
pub const NAMED_MAX: usize = 4;

/// `a, b, c, d and 2 more`: at most `NAMED_MAX` names.
pub fn name_list(items: Vec<String>) -> String {
    let more = items.len().saturating_sub(NAMED_MAX);
    let shown = items
        .into_iter()
        .take(NAMED_MAX)
        .collect::<Vec<_>>()
        .join(", ");
    if more > 0 {
        format!("{shown} and {more} more")
    } else {
        shown
    }
}

/// `1 server`, `3 servers`.
pub fn servers_word(n: usize) -> String {
    if n == 1 {
        "1 server".to_string()
    } else {
        format!("{n} servers")
    }
}

/// The session-ended notification body, shared by the hook (posted with
/// `osascript`) and the app: `Session ended: 2 servers kept running,
/// stopped after 20 min unused: Vite :5173, Bun dev :3000.`. It names the
/// group's `still_open` servers; `dead_idle_s` is the dead-session delay.
pub fn session_ended_body(g: &SessionEndedGroup, dead_idle_s: u64) -> String {
    let names = g
        .still_open
        .iter()
        .map(|s| format!("{} :{}", s.tool_label, s.port))
        .collect();
    format!(
        "Session ended: {} kept running: {}. Stopped after {} unused, once no session works in that folder.",
        servers_word(g.still_open.len()),
        name_list(names),
        format_idle(dead_idle_s.saturating_mul(1000))
    )
}

/// A scheduled stop this close is "about to stop" (a "Needs you" card).
pub const ABOUT_TO_STOP_MS: u64 = 600_000;

const GB: u64 = 1024 * MB;

/// Why a server needs a decision, for its "Needs you" card.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NeedsYou {
    Leak {
        rss_bytes: u64,
        growth_bytes: i64,
        cause: LeakCause,
    },
    AboutToStop {
        in_ms: u64,
    },
    /// Its session ended and a connection (a browser tab) still holds it.
    SessionEndedOpen,
    /// The live-session idle alert, while it is reported.
    IdleAlert {
        idle_ms: u64,
        agent: AgentKind,
    },
}

/// The one action a card suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestedAction {
    Restart,
    Postpone,
    Stop,
}

impl NeedsYou {
    pub fn suggested_action(&self) -> SuggestedAction {
        match self {
            NeedsYou::Leak { .. } => SuggestedAction::Restart,
            NeedsYou::AboutToStop { .. } => SuggestedAction::Postpone,
            NeedsYou::SessionEndedOpen | NeedsYou::IdleAlert { .. } => SuggestedAction::Stop,
        }
    }
}

impl From<Leak> for NeedsYou {
    fn from(l: Leak) -> Self {
        NeedsYou::Leak {
            rss_bytes: l.rss_bytes,
            growth_bytes: l.growth_bytes,
            cause: l.cause,
        }
    }
}

/// The card of a tracked server, if it needs one. `leak` is its leak not
/// ignored, `session_ended_open` whether it is still open after its session
/// ended, `idle_alert` whether its idle alert is reported. Priority: leak,
/// session ended but open, about to stop, idle alert. A kept server (Keep
/// or a keep note) only ever shows a leak or a session ended but open.
pub fn needs_you(
    view: &ServerView,
    leak: Option<Leak>,
    session_ended_open: bool,
    idle_alert: bool,
) -> Option<NeedsYou> {
    if let Some(l) = leak {
        return Some(l.into());
    }
    if session_ended_open {
        return Some(NeedsYou::SessionEndedOpen);
    }
    if view.kept_by_user || view.note_reason.is_some() {
        return None;
    }
    match view.kill_in_ms {
        Some(in_ms) if in_ms <= ABOUT_TO_STOP_MS => Some(NeedsYou::AboutToStop { in_ms }),
        _ if idle_alert => Some(NeedsYou::IdleAlert {
            idle_ms: view.idle_ms,
            agent: view.agent,
        }),
        _ => None,
    }
}

/// Every "Needs you" card of a pass: tracked servers in `servers` order
/// (`needs_you`), then observed servers, which only ever show a leak.
/// `leaks` holds the leak each card shows, `ended_open` the servers still
/// open after their session ended, `idle_alerts` the servers whose idle
/// alert card shows.
pub fn needs_you_cards(
    servers: &[ServerView],
    observed: &[ObservedServer],
    leaks: &HashMap<ServerKey, Leak>,
    ended_open: &HashSet<ServerKey>,
    idle_alerts: &HashSet<ServerKey>,
) -> Vec<(ServerKey, NeedsYou)> {
    let tracked = servers.iter().filter_map(|v| {
        let n = needs_you(
            v,
            leaks.get(&v.key).copied(),
            ended_open.contains(&v.key),
            idle_alerts.contains(&v.key),
        );
        n.map(|n| (v.key, n))
    });
    let observed = observed
        .iter()
        .filter_map(|o| leaks.get(&o.key).map(|&l| (o.key, NeedsYou::from(l))));
    tracked.chain(observed).collect()
}

/// The one-sentence reason on a card.
pub fn card_reason(n: &NeedsYou) -> String {
    match n {
        NeedsYou::Leak {
            growth_bytes,
            cause: LeakCause::Growth,
            ..
        } => format!(
            "Growing fast: +{} in 10 min",
            memory_label((*growth_bytes).max(0) as u64)
        ),
        NeedsYou::Leak { rss_bytes, .. } => format!("Using {}", memory_label(*rss_bytes)),
        NeedsYou::AboutToStop { in_ms } => {
            format!("Session ended, unused · stops in {}", format_idle(*in_ms))
        }
        NeedsYou::SessionEndedOpen => "Session ended, still open in a browser".to_string(),
        NeedsYou::IdleAlert { idle_ms, agent } => {
            format!("{} session idle {}", agent.label(), format_idle(*idle_ms))
        }
    }
}

/// The status of a tracked server's row: `in use`, `kept by Claude:
/// "<reason>"` (a keep note), `kept by you` (Keep), `stops in N min`, else
/// `idle N min`.
pub fn status_text(view: &ServerView) -> String {
    if view.in_use {
        "in use".to_string()
    } else if let Some(reason) = &view.note_reason {
        let by = format!("kept by {}", view.agent.label());
        if reason.is_empty() {
            by
        } else {
            format!("{by}: \"{reason}\"")
        }
    } else if view.kept_by_user {
        "kept by you".to_string()
    } else if let Some(ms) = view.kill_in_ms {
        format!("stops in {}", format_idle(ms))
    } else {
        format!("idle {}", format_idle(view.idle_ms))
    }
}

/// The status of an observed server's row: `excluded`, `in use`, else
/// `idle N min`. Never a countdown: nothing stops it automatically.
pub fn status_text_observed(o: &ObservedServer, in_use: bool, idle_ms: u64) -> String {
    if o.excluded {
        "excluded".to_string()
    } else if in_use {
        "in use".to_string()
    } else {
        format!("idle {}", format_idle(idle_ms))
    }
}

/// `90 MB` under 1 GB, `2.3 GB` from 1 GB.
pub fn memory_label(bytes: u64) -> String {
    let mb = (bytes + MB / 2) / MB;
    if mb < 1024 {
        format!("{mb} MB")
    } else {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    }
}

/// `4.8 GB dev servers (13%) · apps 26.8 · free 4.4 of 36 GB`: dev servers'
/// memory and share of the machine, everything else in use, what is free,
/// and the machine's memory.
pub fn usage_line(dev: u64, total: u64, used: u64) -> String {
    let gb = |b: u64| b as f64 / GB as f64;
    let pct = if total == 0 {
        0
    } else {
        (dev as f64 * 100.0 / total as f64).round() as u64
    };
    format!(
        "{} dev servers ({pct}%) · apps {:.1} · free {:.1} of {} GB",
        memory_label(dev),
        gb(used.saturating_sub(dev)),
        gb(total.saturating_sub(used)),
        gb(total).round() as u64
    )
}

/// Free memory is under 10 % of the machine's.
pub fn free_is_low(total: u64, used: u64) -> bool {
    total.saturating_sub(used).saturating_mul(10) < total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServerKey;

    fn view(alive: bool, idle_ms: u64) -> ServerView {
        ServerView {
            key: ServerKey {
                pid: 1,
                start_time: 1,
            },
            port: 3000,
            ports: vec![3000],
            command: "vite".into(),
            tool_label: "Vite".into(),
            worktree: "/repo/app".into(),
            worktree_name: "app".into(),
            location_label: "app".into(),
            session_name: "app-1d".into(),
            agent: crate::agents::AgentKind::ClaudeCode,
            started_ms: 0,
            session_alive: alive,
            idle_ms,
            kill_in_ms: None,
            in_use: false,
            postponed_until_ms: None,
            note_reason: None,
            kept_by_user: false,
        }
    }

    #[test]
    fn server_states() {
        assert_eq!(server_state(&view(false, 0)), "session dead");
        assert_eq!(server_state(&view(false, 600_000)), "session dead");
        assert_eq!(server_state(&view(true, 0)), "active");
        assert_eq!(server_state(&view(true, 59_999)), "active");
        assert_eq!(server_state(&view(true, 60_000)), "idle");
        assert_eq!(server_state(&view(true, 600_000)), "idle");
    }

    #[test]
    fn started_labels() {
        let now = 1_800_000_000_000;
        assert_eq!(started_label(now - 30_000, now), "started just now");
        assert_eq!(started_label(now - 4 * 60_000, now), "started 4 min ago");
        assert_eq!(started_label(now - 2 * 86_400_000, now), "started 2 d ago");
        // A clock that went back never underflows.
        assert_eq!(started_label(now + 5_000, now), "started just now");
    }

    #[test]
    fn session_ended_body_names_the_kept_servers() {
        use crate::state::GroupServer;
        let open = |label: &str, port| GroupServer {
            key: ServerKey {
                pid: 9,
                start_time: 9,
            },
            port,
            tool_label: label.into(),
            in_use: false,
        };
        let g = SessionEndedGroup {
            session_pid: 1,
            session_proc_start: "x".into(),
            conversation_id: String::new(),
            session_name: "web".into(),
            at_ms: 0,
            stopped: Vec::new(),
            still_open: vec![open("Vite", 5173), open("Bun dev", 3000)],
            delivered: false,
            agent: crate::agents::AgentKind::ClaudeCode,
        };
        assert_eq!(
            session_ended_body(&g, 1200),
            "Session ended: 2 servers kept running: Vite :5173, Bun dev :3000. Stopped after 20 min unused, once no session works in that folder."
        );
    }

    const MIN: u64 = 60_000;

    fn mb(n: u64) -> u64 {
        n * MB
    }

    fn size_leak(rss: u64) -> Leak {
        Leak {
            rss_bytes: rss,
            growth_bytes: 0,
            cause: LeakCause::Size,
        }
    }

    fn dead(kill_in_ms: u64) -> ServerView {
        ServerView {
            kill_in_ms: Some(kill_in_ms),
            ..view(false, 0)
        }
    }

    #[test]
    fn needs_you_variants_and_actions() {
        let v = view(true, 34 * MIN);
        let n = needs_you(&v, Some(size_leak(mb(3000))), false, false).unwrap();
        assert!(matches!(n, NeedsYou::Leak { .. }));
        assert_eq!(n.suggested_action(), SuggestedAction::Restart);
        let n = needs_you(&dead(8 * MIN), None, false, false).unwrap();
        assert_eq!(n, NeedsYou::AboutToStop { in_ms: 8 * MIN });
        assert_eq!(n.suggested_action(), SuggestedAction::Postpone);
        let n = needs_you(&dead(15 * MIN), None, true, false).unwrap();
        assert_eq!(n, NeedsYou::SessionEndedOpen);
        assert_eq!(n.suggested_action(), SuggestedAction::Stop);
        let n = needs_you(&v, None, false, true).unwrap();
        assert_eq!(
            n,
            NeedsYou::IdleAlert {
                idle_ms: 34 * MIN,
                agent: AgentKind::ClaudeCode
            }
        );
        assert_eq!(n.suggested_action(), SuggestedAction::Stop);
        // Nothing to decide.
        assert_eq!(needs_you(&v, None, false, false), None);
        assert_eq!(
            needs_you(&dead(ABOUT_TO_STOP_MS + 1), None, false, false),
            None
        );
        assert!(needs_you(&dead(ABOUT_TO_STOP_MS), None, false, false).is_some());
    }

    #[test]
    fn cards_list_tracked_then_observed_leaks() {
        let key = |pid| ServerKey { pid, start_time: 1 };
        let a = ServerView {
            key: key(1),
            ..dead(MIN)
        };
        let b = ServerView {
            key: key(2),
            ..view(true, 0)
        };
        let o = |pid| ObservedServer {
            key: key(pid),
            port: 4000,
            ports: vec![4000],
            command: "next dev".into(),
            cwd: None,
            excluded: false,
        };
        let leaks = HashMap::from([(key(2), size_leak(mb(3000))), (key(9), size_leak(mb(3000)))]);
        let idle = HashSet::from([key(8)]);
        let cards = needs_you_cards(&[a, b], &[o(8), o(9)], &leaks, &HashSet::new(), &idle);
        assert_eq!(
            cards,
            vec![
                (key(1), NeedsYou::AboutToStop { in_ms: MIN }),
                (key(2), size_leak(mb(3000)).into()),
                (key(9), size_leak(mb(3000)).into()),
            ]
        );
    }

    #[test]
    fn kept_server_only_shows_leaks() {
        for v in [
            ServerView {
                kept_by_user: true,
                ..dead(MIN)
            },
            ServerView {
                note_reason: Some(String::new()),
                ..dead(MIN)
            },
        ] {
            assert_eq!(needs_you(&v, None, false, true), None);
            assert!(matches!(
                needs_you(&v, Some(size_leak(mb(3000))), false, true),
                Some(NeedsYou::Leak { .. })
            ));
        }
    }

    #[test]
    fn priority_leak_over_about_to_stop() {
        let v = dead(MIN);
        assert!(matches!(
            needs_you(&v, Some(size_leak(mb(3000))), true, true),
            Some(NeedsYou::Leak { .. })
        ));
        assert_eq!(
            needs_you(&v, None, true, true),
            Some(NeedsYou::SessionEndedOpen)
        );
        assert_eq!(
            needs_you(&v, None, false, true),
            Some(NeedsYou::AboutToStop { in_ms: MIN })
        );
    }

    fn all_reasons() -> Vec<String> {
        vec![
            card_reason(&NeedsYou::Leak {
                rss_bytes: mb(1200),
                growth_bytes: mb(600) as i64,
                cause: LeakCause::Growth,
            }),
            card_reason(&size_leak(2355 * MB).into()),
            card_reason(&NeedsYou::AboutToStop { in_ms: 8 * MIN }),
            card_reason(&NeedsYou::SessionEndedOpen),
            card_reason(&NeedsYou::IdleAlert {
                idle_ms: 34 * MIN,
                agent: AgentKind::ClaudeCode,
            }),
            card_reason(&NeedsYou::IdleAlert {
                idle_ms: 34 * MIN,
                agent: AgentKind::Codex,
            }),
        ]
    }

    #[test]
    fn card_reason_copy() {
        assert_eq!(
            all_reasons(),
            [
                "Growing fast: +600 MB in 10 min",
                "Using 2.3 GB",
                "Session ended, unused · stops in 8 min",
                "Session ended, still open in a browser",
                "Claude session idle 34 min",
                "Codex session idle 34 min",
            ]
        );
    }

    fn all_statuses() -> Vec<String> {
        let used = ServerView {
            in_use: true,
            note_reason: Some("demo".into()),
            ..dead(MIN)
        };
        let noted = ServerView {
            note_reason: Some("watching the build".into()),
            kept_by_user: true,
            ..dead(MIN)
        };
        let noted_codex = ServerView {
            note_reason: Some(String::new()),
            agent: AgentKind::Codex,
            ..view(true, 0)
        };
        let kept = ServerView {
            kept_by_user: true,
            ..dead(MIN)
        };
        let observed = |excluded| ObservedServer {
            key: crate::model::ServerKey {
                pid: 2,
                start_time: 2,
            },
            port: 5432,
            ports: vec![5432],
            command: "postgres".into(),
            cwd: None,
            excluded,
        };
        vec![
            status_text(&used),
            status_text(&noted),
            status_text(&noted_codex),
            status_text(&kept),
            status_text(&dead(8 * MIN)),
            status_text(&view(true, 34 * MIN)),
            status_text_observed(&observed(true), true, 0),
            status_text_observed(&observed(false), true, 0),
            status_text_observed(&observed(false), false, 12 * MIN),
        ]
    }

    #[test]
    fn status_text_copy() {
        assert_eq!(
            all_statuses(),
            [
                "in use",
                "kept by Claude: \"watching the build\"",
                "kept by Codex",
                "kept by you",
                "stops in 8 min",
                "idle 34 min",
                "excluded",
                "in use",
                "idle 12 min",
            ]
        );
    }

    #[test]
    fn memory_labels() {
        assert_eq!(memory_label(mb(90)), "90 MB");
        assert_eq!(memory_label(mb(1023)), "1023 MB");
        assert_eq!(memory_label(mb(1024)), "1.0 GB");
        assert_eq!(memory_label(2355 * MB), "2.3 GB");
    }

    fn gb_f(x: f64) -> u64 {
        (x * GB as f64) as u64
    }

    #[test]
    fn usage_line_copy() {
        assert_eq!(
            usage_line(gb_f(4.8), 36 * GB, gb_f(31.6)),
            "4.8 GB dev servers (13%) · apps 26.8 · free 4.4 of 36 GB"
        );
        assert_eq!(
            usage_line(mb(900), 16 * GB, 8 * GB),
            "900 MB dev servers (5%) · apps 7.1 · free 8.0 of 16 GB"
        );
        assert!(free_is_low(1000, 901));
        assert!(!free_is_low(1000, 900));
    }

    #[test]
    fn copy_has_no_em_dash() {
        let mut all = all_reasons();
        all.extend(all_statuses());
        all.push(usage_line(gb_f(4.8), 36 * GB, gb_f(31.6)));
        for s in all {
            assert!(!s.contains('\u{2014}'), "{s}");
        }
    }

    #[test]
    fn idle_formats() {
        assert_eq!(format_idle(5_000), "<1 min");
        assert_eq!(format_idle(12 * 60_000), "12 min");
        assert_eq!(format_idle(2 * 3_600_000), "2 h");
        assert_eq!(format_idle(3 * 86_400_000), "3 d");
    }
}
