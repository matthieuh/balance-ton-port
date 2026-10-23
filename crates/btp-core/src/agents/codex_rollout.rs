//! Codex rollout (transcript) metadata. Only line 1 (`session_meta`) is ever
//! read, and only the keys listed in `SessionMeta`: nothing of the
//! conversation is parsed, stored or logged.

use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Line 1 is capped here so a pathological file cannot blow up memory.
const META_MAX_BYTES: u64 = 65_536;

const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMeta {
    /// The thread id (also the rollout uuid).
    pub id: String,
    pub started_ms: u64,
    pub cwd: PathBuf,
    pub parent_thread_id: Option<String>,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    payload: Payload,
}

#[derive(Deserialize)]
struct Payload {
    id: String,
    timestamp: String,
    cwd: String,
    #[serde(default)]
    parent_thread_id: Option<String>,
}

/// Parse the `session_meta` line. Any missing or mistyped required key gives `None`.
pub fn parse_session_meta(line: &str) -> Option<SessionMeta> {
    let parsed: Line = serde_json::from_str(line).ok()?;
    if parsed.kind != "session_meta" {
        return None;
    }
    let p = parsed.payload;
    Some(SessionMeta {
        id: p.id,
        started_ms: parse_iso_utc_ms(&p.timestamp)?,
        cwd: PathBuf::from(p.cwd),
        parent_thread_id: p.parent_thread_id,
    })
}

/// Read line 1 of a rollout and parse it.
pub fn read_meta(path: &Path) -> Option<SessionMeta> {
    let file = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(file.take(META_MAX_BYTES))
        .read_line(&mut line)
        .ok()?;
    parse_session_meta(line.trim_end())
}

/// The newest rollout of a thread: `rollout-<ts>-<id>.jsonl` or
/// `rollout-<ts>-<id>_<rollout id>.jsonl` under `sessions/Y/M/D/`.
/// Date dirs are searched newest first, stopping at the first day with a match.
pub fn rollout_for_thread(codex_home: &Path, thread_id: &str) -> Option<PathBuf> {
    if thread_id.is_empty() {
        return None;
    }
    let root = codex_home.join("sessions");
    for day in date_dirs_newest_first(&root) {
        let best = jsonl_files(&day)
            .into_iter()
            .filter(|(path, _)| is_rollout_of(path, thread_id))
            .max_by_key(|(_, mtime)| *mtime);
        if let Some((path, _)) = best {
            return Some(path);
        }
    }
    None
}

/// The newest rollout of today or yesterday (UTC) written since `not_before_ms`
/// whose `session_meta` cwd is `cwd`.
pub fn newest_rollout_for_cwd(
    codex_home: &Path,
    cwd: &Path,
    not_before_ms: u64,
) -> Option<(PathBuf, SessionMeta)> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let want = canonical(cwd);
    let mut best: Option<(u64, PathBuf, SessionMeta)> = None;
    for back in 0..2 {
        let (y, m, d) = civil_from_days(((now_ms - back * DAY_MS) / DAY_MS) as i64);
        let dir = codex_home
            .join("sessions")
            .join(format!("{y:04}"))
            .join(format!("{m:02}"))
            .join(format!("{d:02}"));
        for (path, mtime) in jsonl_files(&dir) {
            if mtime < not_before_ms || best.as_ref().is_some_and(|(t, _, _)| *t >= mtime) {
                continue;
            }
            let Some(meta) = read_meta(&path) else {
                continue;
            };
            if canonical(&meta.cwd) == want {
                best = Some((mtime, path, meta));
            }
        }
    }
    best.map(|(_, path, meta)| (path, meta))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_rollout_of(path: &Path, thread_id: &str) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    if !stem.starts_with("rollout-") {
        return false;
    }
    let tail = format!("-{thread_id}");
    stem.ends_with(&tail) || stem.contains(&format!("{tail}_"))
}

/// `.jsonl` files of a directory with their mtime in ms. Other extensions
/// (compressed rollouts) are ignored.
fn jsonl_files(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let mtime = e
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((e.path(), mtime))
        })
        .collect()
}

fn sorted_subdirs_desc(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by(|a, b| b.cmp(a));
    dirs
}

fn date_dirs_newest_first(root: &Path) -> Vec<PathBuf> {
    sorted_subdirs_desc(root)
        .iter()
        .flat_map(|y| sorted_subdirs_desc(y))
        .flat_map(|m| sorted_subdirs_desc(&m))
        .collect()
}

