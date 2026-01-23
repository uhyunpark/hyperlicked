//! Upgrade manager for coordinated binary upgrades
//!
//! Handles:
//! - Watching for upgrade signals (upgrade-info.json)
//! - Downloading new binaries
//! - Verifying signatures and checksums
//! - Coordinating the upgrade at safe points

use crate::visor::config::VisorConfig;
use crate::visor::verify::{verify_binary, verify_checksum};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{error, info, warn};

#[derive(Debug, Error)]
pub enum UpgradeError {
    #[error("failed to read upgrade info: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("failed to parse upgrade info: {0}")]
    ParseError(#[from] serde_json::Error),
    #[error("binary not found for platform {0}")]
    PlatformNotSupported(String),
    #[error("download failed: {0}")]
    DownloadFailed(String),
    #[error("checksum verification failed")]
    ChecksumMismatch,
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),
    #[error("failed to install binary: {0}")]
    InstallFailed(String),
    #[error("upgrade info not found")]
    NotFound,
}

/// Binary information for a specific platform
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryInfo {
    /// Download URL
    pub url: String,
    /// Checksum (sha256:...)
    pub checksum: String,
}

/// Upgrade information file format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeInfo {
    /// Version name (e.g., "v1.1.0")
    pub name: String,
    /// Block height at which to trigger upgrade
    pub height: u64,
    /// Human-readable description
    #[serde(default)]
    pub info: String,
    /// Binaries for each platform
    pub binaries: HashMap<String, BinaryInfo>,
}

impl UpgradeInfo {
    /// Load upgrade info from a file
    pub fn load(path: &Path) -> Result<Self, UpgradeError> {
        let contents = std::fs::read_to_string(path)?;
        let info: Self = serde_json::from_str(&contents)?;
        Ok(info)
    }

    /// Get binary info for current platform
    pub fn binary_for_current_platform(&self) -> Option<&BinaryInfo> {
        let platform = current_platform();
        self.binaries.get(&platform)
    }
}

/// Get the current platform string (e.g., "darwin/arm64", "linux/amd64")
fn current_platform() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    // Normalize arch names
    let arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => arch,
    };

    format!("{}/{}", os, arch)
}

/// Upgrade manager
pub struct UpgradeManager {
    config: VisorConfig,
    pending_upgrade: Option<UpgradeInfo>,
    current_height: u64,
}

impl UpgradeManager {
    /// Create a new upgrade manager
    pub fn new(config: VisorConfig) -> Self {
        Self {
            config,
            pending_upgrade: None,
            current_height: 0,
        }
    }

    /// Update the current block height
    pub fn set_height(&mut self, height: u64) {
        self.current_height = height;
    }

    /// Check for pending upgrade info file
    pub fn check_upgrade_info(&mut self) -> Option<&UpgradeInfo> {
        let path = self.config.upgrade_info_path();

        if !path.exists() {
            return None;
        }

        if self.pending_upgrade.is_none() {
            match UpgradeInfo::load(&path) {
                Ok(info) => {
                    info!(
                        "Found upgrade {} scheduled at height {}",
                        info.name, info.height
                    );
                    self.pending_upgrade = Some(info);
                }
                Err(e) => {
                    warn!("Failed to load upgrade info: {}", e);
                    return None;
                }
            }
        }

        self.pending_upgrade.as_ref()
    }

    /// Check if upgrade should be triggered at current height
    pub fn should_upgrade(&self) -> bool {
        if let Some(ref upgrade) = self.pending_upgrade {
            self.current_height >= upgrade.height
        } else {
            false
        }
    }

    /// Get the pending upgrade info
    pub fn pending_upgrade(&self) -> Option<&UpgradeInfo> {
        self.pending_upgrade.as_ref()
    }

