use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub workspaces: Vec<WorkspaceConfig>,

    #[serde(default)]
    pub exclude_channels: Vec<String>,

    #[serde(default)]
    pub exclude_users: Vec<String>,

    #[serde(default = "default_cache_refresh")]
    pub cache_refresh_interval_secs: u64,

    #[serde(default = "default_log_level")]
    pub log_level: String,

    #[serde(default)]
    pub health_port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct WorkspaceConfig {
    pub name: String,
    pub bot_token: String,
    pub app_token: String,
}

fn default_cache_refresh() -> u64 {
    300
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Config = serde_yaml::from_str(&contents)
            .with_context(|| "Failed to parse config YAML")?;

        anyhow::ensure!(
            config.workspaces.len() == 2,
            "Exactly 2 workspaces must be configured (got {})",
            config.workspaces.len()
        );

        Ok(config)
    }

    #[allow(dead_code)]
    /// Get the "other" workspace index (0 -> 1, 1 -> 0)
    pub fn peer_index(&self, idx: usize) -> usize {
        if idx == 0 { 1 } else { 0 }
    }
}
