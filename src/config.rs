//! Runtime Configuration
//!
//! Configures behavior based on MODE environment variable:
//! - `dev` (default): Auto-faucet, relaxed validation, test accounts
//! - `testnet`: Real validation, but test network
//! - `mainnet`: Full validation, production mode

use std::sync::OnceLock;

/// Global config singleton
static CONFIG: OnceLock<Config> = OnceLock::new();

/// Runtime mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Development mode: auto-faucet, relaxed validation
    Dev,
    /// Testnet mode: real validation, test network
    Testnet,
    /// Mainnet mode: full validation, production
    Mainnet,
}

impl Mode {
    pub fn from_env() -> Self {
        match std::env::var("MODE").as_deref() {
            Ok("mainnet") | Ok("production") => Mode::Mainnet,
            Ok("testnet") | Ok("staging") => Mode::Testnet,
            _ => Mode::Dev, // Default to dev
        }
    }

    pub fn is_dev(&self) -> bool {
        matches!(self, Mode::Dev)
    }

    pub fn is_production(&self) -> bool {
        matches!(self, Mode::Mainnet)
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Dev => write!(f, "dev"),
            Mode::Testnet => write!(f, "testnet"),
            Mode::Mainnet => write!(f, "mainnet"),
        }
    }
}

/// Runtime configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Runtime mode
    pub mode: Mode,

    /// Auto-fund new accounts with this amount (cents)
    /// Only applies in dev mode
    pub faucet_amount: i64,

    /// Block time in milliseconds (0 = max speed)
    pub block_time_ms: u64,

    /// Log all blocks (including empty heartbeats)
    pub log_all_blocks: bool,

    /// Skip signature verification (dev mode only!)
    pub skip_signature_verification: bool,

    /// API port
    pub port: u16,
}

impl Config {
    /// Load configuration from environment
    pub fn from_env() -> Self {
        let mode = Mode::from_env();

        let faucet_amount = if mode.is_dev() {
            std::env::var("DEV_FAUCET_AMOUNT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10_000_000) // $100,000 default
        } else {
            0 // No faucet in production
        };

        let skip_signature_verification = mode.is_dev()
            && std::env::var("SKIP_SIG_VERIFY")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false);

        Self {
            mode,
            faucet_amount,
            block_time_ms: std::env::var("BLOCK_TIME_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
            log_all_blocks: std::env::var("LOG_BLOCKS")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false),
            skip_signature_verification,
            port: std::env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
        }
    }

    /// Get global config instance
    pub fn global() -> &'static Config {
        CONFIG.get_or_init(|| Config::from_env())
    }

    /// Initialize global config (call once at startup)
    pub fn init() {
        let _ = Self::global();
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_default_is_dev() {
        // Without MODE env var, should default to dev
        assert!(Mode::from_env().is_dev());
    }

    #[test]
    fn test_config_defaults() {
        let config = Config::from_env();
        assert!(config.mode.is_dev());
        assert_eq!(config.faucet_amount, 10_000_000); // $100k
    }
}