    /// Download the upgrade binary
    pub async fn download_upgrade(&self, upgrade: &UpgradeInfo) -> Result<PathBuf, UpgradeError> {
        let binary_info = upgrade
            .binary_for_current_platform()
            .ok_or_else(|| UpgradeError::PlatformNotSupported(current_platform()))?;

        let version_dir = self.config.binaries_dir().join(&upgrade.name);
        fs::create_dir_all(&version_dir).await?;

        let binary_path = version_dir.join(&self.config.daemon_name);

        info!("Downloading {} to {}", binary_info.url, binary_path.display());

        // Download the binary
        let response = reqwest::get(&binary_info.url)
            .await
            .map_err(|e| UpgradeError::DownloadFailed(e.to_string()))?;

        if !response.status().is_success() {
            return Err(UpgradeError::DownloadFailed(format!(
                "HTTP {}",
                response.status()
            )));
        }

        let bytes = response
            .bytes()
            .await
            .map_err(|e| UpgradeError::DownloadFailed(e.to_string()))?;

        // Write to file
        let mut file = fs::File::create(&binary_path).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&binary_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&binary_path, perms)?;
        }

        // Verify checksum
        info!("Verifying checksum...");
        if !verify_checksum(&binary_path, &binary_info.checksum)? {
            error!("Checksum verification failed!");
            fs::remove_file(&binary_path).await?;
            return Err(UpgradeError::ChecksumMismatch);
        }

        info!("Checksum verified successfully");
        Ok(binary_path)
    }

    /// Verify the binary signature
    pub fn verify_signature(&self, binary_path: &Path) -> Result<(), UpgradeError> {
        if self.config.skip_verify {
            warn!("Skipping signature verification (skip_verify=true)");
            return Ok(());
        }

        let sig_path = binary_path.with_extension("sig");
        if !sig_path.exists() {
            // Try .asc extension
            let sig_path = binary_path.with_extension("asc");
            if !sig_path.exists() {
                warn!("No signature file found, skipping verification");
                return Ok(());
            }
        }

        let pubkey_path = self
            .config
            .gpg_key_path
            .clone()
            .unwrap_or_else(|| self.config.daemon_home.join("visor/pub_key.asc"));

        if !pubkey_path.exists() {
            warn!("No public key found at {}, skipping verification", pubkey_path.display());
            return Ok(());
        }

        verify_binary(binary_path, &sig_path, &pubkey_path)
            .map_err(|e| UpgradeError::SignatureInvalid(e.to_string()))?;

        info!("Signature verified successfully");
        Ok(())
    }

    /// Install the upgrade by updating the current symlink
    pub async fn install_upgrade(&mut self, upgrade: &UpgradeInfo) -> Result<(), UpgradeError> {
        let version_dir = self.config.binaries_dir().join(&upgrade.name);
        let binary_path = version_dir.join(&self.config.daemon_name);

        if !binary_path.exists() {
            return Err(UpgradeError::InstallFailed(format!(
                "Binary not found at {}",
                binary_path.display()
            )));
        }

        let current_link = self.config.current_binary_link();

        // Remove old symlink if it exists
        if current_link.exists() || current_link.is_symlink() {
            fs::remove_file(&current_link).await.ok();
        }

        // Create new symlink
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&version_dir, &current_link)
                .map_err(|e| UpgradeError::InstallFailed(e.to_string()))?;
        }

        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&version_dir, &current_link)
                .map_err(|e| UpgradeError::InstallFailed(e.to_string()))?;
        }

        info!(
            "Upgrade installed: {} -> {}",
            current_link.display(),
            version_dir.display()
        );

        // Clear pending upgrade
        self.pending_upgrade = None;

        // Remove upgrade-info.json
        let upgrade_info_path = self.config.upgrade_info_path();
        if upgrade_info_path.exists() {
            fs::remove_file(&upgrade_info_path).await.ok();
        }

        Ok(())
    }

    /// Manually add a binary for a specific version
    pub async fn add_binary(
        &self,
        version: &str,
        binary_path: &Path,
    ) -> Result<PathBuf, UpgradeError> {
        let version_dir = self.config.binaries_dir().join(version);
        fs::create_dir_all(&version_dir).await?;

        let target_path = version_dir.join(&self.config.daemon_name);

        // Copy the binary
        fs::copy(binary_path, &target_path).await?;

        // Make executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&target_path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&target_path, perms)?;
        }

        info!(
            "Added binary for version {} at {}",
            version,
            target_path.display()
        );
        Ok(target_path)
    }

    /// Get upgrade status
    pub fn status(&self) -> UpgradeStatus {
        UpgradeStatus {
            current_height: self.current_height,
            pending_upgrade: self.pending_upgrade.clone(),
            current_binary: self.config.current_binary(),
        }
    }
}

/// Upgrade status information
#[derive(Debug)]
pub struct UpgradeStatus {
    pub current_height: u64,
    pub pending_upgrade: Option<UpgradeInfo>,
    pub current_binary: PathBuf,
}

impl std::fmt::Display for UpgradeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Current Height: {}", self.current_height)?;
        writeln!(f, "Current Binary: {}", self.current_binary.display())?;

        if let Some(ref upgrade) = self.pending_upgrade {
            writeln!(f, "\nPending Upgrade:")?;
            writeln!(f, "  Version: {}", upgrade.name)?;
            writeln!(f, "  Height: {}", upgrade.height)?;
            writeln!(f, "  Info: {}", upgrade.info)?;

            let blocks_remaining = upgrade.height.saturating_sub(self.current_height);
            writeln!(f, "  Blocks Remaining: {}", blocks_remaining)?;
        } else {
            writeln!(f, "\nNo pending upgrade")?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_current_platform() {
        let platform = current_platform();
        assert!(platform.contains('/'));
    }

    #[test]
    fn test_upgrade_info_parse() {
        let json = r#"{
            "name": "v1.1.0",
            "height": 1000000,
            "info": "Security fixes",
            "binaries": {
                "linux/amd64": {
                    "url": "https://example.com/binary",
                    "checksum": "sha256:abc123"
                }
            }
        }"#;

        let info: UpgradeInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.name, "v1.1.0");
        assert_eq!(info.height, 1000000);
    }

    #[test]
    fn test_should_upgrade() {
        let config = VisorConfig::default();
        let mut manager = UpgradeManager::new(config);

        manager.pending_upgrade = Some(UpgradeInfo {
            name: "v1.1.0".to_string(),
            height: 100,
            info: String::new(),
            binaries: HashMap::new(),
        });

        manager.set_height(50);
        assert!(!manager.should_upgrade());

        manager.set_height(100);
        assert!(manager.should_upgrade());

        manager.set_height(150);
        assert!(manager.should_upgrade());
    }
}
