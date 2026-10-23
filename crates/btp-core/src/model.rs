use crate::agents::AgentKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServerKey {
    pub pid: i32,
    pub start_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedServer {
    pub key: ServerKey,
    pub port: u16,
    pub command: String,
    pub worktree: PathBuf,
    /// The conversation id (`sessionId` in the session file) at link time.
    /// One Claude Code process can hold several conversations in a row
    /// (`/clear`, `/resume`); `session_pid` and `session_proc_start` identify
    /// the process, this field the conversation the server was started in.
    pub session_id: String,
    pub session_pid: i32,
    /// The session process start time as Claude Code writes it (`procStart`),
    /// captured at link time. The session is alive while a process with
    /// `session_pid` has this exact start time.
    #[serde(default)]
    pub session_proc_start: String,
    /// The session's display name at link time (its `name`, or the basename
    /// of its working directory). Empty for servers tracked before it existed.
    #[serde(default)]
    pub session_name: String,
    pub first_seen_ms: u64,
    pub last_active_ms: u64,
    pub kept: bool,
    pub alerted: bool,
    /// The agent's intent for this server (`btp note`). Lives with this
    /// process: a restart is a new row, so the note is gone.
    #[serde(default)]
    pub note: Option<Note>,
    /// Already offered once as a leftover of an earlier conversation of the
    /// same process (tier 3): never offered again.
    #[serde(default)]
    pub asked_stale: bool,
    /// The agent whose session started this server.
    #[serde(default)]
    pub agent: AgentKind,
    /// "Postpone 1 h": the dead-session kill waits until then (Unix ms).
    /// Never delays a deleted worktree's kill.
    #[serde(default)]
    pub postponed_until_ms: Option<u64>,
}

impl TrackedServer {
    /// Kept by the user (Keep) or by the agent (`btp note --keep`).
    pub fn is_kept(&self) -> bool {
        self.kept || self.note.as_ref().is_some_and(|n| n.keep)
    }

    /// The agent said it is done with this server (`btp note --done`).
    pub fn is_done(&self) -> bool {
        self.note.as_ref().is_some_and(|n| n.done)
    }
}

/// An agent intent note. It can only make the rules more cautious (`keep`)
/// or speed up a kill they already allow (`done`), never more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub set_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub key: ServerKey,
    pub alive: bool,
    pub established: u32,
    pub worktree_exists: bool,
    pub session_alive: bool,
    pub session_idle_ms: Option<u64>,
    pub shared_with_live_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KillReason {
    WorktreeDeleted,
    DeadSessionIdle {
        idle_ms: u64,
    },
    Manual,
    /// Killed when its Claude Code session process quit (SessionEnd hook).
    SessionEnded,
    /// Any kind this version does not know (written by a newer version).
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Kill { key: ServerKey, reason: KillReason },
    Alert { key: ServerKey, idle_ms: u64 },
    Forget { key: ServerKey },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillTarget {
    Port(u16),
    Pid(i32),
    /// Exactly this process (pid and start time), as the app identifies a row.
    Key(ServerKey),
}
