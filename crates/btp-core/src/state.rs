use crate::agents::AgentKind;
use crate::model::{KillReason, ServerKey, TrackedServer};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_RECENT_KILLS: usize = 5;
/// Session-ended groups kept in the state (oldest dropped first).
const MAX_SESSION_ENDED: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KillRecord {
    pub port: u16,
    pub command: String,
    pub worktree: PathBuf,
    pub reason: KillReason,
    pub at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub servers: Vec<TrackedServer>,
    #[serde(default)]
    pub recent_kills: Vec<KillRecord>,
    /// What each session end did, for one grouped notification per ended
    /// session (oldest first).
    #[serde(default)]
    pub session_ended: Vec<SessionEndedGroup>,
}

/// One server named in a session-ended group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupServer {
    pub key: ServerKey,
    pub port: u16,
    pub tool_label: String,
    /// Still open only: `true` when a connection kept it (a browser tab),
    /// `false` when it was kept on purpose (Keep or a keep note).
    #[serde(default)]
    pub in_use: bool,
}

/// What `btp session-end` did for one Claude Code process that quit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEndedGroup {
    pub session_pid: i32,
    pub session_proc_start: String,
    /// The conversation id the hook passed, empty when it passed none.
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub session_name: String,
    pub at_ms: u64,
    /// Servers sent SIGTERM.
    #[serde(default)]
    pub stopped: Vec<GroupServer>,
    /// Servers of that session left running.
    #[serde(default)]
    pub still_open: Vec<GroupServer>,
    /// The notification went out (posted by the hook itself when no app
    /// held the watch lock, else set by the app once delivered).
    #[serde(default)]
    pub delivered: bool,
    #[serde(default)]
    pub agent: AgentKind,
}

impl SessionEndedGroup {
    /// This group's identity: the ended process and when it ended.
    pub fn is(&self, session_pid: i32, session_proc_start: &str, at_ms: u64) -> bool {
        self.session_pid == session_pid
            && self.session_proc_start == session_proc_start
            && self.at_ms == at_ms
    }
}

impl State {
    /// Inserts at the front (newest first) and keeps at most 5 records.
    pub fn record_kill(&mut self, r: KillRecord) {
        self.recent_kills.insert(0, r);
        self.recent_kills.truncate(MAX_RECENT_KILLS);
    }

    /// The groups not notified yet (oldest first), each narrowed to its kept
    /// servers still tracked. A group with none left is marked delivered
    /// here: there is nothing to tell any more.
    pub fn pending_session_ended(&mut self) -> Vec<SessionEndedGroup> {
        let tracked: Vec<ServerKey> = self.servers.iter().map(|s| s.key).collect();
        let mut pending = Vec::new();
        for g in self.session_ended.iter_mut().filter(|g| !g.delivered) {
            let mut left = g.clone();
            left.still_open.retain(|s| tracked.contains(&s.key));
            if left.still_open.is_empty() {
                g.delivered = true;
            } else {
                pending.push(left);
            }
        }
        pending
    }

    /// `pending_session_ended`, each returned group marked delivered in the
    /// same state-lock section: whoever claims a group is the only one to
    /// post it. A failed post calls `unmark_session_ended_delivered`.
    pub fn claim_session_ended(&mut self) -> Vec<SessionEndedGroup> {
        let claimed = self.pending_session_ended();
        for g in &claimed {
            self.mark_session_ended_delivered(g.session_pid, &g.session_proc_start, g.at_ms);
        }
        claimed
    }

    /// Hands a claimed group back (its notification could not be added), so
    /// the next pass tries again.
    pub fn unmark_session_ended_delivered(
        &mut self,
        session_pid: i32,
        session_proc_start: &str,
        at_ms: u64,
    ) {
        for g in self
            .session_ended
            .iter_mut()
            .filter(|g| g.is(session_pid, session_proc_start, at_ms))
        {
            g.delivered = false;
        }
    }

    /// The group with this identity, if still recorded.
    pub fn session_ended_group(
        &self,
        session_pid: i32,
        session_proc_start: &str,
        at_ms: u64,
    ) -> Option<&SessionEndedGroup> {
        self.session_ended
            .iter()
            .find(|g| g.is(session_pid, session_proc_start, at_ms))
    }

