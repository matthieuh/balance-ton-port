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
            claude_config_dirs: crate::sessions::default_config_dirs(),
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