/// `YYYY-MM-DDThh:mm:ss[.fff]Z` to epoch milliseconds.
fn parse_iso_utc_ms(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<u32>().ok()?,
        d.next()?.parse::<u32>().ok()?,
    );
    if d.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    let (hms, frac) = match time.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (time, None),
    };
    let mut t = hms.split(':');
    let (h, mi, se) = (
        t.next()?.parse::<u64>().ok()?,
        t.next()?.parse::<u64>().ok()?,
        t.next()?.parse::<u64>().ok()?,
    );
    if t.next().is_some() || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let millis = match frac {
        None => 0,
        Some(f) => {
            if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let three: String = f.chars().chain("000".chars()).take(3).collect();
            three.parse::<u64>().ok()?
        }
    };
    let days = days_from_civil(y, mo, da);
    if days < 0 {
        return None;
    }
    Some(days as u64 * DAY_MS + ((h * 60 + mi) * 60 + se) * 1000 + millis)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of `days_from_civil`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    const ID: &str = "0199a3b0-0000-7000-8000-000000000001";

    fn meta_line(id: &str, cwd: &str) -> String {
        format!(
            r#"{{"type":"session_meta","payload":{{"id":"{id}","timestamp":"2026-10-22T09:15:02.123Z","cwd":"{cwd}","source":"cli","base_instructions":"placeholder"}}}}"#
        )
    }

    fn write(path: &Path, body: &str, age_ms: u64) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        let mtime = SystemTime::now() - Duration::from_millis(age_ms);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn today_dir(home: &Path) -> PathBuf {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let (y, m, d) = civil_from_days((now.as_millis() as u64 / DAY_MS) as i64);
        home.join(format!("sessions/{y:04}/{m:02}/{d:02}"))
    }

    #[test]
    fn parses_session_meta_line() {
        let m = parse_session_meta(&meta_line(ID, "/Users/test/web")).unwrap();
        assert_eq!(m.id, ID);
        assert_eq!(m.started_ms, 1_792_660_502_123);
        assert_eq!(m.cwd, PathBuf::from("/Users/test/web"));
        assert_eq!(m.parent_thread_id, None);

        let with_parent = r#"{"type":"session_meta","payload":{"id":"a","timestamp":"2026-10-22T09:15:02Z","cwd":"/x","parent_thread_id":"p"}}"#;
        let m = parse_session_meta(with_parent).unwrap();
        assert_eq!(m.started_ms, 1_792_660_502_000);
        assert_eq!(m.parent_thread_id.as_deref(), Some("p"));
    }

    #[test]
    fn meta_missing_cwd_or_wrong_type_is_none() {
        let no_cwd =
            r#"{"type":"session_meta","payload":{"id":"a","timestamp":"2026-10-22T09:15:02Z"}}"#;
        assert!(parse_session_meta(no_cwd).is_none());
        let other = r#"{"type":"event_msg","payload":{"id":"a","timestamp":"2026-10-22T09:15:02Z","cwd":"/x"}}"#;
        assert!(parse_session_meta(other).is_none());
        let bad_ts =
            r#"{"type":"session_meta","payload":{"id":"a","timestamp":"yesterday","cwd":"/x"}}"#;
        assert!(parse_session_meta(bad_ts).is_none());
        assert!(parse_session_meta("not json").is_none());
    }

    #[test]
    fn finds_rollout_by_thread_id_including_reverted_suffix() {
        let home = tempfile::tempdir().unwrap();
        let day = home.path().join("sessions/2026/10/22");
        let plain = day.join(format!("rollout-2026-10-22T11-15-02-{ID}.jsonl"));
        let reverted = day.join(format!("rollout-2026-10-22T11-15-02-{ID}_abc123.jsonl"));
        let zst = day.join(format!("rollout-2026-10-22T11-15-02-{ID}.jsonl.zst"));
        let other =
            day.join("rollout-2026-10-22T11-15-02-ffffffff-0000-7000-8000-000000000002.jsonl");
        write(&plain, "x", 10_000);
        write(&reverted, "x", 5_000);
        write(&zst, "x", 1_000);
        write(&other, "x", 0);
        assert_eq!(rollout_for_thread(home.path(), ID), Some(reverted));

        // An older day with a match is not reached when a newer day matches.
        let old = home.path().join(format!(
            "sessions/2026/10/21/rollout-2026-10-21T11-15-02-{ID}.jsonl"
        ));
        write(&old, "x", 0);
        assert_eq!(
            rollout_for_thread(home.path(), ID)
                .unwrap()
                .parent()
                .unwrap(),
            day
        );
    }

    #[test]
    fn newest_rollout_for_cwd_matches_meta_cwd() {
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let day = today_dir(home.path());
        let cwd = work.path().to_str().unwrap();
        let mine = day.join("rollout-2026-10-22T11-15-02-aaaa.jsonl");
        let stale = day.join("rollout-2026-10-22T10-00-00-bbbb.jsonl");
        let foreign = day.join("rollout-2026-10-22T12-00-00-cccc.jsonl");
        write(&mine, &meta_line("aaaa", cwd), 10_000);
        write(&stale, &meta_line("bbbb", cwd), 600_000);
        write(
            &foreign,
            &meta_line("cccc", elsewhere.path().to_str().unwrap()),
            0,
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let (path, meta) = newest_rollout_for_cwd(home.path(), work.path(), now - 60_000).unwrap();
        assert_eq!(path, mine);
        assert_eq!(meta.id, "aaaa");
        assert!(newest_rollout_for_cwd(home.path(), work.path(), now + 60_000).is_none());
    }

    #[test]
    fn missing_rollout_is_none() {
        let home = tempfile::tempdir().unwrap();
        assert!(rollout_for_thread(home.path(), ID).is_none());
        assert!(newest_rollout_for_cwd(home.path(), Path::new("/x"), 0).is_none());
        assert!(read_meta(&home.path().join("nope.jsonl")).is_none());
    }
}
