//! Visor configuration
//!
//! Configuration is loaded from `visor_config.json` in the daemon home directory.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Default configuration values
pub mod defaults {
    pub const DAEMON_NAME: &str = "hl-node";
    pub const RESTART_DELAY_MS: u64 = 1000;
    pub const MAX_RESTART_ATTEMPTS: u32 = 5;
    pub const HEALTH_CHECK_INTERVAL_MS: u64 = 5000;
    pub const HEALTH_CHECK_TIMEOUT_MS: u64 = 3000;
    pub const HEALTH_CHECK_ENDPOINT: &str = "http://127.0.0.1:8080/health";
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("failed to parse config file: {0}")]
    ParseError(#[from] serde_json::Error),
    #[error("daemon home directory not found")]
    HomeDirNotFound,
}

/// Visor configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisorConfig {
    /// Name of the daemon binary (default: hl-node)
    #[serde(default = "default_daemon_name")]
    pub daemon_name: String,

    /// Home directory for the daemon
    #[serde(default = "default_daemon_home")]
    pub daemon_home: PathBuf,

    /// URL for downloading binaries
    #[serde(default)]
    pub binaries_url: Option<String>,

    /// URL for fetching GPG public key
    #[serde(default)]
    pub gpg_key_url: Option<String>,

    /// Path to GPG public key file (local)
    #[serde(default)]
    pub gpg_key_path: Option<PathBuf>,

    /// Automatically download new binaries
    #[serde(default)]
    pub auto_download: bool,

    /// Restart daemon after upgrade
    #[serde(default = "default_true")]
    pub restart_after_upgrade: bool,

    /// Delay between restarts in milliseconds
    #[serde(default = "default_restart_delay")]
    pub restart_delay_ms: u64,

    /// Maximum restart attempts before giving up
    #[serde(default = "default_max_restart_attempts")]
    pub max_restart_attempts: u32,

    /// Health check interval in milliseconds
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_ms: u64,

    /// Health check timeout in milliseconds
    #[serde(default = "default_health_check_timeout")]
    pub health_check_timeout_ms: u64,

    /// Health check endpoint URL
    #[serde(default = "default_health_check_endpoint")]
    pub health_check_endpoint: String,

    /// Skip GPG verification (unsafe, for development only)
    #[serde(default)]
    pub skip_verify: bool,

    /// Extra arguments to pass to the daemon
    #[serde(default)]
    pub daemon_args: Vec<String>,

    /// Environment variables to set for the daemon
    #[serde(default)]
    pub daemon_env: std::collections::HashMap<String, String>,
}

fn default_daemon_name() -> String {
    defaults::DAEMON_NAME.to_string()
}

fn default_daemon_home() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join("hyperlicked"))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn default_true() -> bool {
    true
}

fn default_restart_delay() -> u64 {
    defaults::RESTART_DELAY_MS
}

fn default_max_restart_attempts() -> u32 {
    defaults::MAX_RESTART_ATTEMPTS
}

fn default_health_check_interval() -> u64 {
    defaults::HEALTH_CHECK_INTERVAL_MS
}

fn default_health_check_timeout() -> u64 {
    defaults::HEALTH_CHECK_TIMEOUT_MS
}

fn default_health_check_endpoint() -> String {
    defaults::HEALTH_CHECK_ENDPOINT.to_string()
}

impl Default for VisorConfig {
    fn default() -> Self {
        Self {
            daemon_name: default_daemon_name(),
            daemon_home: default_daemon_home(),
            binaries_url: None,
            gpg_key_url: None,
            gpg_key_path: None,
            auto_download: false,
            restart_after_upgrade: true,
            restart_delay_ms: defaults::RESTART_DELAY_MS,
            max_restart_attempts: defaults::MAX_RESTART_ATTEMPTS,
            health_check_interval_ms: defaults::HEALTH_CHECK_INTERVAL_MS,
            health_check_timeout_ms: defaults::HEALTH_CHECK_TIMEOUT_MS,
            health_check_endpoint: defaults::HEALTH_CHECK_ENDPOINT.to_string(),
            skip_verify: false,
            daemon_args: Vec::new(),
            daemon_env: std::collections::HashMap::new(),
        }
    }
}

impl VisorConfig {
    /// Load configuration from a file
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = serde_json::from_str(&contents)?;
        Ok(config)
    }

    /// Load configuration from the default location
    pub fn load_default() -> Result<Self, ConfigError> {
        let home = dirs::home_dir().ok_or(ConfigError::HomeDirNotFound)?;
        let config_path = home.join("hyperlicked/visor/visor_config.json");

        if config_path.exists() {
            Self::load(&config_path)
        } else {
            Ok(Self::default())
        }
    }

    /// Save configuration to a file
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Path to the binaries directory
    pub fn binaries_dir(&self) -> PathBuf {
        self.daemon_home.join("binaries")
    }

    /// Path to the current binary symlink
    pub fn current_binary_link(&self) -> PathBuf {
        self.binaries_dir().join("current")
    }

    /// Path to the current binary executable
    pub fn current_binary(&self) -> PathBuf {
        self.current_binary_link().join(&self.daemon_name)
    }

    /// Path to the visor logs directory
    pub fn logs_dir(&self) -> PathBuf {
        self.daemon_home.join("data/visor_logs")
    }

    /// Path to the upgrade info file
    pub fn upgrade_info_path(&self) -> PathBuf {
        self.daemon_home.join("upgrade-info.json")
    }

    /// Path to the data directory
    pub fn data_dir(&self) -> PathBuf {
        self.daemon_home.join("data")
    }

    /// Ensure all required directories exist
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.binaries_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        std::fs::create_dir_all(self.logs_dir().join("crash_reports"))?;
        std::fs::create_dir_all(self.daemon_home.join("visor"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = VisorConfig::default();
        assert_eq!(config.daemon_name, "hl-node");
        assert_eq!(config.max_restart_attempts, 5);
        assert!(!config.auto_download);
    }

    #[test]
    fn test_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        let config = VisorConfig {
            daemon_name: "test-daemon".to_string(),
            max_restart_attempts: 10,
            ..Default::default()
        };

        config.save(&config_path).unwrap();
        let loaded = VisorConfig::load(&config_path).unwrap();

        assert_eq!(loaded.daemon_name, "test-daemon");
        assert_eq!(loaded.max_restart_attempts, 10);
    }
}
