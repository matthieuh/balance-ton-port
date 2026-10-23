//! Labels shared by the CLI and the app (through the FFI), so both show a
//! server's state and idle time the same way.

use crate::state::SessionEndedGroup;
use crate::watcher::ServerView;

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

    #[test]
    fn idle_formats() {
        assert_eq!(format_idle(5_000), "<1 min");
        assert_eq!(format_idle(12 * 60_000), "12 min");
        assert_eq!(format_idle(2 * 3_600_000), "2 h");
        assert_eq!(format_idle(3 * 86_400_000), "3 d");
    }
}
