use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// What happens when a new server wants a port that another server holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortConflict {
    /// Free the port when its holder is idle, otherwise ask.
    #[default]
    FreeIdle,
    /// Always ask before touching the holder.
    Ask,
    /// Never touch the holder: run the new server on another port.
    Reroute,
}

impl PortConflict {
    /// The values accepted by `Config::set`, in display order.
    pub const NAMES: &'static str = "free_idle, ask, reroute";

    pub fn as_str(self) -> &'static str {
        match self {
            PortConflict::FreeIdle => "free_idle",
            PortConflict::Ask => "ask",
            PortConflict::Reroute => "reroute",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "free_idle" => Some(PortConflict::FreeIdle),
            "ask" => Some(PortConflict::Ask),
            "reroute" => Some(PortConflict::Reroute),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub dead_session_idle_s: u64,
    pub alert_after_s: u64,
    pub kill_grace_s: u64,
    pub scan_interval_s: u64,
    /// Extra Claude config dirs, watched on top of the ones found
    /// automatically (`effective_claude_dirs`). Never holds the found ones,
    /// so a `~/.claude-*` dir created later is still picked up.
    pub claude_config_dirs: Vec<PathBuf>,
    pub exclude_commands: Vec<String>,
    /// Ports never tracked: a server listening on one of them (itself or a
    /// child) is left alone.
    pub exclude_ports: Vec<u16>,
    /// A server above this much memory (MB) leaks.
    pub leak_memory_mb: u64,
    /// A server growing by more than this (MB) in 10 minutes leaks.
    pub leak_growth_mb: u64,
    /// What to do when a new server wants a port another server holds.
    pub port_conflict: PortConflict,
    /// Whether the agent hook checks a port before a server command starts.
    pub port_check_hook: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            dead_session_idle_s: 1200,
            alert_after_s: 1800,
            kill_grace_s: 5,
            scan_interval_s: 5,
            claude_config_dirs: Vec::new(),
            exclude_commands: [
                "mcp",
                "argent",
                "postgres",
                "redis-server",
                "mysqld",
                "mongod",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            exclude_ports: Vec::new(),
            leak_memory_mb: 2048,
            leak_growth_mb: 500,
            port_conflict: PortConflict::default(),
            port_check_hook: true,
        }
    }
}

const CONFIG_FILE: &str = "config.toml";

/// Every Claude config dir whose sessions are read: the ones found
/// automatically (`~/.claude`, `CLAUDE_CONFIG_DIR`, `~/.claude-*` with a
/// `sessions/` subdir), then the configured extras; deduplicated, existing
/// only.
pub fn effective_claude_dirs(cfg: &Config) -> Vec<PathBuf> {
    effective_claude_dirs_with(crate::sessions::default_config_dirs(), cfg)
}

