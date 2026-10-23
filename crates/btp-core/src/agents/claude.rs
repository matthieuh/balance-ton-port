//! Claude Code: sessions come from `<config dir>/sessions/<pid>.json`.

use super::{Agent, AgentKind, AgentSession};
use crate::platform::Platform;
use crate::sessions::{read_sessions, ClaudeSession};
use std::path::PathBuf;

/// The Claude Code CLI itself: argv[0] named `claude`, a runtime running a
/// script named `claude` (`node /opt/homebrew/bin/claude`), or the npm
/// package (`node …/@anthropic-ai/claude-code/cli.js`).
pub fn is_claude_cli(command: &str) -> bool {
    let base = |w: &str| w.rsplit('/').next().unwrap_or("").to_string();
    let mut words = command.split_whitespace();
    let argv0 = base(words.next().unwrap_or(""));
    if argv0 == "claude" || command.contains("@anthropic-ai/claude-code") {
        return true;
    }
    let runtime = matches!(argv0.as_str(), "node" | "nodejs" | "bun" | "deno");
    runtime && words.find(|w| !w.starts_with('-')).map(base).as_deref() == Some("claude")
}

impl From<ClaudeSession> for AgentSession {
    fn from(s: ClaudeSession) -> Self {
        AgentSession {
            agent: AgentKind::ClaudeCode,
            pid: s.pid,
            proc_start: s.proc_start,
            conversation_id: s.session_id,
            other_conversations: Vec::new(),
            cwd: s.cwd,
            name: s.name,
            status: s.status,
            status_updated_at_ms: s.status_updated_at_ms,
        }
    }
}

pub struct ClaudeAgent {
    pub dirs: Vec<PathBuf>,
}

impl Agent for ClaudeAgent {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn is_cli(&self, command: &str) -> bool {
        is_claude_cli(command)
    }

    fn live_sessions(&self, p: &dyn Platform) -> Vec<AgentSession> {
        read_sessions(&self.dirs)
            .into_iter()
            .filter(|s| p.proc_start_string(s.pid).as_deref() == Some(s.proc_start.as_str()))
            .map(AgentSession::from)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FakePlatform;
    use crate::sessions::{read_sessions, SessionStatus};

    const REAL: &str = include_str!("../../tests/fixtures/sessions/111.json");

    fn dir_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("sessions");
        std::fs::create_dir_all(&s).unwrap();
        for (n, c) in files {
            std::fs::write(s.join(n), c).unwrap();
        }
        let p = d.path().to_path_buf();
        (d, vec![p])
    }

    #[test]
    fn claude_session_converts_to_agent_session() {
        let (_d, dirs) = dir_with(&[("111.json", REAL)]);
        let s: AgentSession = read_sessions(&dirs).remove(0).into();
        assert_eq!(s.agent, AgentKind::ClaudeCode);
        assert_eq!(s.pid, 111);
        assert_eq!(s.conversation_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(s.proc_start, "Thu Oct 15 12:51:40 2026");
        assert_eq!(s.status, SessionStatus::Idle);
        assert_eq!(s.name, "project-1d");
        assert!(s.other_conversations.is_empty());
    }

    #[test]
    fn claude_agent_keeps_only_verified_processes() {
        let other = REAL.replace("\"pid\":111", "\"pid\":222");
        let (_d, dirs) = dir_with(&[("111.json", REAL), ("222.json", &other)]);
        let p = FakePlatform::new();
        // 2026-10-15 12:51:40 UTC, in microseconds.
        p.spawn(
            111,
            1,
            1_792_068_700_000_000,
            "claude",
            "/Users/test/project",
        );
        p.spawn(
            222,
            1,
            1_792_068_701_000_000,
            "claude",
            "/Users/test/project",
        );
        let agent = ClaudeAgent { dirs };
        let live = agent.live_sessions(&p);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].pid, 111);
    }
}
