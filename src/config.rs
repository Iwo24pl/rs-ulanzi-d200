use anyhow::{Context, Result};
use log::warn;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const WINDOW_MODE_STATUS: u8 = 0;
pub const WINDOW_MODE_CLOCK: u8 = 1;
pub const WINDOW_MODE_CLEAR: u8 = 2;

/// tokio's `interval` panics on a zero period, and the D200 firmware cannot
/// usefully refresh faster than this.
const MIN_STATS_INTERVAL_MS: u64 = 100;
const MAX_STATS_INTERVAL_MS: u64 = 60_000;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_brightness")]
    pub brightness: u8,
    #[serde(default)]
    pub label_style: HashMap<String, serde_json::Value>,
    #[serde(default = "default_display_mode")]
    pub display_mode: u8,
    #[serde(default = "default_stats_interval")]
    pub stats_interval_ms: u64,
    #[serde(skip)]
    pub filepath: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            brightness: default_brightness(),
            label_style: HashMap::new(),
            display_mode: default_display_mode(),
            stats_interval_ms: default_stats_interval(),
            filepath: None,
        }
    }
}

fn default_brightness() -> u8 {
    100
}
fn default_display_mode() -> u8 {
    WINDOW_MODE_STATUS
}
fn default_stats_interval() -> u64 {
    1000
}

impl Config {
    /// Resolve a user-supplied config path.
    /// OpenDeck on Windows launches the plugin with an unpredictable CWD,
    /// so a relative path like `config.yaml` is first tried next to the
    /// executable (the plugin folder), then against the CWD.
    pub fn resolve_path<P: AsRef<Path>>(path: P) -> PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            return path.to_path_buf();
        }
        // 1) Next to the executable (OpenDeck plugin folder).
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            let candidate = dir.join(path);
            if candidate.exists() {
                return candidate;
            }
            // Remember the exe-dir candidate even if it doesn't exist yet,
            // so first-run saves land next to the plugin, not in a random CWD.
            // Only do this when CWD doesn't have the file.
            if !path.exists() {
                return candidate;
            }
        }
        // 2) Fall back to CWD-relative.
        path.to_path_buf()
    }

    /// Clamp/repair user-editable values so a bad config cannot panic the
    /// daemon or leave the device in an undefined display mode.
    pub fn validate(&mut self) {
        self.brightness = self.brightness.min(100);
        self.stats_interval_ms = self
            .stats_interval_ms
            .clamp(MIN_STATS_INTERVAL_MS, MAX_STATS_INTERVAL_MS);
        if self.display_mode > WINDOW_MODE_CLEAR {
            warn!(
                "Invalid display_mode {} in config, falling back to status",
                self.display_mode
            );
            self.display_mode = WINDOW_MODE_STATUS;
        }
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = Self::resolve_path(path);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;

        let mut config: Config = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse YAML config: {:?}", path))?;

        config.validate();
        config.filepath = Some(path);

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(ref path) = self.filepath {
            let content =
                serde_yaml::to_string(self).with_context(|| "Failed to serialize config")?;
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create config dir: {:?}", parent))?;
            }
            fs::write(path, content)
                .with_context(|| format!("Failed to write config file: {:?}", path))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let mut config = Config::default();
        config.validate();
        assert_eq!(config.brightness, 100);
        assert_eq!(config.display_mode, WINDOW_MODE_STATUS);
        assert_eq!(config.stats_interval_ms, 1000);
    }

    #[test]
    fn validate_repairs_invalid_values() {
        let mut config = Config {
            brightness: 250,
            display_mode: 9,
            stats_interval_ms: 0,
            ..Config::default()
        };
        config.validate();
        assert_eq!(config.brightness, 100);
        assert_eq!(config.display_mode, WINDOW_MODE_STATUS);
        assert_eq!(config.stats_interval_ms, MIN_STATS_INTERVAL_MS);

        config.stats_interval_ms = u64::MAX;
        config.validate();
        assert_eq!(config.stats_interval_ms, MAX_STATS_INTERVAL_MS);
    }

    #[test]
    fn yaml_missing_fields_use_defaults() {
        let mut config: Config = serde_yaml::from_str("brightness: 30\n").unwrap();
        config.validate();
        assert_eq!(config.brightness, 30);
        assert_eq!(config.display_mode, WINDOW_MODE_STATUS);
        assert_eq!(config.stats_interval_ms, 1000);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir =
            std::env::temp_dir().join(format!("rs-ulanzi-d200-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yaml");

        let config = Config {
            filepath: Some(path.clone()),
            display_mode: WINDOW_MODE_CLOCK,
            ..Config::default()
        };
        config.save().unwrap();

        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.display_mode, WINDOW_MODE_CLOCK);
        assert_eq!(loaded.filepath.as_deref(), Some(path.as_path()));

        fs::remove_dir_all(&dir).unwrap();
    }
}
