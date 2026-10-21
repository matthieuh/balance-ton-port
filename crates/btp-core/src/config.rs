use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

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

pub fn load_config(dir: &Path) -> Config {
    let path = dir.join(CONFIG_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "btp: invalid config {}: {e}; using defaults",
                path.display()
            );
            Config::default()
        }
    }
}

pub fn save_config(dir: &Path, c: &Config) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = toml::to_string_pretty(c).map_err(io::Error::other)?;
    std::fs::write(dir.join(CONFIG_FILE), text)
}

impl Config {
    /// The scan interval, never below one second (a 0 in the file would
    /// otherwise busy-loop the watcher).
    pub fn scan_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.scan_interval_s.max(1))
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let field = match key {
            "dead_session_idle_s" => &mut self.dead_session_idle_s,
            "alert_after_s" => &mut self.alert_after_s,
            "kill_grace_s" => &mut self.kill_grace_s,
            "scan_interval_s" => &mut self.scan_interval_s,
            _ => return Err(format!("unknown key: {key}")),
        };
        *field = value
            .parse::<u64>()
            .map_err(|_| format!("{key} expects a non-negative integer, got {value:?}"))?;
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
        save_config(d.path(), &load_config(d.path())).unwrap();
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
        assert_eq!(load_config(d.path()), Config::default());
    }

    #[test]
    fn invalid_toml_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "not = [valid").unwrap();
        assert_eq!(load_config(d.path()), Config::default());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("config.toml"), "alert_after_s = 60\n").unwrap();
        let c = load_config(d.path());
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
        assert_eq!(load_config(d.path()), c);
    }
}