    /// Records that this group's notification went out.
    pub fn mark_session_ended_delivered(
        &mut self,
        session_pid: i32,
        session_proc_start: &str,
        at_ms: u64,
    ) {
        for g in self
            .session_ended
            .iter_mut()
            .filter(|g| g.is(session_pid, session_proc_start, at_ms))
        {
            g.delivered = true;
        }
    }

    /// Appends a group and keeps the 20 newest.
    pub fn record_session_ended(&mut self, g: SessionEndedGroup) {
        self.session_ended.push(g);
        let extra = self.session_ended.len().saturating_sub(MAX_SESSION_ENDED);
        self.session_ended.drain(..extra);
    }
}

/// The saved state. Missing: empty. Unparsable: moved aside, empty. Any
/// other read error (permissions, I/O) is returned, so the caller never
/// writes over a state it could not read.
fn load(dir: &Path) -> io::Result<State> {
    let path = dir.join("state.json");
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e),
    };
    Ok(match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            let corrupt = dir.join("state.json.corrupt");
            let _ = fs::rename(&path, &corrupt);
            eprintln!(
                "btp: state.json is corrupt ({e}), starting fresh, copy kept at {}",
                corrupt.display()
            );
            State::default()
        }
    })
}

fn write_atomic(dir: &Path, state: &State) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
    let tmp = dir.join(format!("state.json.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        fs::rename(&tmp, dir.join("state.json"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The saved state for a reader that cannot write the data dir (a sandbox):
/// no lock file, no directory creation, never a write. Missing: empty.
/// Unparsable: an `InvalidData` error (the corrupt file is left alone).
pub fn read_state_readonly(dir: &Path) -> io::Result<State> {
    match fs::read(dir.join("state.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e),
    }
}

/// `true` for the errors a sandboxed process gets writing the data dir.
pub fn is_read_only_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem
    )
}

/// Runs `f` on the state under an exclusive lock (`state.lock`, separate from
/// `state.json` so the atomic rename never invalidates it), then writes it back.
pub fn with_state<T>(dir: &Path, f: impl FnOnce(&mut State) -> T) -> io::Result<T> {
    fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("state.lock"))?;
    FileExt::lock(&lock)?;
    let mut state = match load(dir) {
        Ok(s) => s,
        Err(e) => {
            let _ = FileExt::unlock(&lock);
            return Err(e);
        }
    };
    let out = f(&mut state);
    let written = write_atomic(dir, &state);
    let _ = FileExt::unlock(&lock);
    written?;
    Ok(out)
}

