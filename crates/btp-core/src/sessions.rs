use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Idle,
    Busy,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSession {
    pub pid: i32,
    pub session_id: String,
    pub cwd: PathBuf,
    pub status: SessionStatus,
    pub status_updated_at_ms: u64,
    pub proc_start: String,
}

fn parse_session(text: &str) -> Option<ClaudeSession> {
    let v: Value = serde_json::from_str(text).ok()?;
    let status = match v.get("status")?.as_str()? {
        "idle" => SessionStatus::Idle,
        "busy" => SessionStatus::Busy,
        _ => SessionStatus::Unknown,
    };
    Some(ClaudeSession {
        pid: i32::try_from(v.get("pid")?.as_i64()?).ok()?,
        session_id: v.get("sessionId")?.as_str()?.to_string(),
        cwd: PathBuf::from(v.get("cwd")?.as_str()?),
        status,
        status_updated_at_ms: v.get("statusUpdatedAt")?.as_u64()?,
        proc_start: v.get("procStart")?.as_str()?.to_string(),
    })
}

/// Reads `<dir>/sessions/<pid>.json` for every dir. Files that are not `.json`,
/// are unparsable, or miss a required key are skipped.
pub fn read_sessions(dirs: &[PathBuf]) -> Vec<ClaudeSession> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir.join("sessions")) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Some(s) = parse_session(&text) {
                out.push(s);
            }
        }
    }
    out
}

pub fn session_idle_ms(s: &ClaudeSession, now_ms: u64) -> Option<u64> {
    (s.status == SessionStatus::Idle).then(|| now_ms.saturating_sub(s.status_updated_at_ms))
}

fn expand_tilde(raw: &str, home: &Path) -> PathBuf {
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Every `~/.claude-*` directory holding a `sessions/` subdirectory, sorted.
fn discover_dash_dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(home) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".claude-"))
        .map(|e| e.path())
        .filter(|p| p.join("sessions").is_dir())
        .collect();
    found.sort();
    found
}

/// `~/.claude`, the (tilde-expanded) `claude_config_dir`, then every
/// `~/.claude-*` directory that has a `sessions/` subdirectory; deduplicated,
/// existing only.
pub fn config_dirs_from(home: &Path, claude_config_dir: Option<&str>) -> Vec<PathBuf> {
    let mut candidates = vec![home.join(".claude")];
    if let Some(raw) = claude_config_dir.filter(|r| !r.is_empty()) {
        candidates.push(expand_tilde(raw, home));
    }
    candidates.extend(discover_dash_dirs(home));
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if c.is_dir() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

pub fn default_config_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let env = std::env::var("CLAUDE_CONFIG_DIR").ok();
    config_dirs_from(&home, env.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("../tests/fixtures/sessions/111.json");
    const BUSY: &str = include_str!("../tests/fixtures/sessions/222.json");
    const WEIRD: &str = include_str!("../tests/fixtures/sessions/333.json");
    const NO_ID: &str = include_str!("../tests/fixtures/sessions/444.json");

    fn setup(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<PathBuf>) {
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
    fn parses_real_session_file() {
        let (_d, dirs) = setup(&[("111.json", REAL)]);
        let v = read_sessions(&dirs);
        assert_eq!(v.len(), 1);
        let s = &v[0];
        assert_eq!(s.pid, 111);
        assert_eq!(s.session_id, "00000000-0000-4000-8000-000000000001");
        assert_eq!(s.cwd, PathBuf::from("/Users/test/project"));
        assert_eq!(s.status, SessionStatus::Idle);
        assert_eq!(s.status_updated_at_ms, 1700000500000);
        assert_eq!(s.proc_start, "Thu Oct 15 12:51:40 2026");
    }

    #[test]
    fn skips_key_and_garbage_files() {
        let (_d, dirs) = setup(&[
            ("111.json", REAL),
            ("111.abcd.key", "secret"),
            ("999.json", "not json{"),
            ("444.json", NO_ID),
            ("notes.txt", "hi"),
        ]);
        let v = read_sessions(&dirs);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pid, 111);
    }

    #[test]
    fn unknown_status_maps_to_unknown_and_missing_dir_is_ignored() {
        let (_d, mut dirs) = setup(&[("222.json", BUSY), ("333.json", WEIRD)]);
        dirs.push(PathBuf::from("/nonexistent/btp-test"));
        let mut v = read_sessions(&dirs);
        v.sort_by_key(|s| s.pid);
        assert_eq!(v[0].status, SessionStatus::Busy);
        assert_eq!(v[1].status, SessionStatus::Unknown);
    }

    #[test]
    fn idle_ms_only_when_idle() {
        let mk = |status| ClaudeSession {
            pid: 1,
            session_id: "x".into(),
            cwd: PathBuf::from("/"),
            status,
            status_updated_at_ms: 1000,
            proc_start: String::new(),
        };
        assert_eq!(session_idle_ms(&mk(SessionStatus::Idle), 5000), Some(4000));
        assert_eq!(session_idle_ms(&mk(SessionStatus::Busy), 5000), None);
        assert_eq!(session_idle_ms(&mk(SessionStatus::Unknown), 5000), None);
        assert_eq!(session_idle_ms(&mk(SessionStatus::Idle), 500), Some(0));
    }

    #[test]
    fn tilde_config_dir_is_expanded() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        std::fs::create_dir(home.path().join(".claude-perso")).unwrap();
        let v = config_dirs_from(home.path(), Some("~/.claude-perso"));
        assert_eq!(
            v,
            vec![
                home.path().join(".claude"),
                home.path().join(".claude-perso")
            ]
        );
    }

    #[test]
    fn dirs_deduplicated_and_only_existing() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        let v = config_dirs_from(home.path(), Some("~/.claude"));
        assert_eq!(v, vec![home.path().join(".claude")]);
        let v = config_dirs_from(home.path(), Some("~/nope"));
        assert_eq!(v, vec![home.path().join(".claude")]);
        let none = tempfile::tempdir().unwrap();
        assert!(config_dirs_from(none.path(), None).is_empty());
    }

    #[test]
    fn discovers_claude_dash_dirs_with_sessions() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        std::fs::create_dir(h.join(".claude")).unwrap();
        std::fs::create_dir_all(h.join(".claude-perso/sessions")).unwrap();
        std::fs::create_dir_all(h.join(".claude-alt/sessions")).unwrap();
        std::fs::create_dir(h.join(".claude-nosessions")).unwrap();
        std::fs::write(h.join(".claude-file"), "x").unwrap();
        std::fs::create_dir_all(h.join("claude-nodot/sessions")).unwrap();
        let v = config_dirs_from(h, None);
        assert_eq!(
            v,
            vec![
                h.join(".claude"),
                h.join(".claude-alt"),
                h.join(".claude-perso")
            ]
        );
        // The env dir comes first after ~/.claude and is not repeated.
        let v = config_dirs_from(h, Some("~/.claude-perso"));
        assert_eq!(
            v,
            vec![
                h.join(".claude"),
                h.join(".claude-perso"),
                h.join(".claude-alt")
            ]
        );
    }
}
