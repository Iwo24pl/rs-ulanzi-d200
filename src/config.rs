use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;


pub const WINDOW_MODE_STATUS: u8 = 0;
pub const WINDOW_MODE_CLOCK: u8 = 1;
pub const WINDOW_MODE_CLEAR: u8 = 2;

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
    pub filepath: Option<std::path::PathBuf>,
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
    return WINDOW_MODE_STATUS as u8;
}
fn default_stats_interval() -> u64 {
    1000
}



impl Config {
    /// Resolve a user-supplied config path.
    /// OpenDeck on Windows launches the plugin with an unpredictable CWD,
    /// so a relative path like `config.yaml` is first tried next to the
    /// executable (the plugin folder), then against the CWD.
    pub fn resolve_path<P: AsRef<Path>>(path: P) -> std::path::PathBuf {
        let path = path.as_ref();
        if path.is_absolute() {
            return path.to_path_buf();
        }
        // 1) Next to the executable (OpenDeck plugin folder).
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join(path);
                if candidate.exists() {
                    return candidate;
                }
                // Remember exe-dir candidate even if it doesn't exist yet,
                // so first-run saves land next to the plugin, not in a
                // random CWD. Only do this when CWD doesn't have the file.
                if !Path::new(path).exists() {
                    return candidate;
                }
            }
        }
        // 2) Fall back to CWD-relative.
        path.to_path_buf()
    }

    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = Self::resolve_path(path);
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {:?}", path))?;

        let mut config: Config = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse YAML config: {:?}", path))?;

        config.filepath = Some(path);

        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        if let Some(ref path) = self.filepath {
            let content = serde_yaml::to_string(self)
                .with_context(|| "Failed to serialize config")?;
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create config dir: {:?}", parent))?;
                }
            }
            fs::write(path, content)
                .with_context(|| format!("Failed to write config file: {:?}", path))?;
        }
        Ok(())
    }
}
