use crate::model::{KillReason, TrackedServer};
use fs4::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const MAX_RECENT_KILLS: usize = 5;

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
}

impl State {
    /// Inserts at the front (newest first) and keeps at most 5 records.
    pub fn record_kill(&mut self, r: KillRecord) {
        self.recent_kills.insert(0, r);
        self.recent_kills.truncate(MAX_RECENT_KILLS);
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
}