/// `effective_claude_dirs` with the automatic discovery supplied by the
/// caller (tests pass none, so no real session is read).
pub fn effective_claude_dirs_with(discovered: Vec<PathBuf>, cfg: &Config) -> Vec<PathBuf> {
    crate::sessions::merge_config_dirs(
        discovered,
        &cfg.claude_config_dirs,
        dirs::home_dir().as_deref(),
    )
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("BTP_DATA_DIR") {
        return PathBuf::from(d);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Library/Application Support/balance-ton-port")
}

/// Minimum for the two delays counted in minutes (`dead_session_idle_s`,
/// `alert_after_s`): a 0 there would kill or alert at once.
pub const MIN_DELAY_S: u64 = 60;
/// Minimum for `kill_grace_s` (0 means SIGKILL at once) and
/// `scan_interval_s` (0 would busy-loop).
pub const MIN_SECONDS_S: u64 = 1;
/// Minimum for `leak_memory_mb`: below it every server would leak.
pub const MIN_LEAK_MEMORY_MB: u64 = 100;
/// Minimum for `leak_growth_mb`: below it ordinary churn would leak.
pub const MIN_LEAK_GROWTH_MB: u64 = 50;

/// The minimum for a numeric key, `None` for an unknown key.
fn minimum(key: &str) -> Option<u64> {
    match key {
        "dead_session_idle_s" | "alert_after_s" => Some(MIN_DELAY_S),
        "kill_grace_s" | "scan_interval_s" => Some(MIN_SECONDS_S),
        "leak_memory_mb" => Some(MIN_LEAK_MEMORY_MB),
        "leak_growth_mb" => Some(MIN_LEAK_GROWTH_MB),
        _ => None,
    }
}

fn below_minimum(key: &str, value: u64, min: u64) -> String {
    format!("{key} must be at least {min}, got {value}")
}

/// The saved config. A missing file gives the defaults. A file that cannot
/// be read or parsed (one mistyped field included) is an error, never
/// silently the defaults: the caller decides what to run on. Values below
/// their minimum are raised to it, with a note on stderr (once per change
/// of the file, not on every read).
pub fn read_config(dir: &Path) -> Result<Config, String> {
    let path = dir.join(CONFIG_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut c: Config = toml::from_str(&text)
        .map_err(|e| format!("invalid {}: {}", path.display(), e.message()))?;
    let notes = c.clamp();
    if !notes.is_empty() {
        let mut seen = NOTED.lock().unwrap_or_else(|e| e.into_inner());
        if note_due(&mut seen, &path, &text) {
            for note in notes {
                eprintln!("btp: {}: {note}", path.display());
            }
        }
    }
    Ok(c)
}

/// The text of each config file whose clamp notes were last printed: the
/// watcher reads the file every pass, the notes are printed once per change.
static NOTED: std::sync::Mutex<Option<HashMap<PathBuf, String>>> = std::sync::Mutex::new(None);

/// `true` (and remembered) when `text` is not what `path` held the last time
/// its notes were printed.
fn note_due(seen: &mut Option<HashMap<PathBuf, String>>, path: &Path, text: &str) -> bool {
    let seen = seen.get_or_insert_with(HashMap::new);
    if seen.get(path).is_some_and(|t| t == text) {
        return false;
    }
    seen.insert(path.to_path_buf(), text.to_string());
    true
}

/// Writes the config through a temp file and a rename, so a reader never
/// sees a half-written file.
pub fn save_config(dir: &Path, c: &Config) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(c).map_err(io::Error::other)?;
    let tmp = dir.join(format!("{CONFIG_FILE}.tmp.{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join(CONFIG_FILE))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

impl Config {
    /// The scan interval, never below one second (a 0 in the file would
    /// otherwise busy-loop the watcher).
    pub fn scan_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.scan_interval_s.max(MIN_SECONDS_S))
    }

    fn numeric_mut(&mut self) -> [(&'static str, &mut u64); 6] {
        [
            ("dead_session_idle_s", &mut self.dead_session_idle_s),
            ("alert_after_s", &mut self.alert_after_s),
            ("kill_grace_s", &mut self.kill_grace_s),
            ("scan_interval_s", &mut self.scan_interval_s),
            ("leak_memory_mb", &mut self.leak_memory_mb),
            ("leak_growth_mb", &mut self.leak_growth_mb),
        ]
    }

    /// `Err` naming the first value below its minimum.
    pub fn validate(&self) -> Result<(), String> {
        let mut c = self.clone();
        for (key, v) in c.numeric_mut() {
            let min = minimum(key).unwrap_or(0);
            if *v < min {
                return Err(below_minimum(key, *v, min));
            }
        }
        Ok(())
    }

    /// Raises every value below its minimum to it; one note per change.
    pub fn clamp(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        for (key, v) in self.numeric_mut() {
            let min = minimum(key).unwrap_or(0);
            if *v < min {
                notes.push(format!("{}, using {min}", below_minimum(key, *v, min)));
                *v = min;
            }
        }
        notes
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key {
            "port_conflict" => {
                self.port_conflict = PortConflict::parse(value).ok_or_else(|| {
                    format!(
                        "port_conflict expects one of {}, got {value:?}",
                        PortConflict::NAMES
                    )
                })?;
                return Ok(());
            }
            "port_check_hook" => {
                self.port_check_hook = match value {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(format!(
                            "port_check_hook expects one of true, false, got {value:?}"
                        ))
                    }
                };
                return Ok(());
            }
            _ => {}
        }
        let min = minimum(key).ok_or_else(|| format!("unknown key: {key}"))?;
        let v = value
            .parse::<u64>()
            .map_err(|_| format!("{key} expects a non-negative integer, got {value:?}"))?;
        if v < min {
            return Err(below_minimum(key, v, min));
        }
        for (k, field) in self.numeric_mut() {
            if k == key {
                *field = v;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.dead_session_idle_s, 1200);
        assert_eq!(c.alert_after_s, 1800);
        assert_eq!(c.kill_grace_s, 5);
        assert_eq!(c.scan_interval_s, 5);
        assert_eq!(
            c.exclude_commands,
            [
                "mcp",
                "argent",
                "postgres",
                "redis-server",
                "mysqld",
                "mongod"
            ]
        );
    }

    #[test]
    fn default_has_no_extra_dirs_and_saving_writes_none() {
        assert!(Config::default().claude_config_dirs.is_empty());
        let d = tempfile::tempdir().unwrap();
        save_config(d.path(), &Config::default()).unwrap();
        let text = std::fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("claude_config_dirs = []"), "{text}");
        // Loading and saving again still writes no discovered dir.
        save_config(d.path(), &read_config(d.path()).unwrap()).unwrap();
        let again = std::fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap();
        assert_eq!(text, again);
    }

    #[test]
    fn effective_dirs_are_discovered_then_extras() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let c = Config {
            claude_config_dirs: vec![b.path().to_path_buf(), a.path().to_path_buf()],
            ..Config::default()
        };
        assert_eq!(
            effective_claude_dirs_with(vec![a.path().to_path_buf()], &c),
            vec![a.path().to_path_buf(), b.path().to_path_buf()]
        );
        assert_eq!(
            effective_claude_dirs_with(Vec::new(), &Config::default()),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read_config(d.path()).unwrap(), Config::default());
    }

    #[test]
    fn invalid_toml_is_reported_not_replaced_by_defaults() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "not = [valid").unwrap();
        let e = read_config(d.path()).unwrap_err();
        assert!(e.contains("config.toml"), "{e}");
        // One mistyped field is an error too, not a silent reset.
        std::fs::write(d.path().join("config.toml"), "alert_after_s = \"abc\"\n").unwrap();
        assert!(read_config(d.path()).is_err());
    }

    #[test]
    fn set_enforces_minimums() {
        let mut c = Config::default();
        for (key, low, ok) in [
            ("dead_session_idle_s", "59", "60"),
            ("alert_after_s", "0", "60"),
            ("kill_grace_s", "0", "1"),
            ("scan_interval_s", "0", "1"),
        ] {
            let e = c.set(key, low).unwrap_err();
            assert!(e.contains("at least"), "{key}: {e}");
            c.set(key, ok).unwrap();
        }
        assert_eq!(c.dead_session_idle_s, 60);
        assert_eq!(c.kill_grace_s, 1);
    }

    #[test]
    fn leak_defaults_and_minimums() {
        let mut c = Config::default();
        assert_eq!((c.leak_memory_mb, c.leak_growth_mb), (2048, 500));
        assert!(c
            .set("leak_growth_mb", "10")
            .unwrap_err()
            .contains("at least 50"));
        assert!(c
            .set("leak_memory_mb", "99")
            .unwrap_err()
            .contains("at least 100"));
        c.set("leak_memory_mb", "100").unwrap();
        c.set("leak_growth_mb", "50").unwrap();
        assert_eq!((c.leak_memory_mb, c.leak_growth_mb), (100, 50));
        let low = Config {
            leak_growth_mb: 1,
            ..Config::default()
        };
        assert!(low.validate().unwrap_err().contains("leak_growth_mb"));
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("config.toml"),
            "leak_memory_mb = 0\nleak_growth_mb = 0\n",
        )
        .unwrap();
        let c = read_config(d.path()).unwrap();
        assert_eq!((c.leak_memory_mb, c.leak_growth_mb), (100, 50));
    }

    #[test]
    fn validate_reports_the_first_value_below_its_minimum() {
        assert_eq!(Config::default().validate(), Ok(()));
        let c = Config {
            kill_grace_s: 0,
            ..Config::default()
        };
        assert!(c.validate().unwrap_err().contains("kill_grace_s"));
    }

    #[test]
    fn values_below_minimum_in_the_file_are_clamped_on_load() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("config.toml"),
            "dead_session_idle_s = 0\nalert_after_s = 5\nkill_grace_s = 0\nscan_interval_s = 0\n",
        )
        .unwrap();
        let c = read_config(d.path()).unwrap();
        assert_eq!(
            (
                c.dead_session_idle_s,
                c.alert_after_s,
                c.kill_grace_s,
                c.scan_interval_s
            ),
            (60, 60, 1, 1)
        );
    }

    #[test]
    fn a_clamp_note_is_due_once_per_distinct_file() {
        let mut seen = None;
        let a = Path::new("/x/config.toml");
        let b = Path::new("/y/config.toml");
        assert!(note_due(&mut seen, a, "kill_grace_s = 0"));
        // Every pass re-reads the same file: no repeat.
        assert!(!note_due(&mut seen, a, "kill_grace_s = 0"));
        // Another file is noted on its own.
        assert!(note_due(&mut seen, b, "kill_grace_s = 0"));
        // A change, then back: each change is noted once.
        assert!(note_due(&mut seen, a, "scan_interval_s = 0"));
        assert!(!note_due(&mut seen, a, "scan_interval_s = 0"));
        assert!(note_due(&mut seen, a, "kill_grace_s = 0"));
    }

    #[test]
    fn save_replaces_the_file_atomically() {
        let d = tempfile::tempdir().unwrap();
        save_config(d.path(), &Config::default()).unwrap();
        // A hard link keeps the old inode: an in-place truncate and write
        // would change it too, a temp file + rename does not.
        let old = d.path().join("old.toml");
        std::fs::hard_link(d.path().join("config.toml"), &old).unwrap();
        let c = Config {
            alert_after_s: 120,
            ..Config::default()
        };
        save_config(d.path(), &c).unwrap();
        assert_eq!(read_config(d.path()).unwrap(), c);
        assert!(std::fs::read_to_string(&old)
            .unwrap()
            .contains("alert_after_s = 1800"));
        let names: Vec<String> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temp file left: {names:?}");
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "alert_after_s = 60\n").unwrap();
        let c = read_config(d.path()).unwrap();
        assert_eq!(c.alert_after_s, 60);
        assert_eq!(c.dead_session_idle_s, 1200);
        assert_eq!(c.exclude_commands, Config::default().exclude_commands);
    }

    #[test]
    fn set_rejects_unknown_key_and_non_integer() {
        let mut c = Config::default();
        assert!(c.set("nope", "1").is_err());
        assert!(c.set("alert_after_s", "abc").is_err());
        assert!(c.set("alert_after_s", "-3").is_err());
        assert_eq!(c, Config::default());
        c.set("kill_grace_s", "9").unwrap();
        assert_eq!(c.kill_grace_s, 9);
    }

    #[test]
    fn scan_interval_is_at_least_one_second() {
        let mut c = Config::default();
        assert_eq!(c.scan_interval(), std::time::Duration::from_secs(5));
        c.scan_interval_s = 0;
        assert_eq!(c.scan_interval(), std::time::Duration::from_secs(1));
    }

    #[test]
    fn save_then_load_roundtrips() {
        let d = tempfile::tempdir().unwrap();
        let c = Config {
            scan_interval_s: 11,
            claude_config_dirs: vec![PathBuf::from("/tmp/x")],
            ..Config::default()
        };
        save_config(d.path(), &c).unwrap();
        assert_eq!(read_config(d.path()).unwrap(), c);
    }

    #[test]
    fn port_conflict_defaults() {
        let c = Config::default();
        assert_eq!(c.port_conflict, PortConflict::FreeIdle);
        assert!(c.port_check_hook);
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "alert_after_s = 60\n").unwrap();
        let c = read_config(d.path()).unwrap();
        assert_eq!(c.port_conflict, PortConflict::FreeIdle);
        assert!(c.port_check_hook);
    }

    #[test]
    fn set_port_conflict() {
        let mut c = Config::default();
        c.set("port_conflict", "ask").unwrap();
        assert_eq!(c.port_conflict, PortConflict::Ask);
        c.set("port_conflict", "reroute").unwrap();
        assert_eq!(c.port_conflict, PortConflict::Reroute);
        c.set("port_conflict", "free_idle").unwrap();
        assert_eq!(c.port_conflict, PortConflict::FreeIdle);
        let e = c.set("port_conflict", "kill").unwrap_err();
        assert!(e.contains("free_idle, ask, reroute"), "{e}");
        c.set("port_check_hook", "false").unwrap();
        assert!(!c.port_check_hook);
        c.set("port_check_hook", "true").unwrap();
        assert!(c.port_check_hook);
        let e = c.set("port_check_hook", "maybe").unwrap_err();
        assert!(e.contains("true, false"), "{e}");
    }

    #[test]
    fn port_settings_roundtrip_through_the_file() {
        let d = tempfile::tempdir().unwrap();
        let c = Config {
            port_conflict: PortConflict::Reroute,
            port_check_hook: false,
            ..Config::default()
        };
        save_config(d.path(), &c).unwrap();
        let text = std::fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap();
        assert!(text.contains("port_conflict = \"reroute\""), "{text}");
        assert_eq!(read_config(d.path()).unwrap(), c);
    }
}
