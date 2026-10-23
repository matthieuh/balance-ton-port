//! The coding agents whose sessions the tool follows. Every agent yields
//! the same `AgentSession`; Claude Code is the first one.

use crate::platform::Platform;
use crate::sessions::SessionStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod claude;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    #[default]
    ClaudeCode,
    Codex,
}

impl AgentKind {
    /// The word shown to the user (`Claude session "web"`).
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude",
            AgentKind::Codex => "Codex",
        }
    }
}

/// One live agent session, whatever the agent.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSession {
    pub agent: AgentKind,
    pub pid: i32,
    /// The process start time formatted like Claude Code's `procStart`.
    pub proc_start: String,
    pub conversation_id: String,
    /// Further conversations held by the same process (empty for Claude).
    pub other_conversations: Vec<String>,
    pub cwd: PathBuf,
    /// The session's display name, empty when the agent gives none.
    pub name: String,
    pub status: SessionStatus,
    pub status_updated_at_ms: u64,
}

pub fn session_idle_ms(s: &AgentSession, now_ms: u64) -> Option<u64> {
    (s.status == SessionStatus::Idle).then(|| now_ms.saturating_sub(s.status_updated_at_ms))
}

pub trait Agent {
    fn kind(&self) -> AgentKind;
    /// `true` when `command` is this agent's CLI.
    fn is_cli(&self, command: &str) -> bool;
    /// Sessions whose process is verified (pid and start time).
    fn live_sessions(&self, p: &dyn Platform) -> Vec<AgentSession>;
    /// The conversation id carried by a process environment, if any.
    fn env_conversation(&self, _env: &[(String, String)]) -> Option<String> {
        None
    }
}

/// Which agent's CLI `command` is, if any.
pub fn is_agent_cli(command: &str) -> Option<AgentKind> {
    claude::is_claude_cli(command).then_some(AgentKind::ClaudeCode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&AgentKind::ClaudeCode).unwrap(),
            "\"claude_code\""
        );
        assert_eq!(
            serde_json::to_string(&AgentKind::Codex).unwrap(),
            "\"codex\""
        );
        assert_eq!(AgentKind::default(), AgentKind::ClaudeCode);
        assert_eq!(AgentKind::ClaudeCode.label(), "Claude");
        assert_eq!(AgentKind::Codex.label(), "Codex");
    }

    #[test]
    fn is_agent_cli_matches_claude_forms() {
        for c in [
            "claude",
            "/opt/homebrew/bin/claude -p x",
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
        ] {
            assert_eq!(is_agent_cli(c), Some(AgentKind::ClaudeCode), "{c}");
        }
        assert_eq!(is_agent_cli("node claude-dashboard.js"), None);
    }

    #[test]
    fn idle_ms_only_when_idle() {
        let mk = |status| AgentSession {
            agent: AgentKind::ClaudeCode,
            pid: 1,
            proc_start: String::new(),
            conversation_id: "x".into(),
            other_conversations: Vec::new(),
            cwd: PathBuf::from("/"),
            name: String::new(),
            status,
            status_updated_at_ms: 1000,
        };
        assert_eq!(session_idle_ms(&mk(SessionStatus::Idle), 5000), Some(4000));
        assert_eq!(session_idle_ms(&mk(SessionStatus::Busy), 5000), None);
        assert_eq!(session_idle_ms(&mk(SessionStatus::Idle), 500), Some(0));
    }
}
