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
    DeadSessionIdle { idle_ms: u64 },
    Manual,
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