pub fn append_kill_log(dir: &Path, r: &KillRecord) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut line = serde_json::to_string(r).map_err(io::Error::other)?;
    line.push('\n');
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("kills.log"))?;
    f.write_all(line.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServerKey;
    use std::sync::Arc;

    fn server(pid: i32) -> TrackedServer {
        TrackedServer {
            key: ServerKey { pid, start_time: 1 },
            port: 3000,
            command: "node".into(),
            worktree: "/tmp/w".into(),
            session_id: "s".into(),
            session_pid: 1,
            session_proc_start: "x".into(),
            session_name: "x".into(),
            first_seen_ms: 0,
            last_active_ms: 0,
            kept: false,
            alerted: false,
            note: None,
            asked_stale: false,
            agent: crate::agents::AgentKind::ClaudeCode,
            postponed_until_ms: None,
        }
    }

    fn rec(at_ms: u64) -> KillRecord {
        KillRecord {
            port: 3000,
            command: "node".into(),
            worktree: "/tmp/w".into(),
            reason: KillReason::Manual,
            at_ms,
        }
    }

    #[test]
    fn missing_state_is_empty() {
        let d = tempfile::tempdir().unwrap();
        let n = with_state(d.path(), |s| (s.servers.len(), s.recent_kills.len())).unwrap();
        assert_eq!(n, (0, 0));
    }

    #[test]
    fn corrupt_state_starts_fresh_and_keeps_copy() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("state.json"), "{not json").unwrap();
        let n = with_state(d.path(), |s| s.servers.len()).unwrap();
        assert_eq!(n, 0);
        assert_eq!(
            fs::read_to_string(d.path().join("state.json.corrupt")).unwrap(),
            "{not json"
        );
    }

    #[test]
    fn unreadable_state_is_an_error_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("state.json");
        fs::write(&path, "{\"servers\": []}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let r = with_state(d.path(), |s| s.servers.push(server(1)));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"servers\": []}");
    }

    #[test]
    fn recent_kills_capped_at_5_newest_first() {
        let mut s = State::default();
        for i in 0..7 {
            s.record_kill(rec(i));
        }
        let ats: Vec<u64> = s.recent_kills.iter().map(|r| r.at_ms).collect();
        assert_eq!(ats, vec![6, 5, 4, 3, 2]);
    }

    #[test]
    fn with_state_persists_changes() {
        let d = tempfile::tempdir().unwrap();
        with_state(d.path(), |s| {
            s.servers.push(server(7));
            s.record_kill(rec(9));
        })
        .unwrap();
        let (n, k) = with_state(d.path(), |s| (s.servers.clone(), s.recent_kills.clone()))
            .map(|(a, b)| (a.len(), b))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(k, vec![rec(9)]);
    }

    #[test]
    fn concurrent_with_state_serializes() {
        let d = tempfile::tempdir().unwrap();
        let dir = Arc::new(d.path().to_path_buf());
        let handles: Vec<_> = (0..2)
            .map(|t| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..100 {
                        with_state(&dir, |s| s.servers.push(server(t * 1000 + i))).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(with_state(&dir, |s| s.servers.len()).unwrap(), 200);
    }

    #[test]
    fn session_ended_reason_is_kind_session_ended() {
        let json = serde_json::to_value(KillReason::SessionEnded).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "session_ended"}));
        let back: KillReason = serde_json::from_value(json).unwrap();
        assert_eq!(back, KillReason::SessionEnded);
    }

    #[test]
    fn unknown_reason_kind_decodes_as_other() {
        let r: KillReason =
            serde_json::from_str(r#"{"kind": "made_up_later", "extra": 3}"#).unwrap();
        assert_eq!(r, KillReason::Other);
        let r: KillRecord = serde_json::from_str(
            r#"{"port": 1, "command": "c", "worktree": "/w", "reason": {"kind": "nope"}, "at_ms": 2}"#,
        )
        .unwrap();
        assert_eq!(r.reason, KillReason::Other);
        let known: KillReason =
            serde_json::from_str(r#"{"kind": "dead_session_idle", "idle_ms": 5}"#).unwrap();
        assert_eq!(known, KillReason::DeadSessionIdle { idle_ms: 5 });
    }

    #[test]
    fn legacy_state_without_new_fields_loads() {
        let d = tempfile::tempdir().unwrap();
        let legacy = r#"{
          "servers": [{
            "key": {"pid": 7, "start_time": 1},
            "port": 3000, "command": "node", "worktree": "/tmp/w",
            "session_id": "s", "session_pid": 1,
            "session_proc_start": "x", "session_name": "x",
            "first_seen_ms": 0, "last_active_ms": 0,
            "kept": true, "alerted": false
          }],
          "recent_kills": [{"port": 3000, "command": "node", "worktree": "/tmp/w",
                            "reason": {"kind": "manual"}, "at_ms": 1}]
        }"#;
        fs::write(d.path().join("state.json"), legacy).unwrap();
        let (servers, kills) =
            with_state(d.path(), |s| (s.servers.clone(), s.recent_kills.clone())).unwrap();
        assert!(!d.path().join("state.json.corrupt").exists());
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].note, None);
        assert_eq!(servers[0].agent, AgentKind::ClaudeCode);
        assert!(!servers[0].asked_stale);
        assert!(servers[0].kept);
        assert_eq!(kills[0].reason, KillReason::Manual);
    }

    #[test]
    fn note_and_asked_stale_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let mut s = server(7);
        s.note = Some(crate::model::Note {
            keep: true,
            done: false,
            reason: Some("testing the form".into()),
            set_at_ms: 42,
        });
        s.asked_stale = true;
        let saved = s.clone();
        with_state(d.path(), |st| st.servers.push(s)).unwrap();
        let back = with_state(d.path(), |st| st.servers.clone()).unwrap();
        assert_eq!(back, vec![saved]);
    }

    fn group(at_ms: u64, pids: &[i32]) -> SessionEndedGroup {
        SessionEndedGroup {
            session_pid: 50,
            session_proc_start: "Thu Oct 22 10:00:00 2026".into(),
            conversation_id: String::new(),
            session_name: "web".into(),
            at_ms,
            stopped: Vec::new(),
            still_open: pids
                .iter()
                .map(|&pid| GroupServer {
                    key: ServerKey { pid, start_time: 1 },
                    port: 3000,
                    tool_label: "Vite".into(),
                    in_use: false,
                })
                .collect(),
            delivered: false,
            agent: crate::agents::AgentKind::ClaudeCode,
        }
    }

    #[test]
    fn pending_groups_list_only_servers_still_tracked() {
        let mut s = State::default();
        s.servers.push(server(1));
        s.record_session_ended(group(10, &[1, 2]));
        let pending = s.pending_session_ended();
        assert_eq!(pending.len(), 1);
        let pids: Vec<i32> = pending[0].still_open.iter().map(|g| g.key.pid).collect();
        assert_eq!(pids, vec![1]);
        // The stored group is left whole and undelivered.
        assert_eq!(s.session_ended[0].still_open.len(), 2);
        assert!(!s.session_ended[0].delivered);
    }

    #[test]
    fn a_group_with_nothing_left_is_marked_delivered_and_not_listed() {
        let mut s = State::default();
        s.record_session_ended(group(10, &[1]));
        assert!(s.pending_session_ended().is_empty());
        assert!(s.session_ended[0].delivered);
    }

    #[test]
    fn delivered_groups_are_not_pending_and_marking_targets_one_group() {
        let mut s = State::default();
        s.servers.push(server(1));
        s.record_session_ended(group(10, &[1]));
        s.record_session_ended(group(20, &[1]));
        s.mark_session_ended_delivered(50, "Thu Oct 22 10:00:00 2026", 10);
        let pending = s.pending_session_ended();
        assert_eq!(
            pending.iter().map(|g| g.at_ms).collect::<Vec<_>>(),
            vec![20]
        );
        // Another start time is another process: untouched.
        s.mark_session_ended_delivered(50, "other", 20);
        assert_eq!(s.pending_session_ended().len(), 1);
        assert!(s
            .session_ended_group(50, "Thu Oct 22 10:00:00 2026", 20)
            .is_some());
        assert!(s
            .session_ended_group(50, "Thu Oct 22 10:00:00 2026", 30)
            .is_none());
    }

    #[test]
    fn a_claimed_group_is_claimed_once_until_handed_back() {
        let mut s = State::default();
        s.servers.push(server(1));
        s.record_session_ended(group(10, &[1]));
        assert_eq!(s.claim_session_ended().len(), 1);
        assert!(s.session_ended[0].delivered);
        assert!(s.claim_session_ended().is_empty());
        s.unmark_session_ended_delivered(50, "Thu Oct 22 10:00:00 2026", 10);
        assert_eq!(s.claim_session_ended().len(), 1);
    }

    #[test]
    fn kill_log_appends_lines() {
        let d = tempfile::tempdir().unwrap();
        append_kill_log(d.path(), &rec(1)).unwrap();
        append_kill_log(d.path(), &rec(2)).unwrap();
        let text = fs::read_to_string(d.path().join("kills.log")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let back: KillRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(back, rec(2));
    }

    #[test]
    fn read_state_readonly_never_writes() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        with_state(d.path(), |s| s.servers.push(server(7))).unwrap();
        let listing = || {
            let mut v: Vec<_> = fs::read_dir(d.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            v.sort();
            v
        };
        let before = listing();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let got = read_state_readonly(d.path());
        let denied = with_state(d.path(), |_| ());
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(got.unwrap().servers.len(), 1);
        assert!(is_read_only_error(&denied.unwrap_err()));
        assert_eq!(listing(), before);
        let empty = tempfile::tempdir().unwrap();
        assert!(read_state_readonly(empty.path())
            .unwrap()
            .servers
            .is_empty());
    }
}
