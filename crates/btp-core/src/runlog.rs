//! Logs of servers started through `btp run`: the output of the server, kept
//! in `<data dir>/logs/run/<pid>-<start>.log` so the app and `btp logs` can
//! show it. Separate from the restart logs of `logs/`, which `prune_logs`
//! trims and never looks into this folder.

use crate::model::ServerKey;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// A log never grows past this; beyond it the newest half is kept.
pub const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// A dead server's log is kept this long after its last write.
pub const LOG_KEEP_MS: u64 = 86_400_000;

fn run_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs").join("run")
}

/// `<data dir>/logs/run/<pid>-<start>.log`.
pub fn log_path(data_dir: &Path, key: ServerKey) -> PathBuf {
    run_dir(data_dir).join(format!("{}-{}.log", key.pid, key.start_time))
}

/// Whether the server has a log.
pub fn exists(data_dir: &Path, key: ServerKey) -> bool {
    log_path(data_dir, key).is_file()
}

/// The log of one server, written as it produces output.
pub struct RunLog {
    path: PathBuf,
    file: File,
    size: u64,
}

impl RunLog {
    /// Opens the log of `key` for appending (mode 0600, in a 0700 folder).
    pub fn create(data_dir: &Path, key: ServerKey) -> io::Result<RunLog> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(run_dir(data_dir))?;
        let path = log_path(data_dir, key);
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        Ok(RunLog { path, file, size })
    }

    /// Appends raw bytes, which need not be UTF-8 nor line-aligned. Past
    /// `MAX_LOG_BYTES` the file keeps its newest half, cut at a line start.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        self.size += bytes.len() as u64;
        if self.size > MAX_LOG_BYTES {
            self.compact()?;
        }
        Ok(())
    }

    fn compact(&mut self) -> io::Result<()> {
        let keep = MAX_LOG_BYTES / 2;
        let mut src = File::open(&self.path)?;
        let len = src.metadata()?.len();
        src.seek(SeekFrom::Start(len.saturating_sub(keep)))?;
        let mut tail = Vec::new();
        src.read_to_end(&mut tail)?;
        // Drop the cut line; with no line break at all, keep the bytes.
        let from = if len > keep {
            tail.iter().position(|&b| b == b'\n').map_or(0, |i| i + 1)
        } else {
            0
        };
        let tmp = self.path.with_extension("tmp");
        let mut out = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        out.write_all(&tail[from..])?;
        out.sync_data().ok();
        fs::rename(&tmp, &self.path)?;
        self.file = open_append(&self.path)?;
        self.size = (tail.len() - from) as u64;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// The server a log file name stands for.
fn key_of(path: &Path) -> Option<ServerKey> {
    if path.extension()? != "log" {
        return None;
    }
    let (pid, start) = path.file_stem()?.to_str()?.split_once('-')?;
    Some(ServerKey {
        pid: pid.parse().ok()?,
        start_time: start.parse().ok()?,
    })
}

/// Deletes the logs of servers that are not `live` and were last written
/// more than `LOG_KEEP_MS` before `now_ms`. Other files are left alone.
pub fn cleanup(data_dir: &Path, live: &dyn Fn(ServerKey) -> bool, now_ms: u64) {
    let Ok(entries) = fs::read_dir(run_dir(data_dir)) else {
        return;
    };
    for path in entries.filter_map(Result::ok).map(|e| e.path()) {
        let Some(key) = key_of(&path) else { continue };
        let Some(modified) = fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        else {
            continue;
        };
        let age = now_ms.saturating_sub(modified.as_millis() as u64);
        if age > LOG_KEEP_MS && !live(key) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// The last `n` lines of a log. Invalid UTF-8 is replaced, never an error.
pub fn tail(path: &Path, n: usize) -> io::Result<Vec<String>> {
    let bytes = fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    Ok(lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| (*l).to_string())
        .collect())
}

/// Patterns matched case-insensitively.
const ERROR_WORDS: [&str; 12] = [
    "error",
    "err!",
    "failed",
    "exception",
    "eaddrinuse",
    "module not found",
    "cannot find module",
    "unhandled",
    "traceback",
    "panicked at",
    "fatal",
    "econnrefused",
];

fn is_error_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    ERROR_WORDS.iter().any(|w| lower.contains(w)) || line.contains("    at ")
}

/// The lines that look like errors with `context` lines around them. Close
/// matches merge into one block; blocks are separated by a `--` line.
pub fn errors(lines: &[String], context: usize) -> Vec<String> {
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !is_error_line(line) {
            continue;
        }
        let from = i.saturating_sub(context);
        let to = (i + context).min(lines.len() - 1);
        match blocks.last_mut() {
            Some(last) if from <= last.1 + 1 => last.1 = last.1.max(to),
            _ => blocks.push((from, to)),
        }
    }
    let mut out = Vec::new();
    for (k, (from, to)) in blocks.into_iter().enumerate() {
        if k > 0 {
            out.push("--".to_string());
        }
        out.extend_from_slice(&lines[from..=to]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn key(pid: i32) -> ServerKey {
        ServerKey { pid, start_time: 7 }
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn log_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RunLog::create(dir.path(), key(1)).unwrap();
        log.write(b"hello\n").unwrap();
        let path = log_path(dir.path(), key(1));
        assert_eq!(path, dir.path().join("logs/run/1-7.log"));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert!(exists(dir.path(), key(1)));
        assert!(!exists(dir.path(), key(2)));
    }

    #[test]
    fn cap_keeps_newest_half() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RunLog::create(dir.path(), key(1)).unwrap();
        // Chunks that are not line-aligned.
        let all: String = (0..450_000).map(|i| format!("line {i:08}\n")).collect();
        assert!(all.len() > 6 * 1024 * 1024);
        for chunk in all.as_bytes().chunks(65_537) {
            log.write(chunk).unwrap();
        }
        let text = fs::read_to_string(log_path(dir.path(), key(1))).unwrap();
        assert!((text.len() as u64) < MAX_LOG_BYTES);
        assert!(text.ends_with("line 00449999\n"));
        assert!(!text.contains("line 00000000\n"));
        assert!(text.starts_with("line "), "starts mid-line");
        assert!(text.lines().all(|l| l.len() == 13));
        assert!(!log_path(dir.path(), key(1)).with_extension("tmp").exists());
    }

    fn aged(path: &Path, age_ms: u64, now_ms: u64) {
        let t = UNIX_EPOCH + Duration::from_millis(now_ms - age_ms);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn cleanup_after_24h_only_for_dead() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let h = 3_600_000;
        for pid in [1, 2, 3] {
            RunLog::create(dir.path(), key(pid)).unwrap();
        }
        aged(&log_path(dir.path(), key(1)), 25 * h, now);
        aged(&log_path(dir.path(), key(2)), 25 * h, now);
        aged(&log_path(dir.path(), key(3)), h, now);
        fs::write(run_dir(dir.path()).join("notes.txt"), "").unwrap();
        cleanup(dir.path(), &|k| k.pid == 1, now);
        assert!(exists(dir.path(), key(1)), "live server keeps its log");
        assert!(!exists(dir.path(), key(2)), "dead and old");
        assert!(exists(dir.path(), key(3)), "dead but recent");
        assert!(run_dir(dir.path()).join("notes.txt").exists());
    }

    #[test]
    fn keep_window_is_exactly_24h() {
        assert_eq!(LOG_KEEP_MS, 24 * 3_600_000);
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        for pid in [1, 2] {
            RunLog::create(dir.path(), key(pid)).unwrap();
        }
        aged(&log_path(dir.path(), key(1)), LOG_KEEP_MS + 1_000, now);
        aged(&log_path(dir.path(), key(2)), LOG_KEEP_MS - 60_000, now);
        cleanup(dir.path(), &|_| false, now);
        assert!(!exists(dir.path(), key(1)), "24 h + 1 s old");
        assert!(exists(dir.path(), key(2)), "23 h 59 min old");
    }

    #[test]
    fn tail_lossy_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = RunLog::create(dir.path(), key(1)).unwrap();
        log.write(b"one\n\xff\xfe\x00bad\ntwo\n\xc3").unwrap();
        let got = tail(&log_path(dir.path(), key(1)), 3).unwrap();
        assert_eq!(got.len(), 3);
        assert!(got[0].contains('\u{fffd}'));
        assert_eq!(got[1], "two");
        assert_eq!(tail(&log_path(dir.path(), key(1)), 1).unwrap().len(), 1);
        assert_eq!(errors(&got, 1).len(), 0);
    }

    #[test]
    fn errors_with_context() {
        let l = lines(&[
            "ready",
            "compiling",
            "app.ts(3,1): error TS2322: bad type",
            "  detail",
            "    at foo (x.js:1:1)",
            "ok 1",
            "ok 2",
            "ok 3",
            "ok 4",
            "ok 5",
            "ok 6",
            "Error: listen EADDRINUSE :::3000",
            "after",
        ]);
        let got = errors(&l, 2);
        assert_eq!(
            got,
            lines(&[
                "ready",
                "compiling",
                "app.ts(3,1): error TS2322: bad type",
                "  detail",
                "    at foo (x.js:1:1)",
                "ok 1",
                "ok 2",
                "--",
                "ok 5",
                "ok 6",
                "Error: listen EADDRINUSE :::3000",
                "after",
            ])
        );
        assert!(errors(&lines(&["fine"]), 2).is_empty());
        assert!(errors(&[], 2).is_empty());
    }
}
