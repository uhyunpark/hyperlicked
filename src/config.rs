//! Runtime Configuration
//!
//! Configures behavior based on MODE environment variable:
//! - `dev` (default): Auto-faucet, relaxed validation, test accounts
//! - `testnet`: Real validation, but test network
//! - `mainnet`: Full validation, production mode
//!
//! Node role (NODE_ROLE env var):
//! - `validator` (default): Full consensus participation
//! - `rpc`: Observer mode, serve API only

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

/// Node role in the network
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    /// Full consensus participation (propose blocks, vote)
    Validator,
    /// Observer mode (follow chain, serve API, no voting)
    Rpc,
}

impl NodeRole {
    pub fn from_env() -> Self {
        match std::env::var("NODE_ROLE").as_deref() {
            Ok("rpc") | Ok("observer") => NodeRole::Rpc,
            _ => NodeRole::Validator, // Default to validator
        }
    }

    pub fn is_validator(&self) -> bool {
        matches!(self, NodeRole::Validator)
    }

    pub fn is_rpc(&self) -> bool {
        matches!(self, NodeRole::Rpc)
    }
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeRole::Validator => write!(f, "validator"),
            NodeRole::Rpc => write!(f, "rpc"),
        }
    }
}

/// Runtime configuration
#[derive(Debug, Clone)]
pub struct Config {
    /// Runtime mode
    pub mode: Mode,

    /// Node role (validator or rpc)
    pub node_role: NodeRole,

    /// Auto-fund new accounts with this amount (cents)
    /// Only applies in dev mode
    pub faucet_amount: i64,

    /// Block time in milliseconds (0 = max speed)
    pub block_time_ms: u64,

    /// Consensus loop delay in milliseconds (0 = yield only, no sleep)
    /// This prevents CPU spinning in the consensus runner loop
    pub consensus_loop_delay_ms: u64,

    /// Log all blocks (including empty heartbeats)
    pub log_all_blocks: bool,

    /// Skip signature verification (dev mode only!)
    pub skip_signature_verification: bool,

    /// API port
    pub port: u16,

    /// Path to RocksDB data directory (None = in-memory only)
    pub data_dir: Option<String>,

    /// Snapshot app state every N committed blocks (0 = disabled)
    pub snapshot_interval: u64,

    /// Enable oracle system at startup (dev mode only)
    pub oracle_enabled: bool,

    /// Enable artificial market maker (dev mode only)
    pub mm_enabled: bool,

    /// Peer URLs for sync (RPC nodes use these to catch up)
    /// Format: "http://host:port"
    pub peers: Vec<String>,

    /// Sync poll interval in milliseconds
    pub sync_poll_interval_ms: u64,

    /// Skip QC signature verification (dev mode only, dangerous!)
    /// Used for testing RPC sync without full BLS verification
    pub skip_qc_verify: bool,

    /// Consecutive failures before blacklisting a peer
    pub peer_blacklist_threshold: u32,

    /// Duration to blacklist a peer in milliseconds
    pub peer_blacklist_duration_ms: u64,

    /// Maximum liquidations per block (circuit breaker)
    pub max_liquidations_per_block: usize,

    /// Maximum pending transactions per bucket in mempool
    pub mempool_max_per_bucket: usize,

    /// Maximum age of mempool transactions before eviction (ms)
    pub mempool_max_age_ms: u64,

    /// Maximum pending transactions per address in mempool
    pub mempool_max_per_address: usize,
}

impl Config {
    /// Load configuration from environment
    pub fn from_env() -> Self {
        let mode = Mode::from_env();
        let node_role = NodeRole::from_env();

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

        let skip_qc_verify = mode.is_dev()
            && std::env::var("SKIP_QC_VERIFY")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false);

        // Parse peers from PEERS env var (comma-separated URLs)
        let peers: Vec<String> = std::env::var("PEERS")
            .ok()
            .map(|s| s.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default();

        Self {
            mode,
            node_role,
            faucet_amount,
            block_time_ms: std::env::var("BLOCK_TIME_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
            consensus_loop_delay_ms: std::env::var("CONSENSUS_LOOP_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10), // Default 10ms to prevent CPU spin
            log_all_blocks: std::env::var("LOG_BLOCKS")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false),
            skip_signature_verification,
            port: std::env::var("PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8080),
            data_dir: std::env::var("DATA_DIR").ok(),
            snapshot_interval: std::env::var("SNAPSHOT_INTERVAL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000), // Default: snapshot every 1000 blocks
            oracle_enabled: std::env::var("ORACLE_ENABLED")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false),
            mm_enabled: std::env::var("MM_ENABLED")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false),
            peers,
            sync_poll_interval_ms: std::env::var("SYNC_POLL_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1000), // Default: poll every 1 second
            skip_qc_verify,
            peer_blacklist_threshold: std::env::var("PEER_BLACKLIST_THRESHOLD")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5), // Default: 5 consecutive failures
            peer_blacklist_duration_ms: std::env::var("PEER_BLACKLIST_DURATION_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60_000), // Default: 60 seconds
            max_liquidations_per_block: std::env::var("MAX_LIQUIDATIONS_PER_BLOCK")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100), // Default: 100 liquidations per block
            mempool_max_per_bucket: std::env::var("MEMPOOL_MAX_PER_BUCKET")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(10_000), // Default: 10,000 txs per bucket
            mempool_max_age_ms: std::env::var("MEMPOOL_MAX_AGE_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3_600_000), // Default: 1 hour
            mempool_max_per_address: std::env::var("MEMPOOL_MAX_PER_ADDRESS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100), // Default: 100 pending txs per address
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

    /// Validate that security-critical flags are not enabled in production
    ///
    /// SECURITY: Prevents accidentally running with SKIP_SIG_VERIFY or SKIP_QC_VERIFY
    /// in testnet or mainnet. These flags should only be used in dev mode.
    pub fn validate_production_safety(&self) -> Result<(), String> {
        if !self.mode.is_dev() {
            if self.skip_signature_verification {
                return Err(
                    "FATAL: SKIP_SIG_VERIFY=true is not allowed in production (MODE != dev). \
                     This flag disables EIP-712 signature verification and is DANGEROUS."
                        .to_string(),
                );
            }
            if self.skip_qc_verify {
                return Err(
                    "FATAL: SKIP_QC_VERIFY=true is not allowed in production (MODE != dev). \
                     This flag disables BLS QC verification and is DANGEROUS."
                        .to_string(),
                );
            }
        }
        Ok(())
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

    #[test]
    fn test_node_role_default_is_validator() {
        // Without NODE_ROLE env var, should default to validator
        assert!(NodeRole::from_env().is_validator());
    }

    #[test]
    fn test_node_role_display() {
        assert_eq!(NodeRole::Validator.to_string(), "validator");
        assert_eq!(NodeRole::Rpc.to_string(), "rpc");
    }

    #[test]
    fn test_config_node_role() {
        let config = Config::from_env();
        assert!(config.node_role.is_validator());
        assert!(config.peers.is_empty());
    }

    #[test]
    fn test_production_safety_dev_mode_allows_skip_flags() {
        let mut config = Config::from_env();
        config.mode = Mode::Dev;
        config.skip_signature_verification = true;
        config.skip_qc_verify = true;
        // Dev mode should allow these flags
        assert!(config.validate_production_safety().is_ok());
    }

    #[test]
    fn test_production_safety_testnet_rejects_skip_sig_verify() {
        let mut config = Config::from_env();
        config.mode = Mode::Testnet;
        config.skip_signature_verification = true;
        config.skip_qc_verify = false;
        // Testnet should reject SKIP_SIG_VERIFY
        let result = config.validate_production_safety();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("SKIP_SIG_VERIFY"));
    }

    #[test]
    fn test_production_safety_mainnet_rejects_skip_qc_verify() {
        let mut config = Config::from_env();
        config.mode = Mode::Mainnet;
        config.skip_signature_verification = false;
        config.skip_qc_verify = true;
        // Mainnet should reject SKIP_QC_VERIFY
        let result = config.validate_production_safety();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("SKIP_QC_VERIFY"));
    }

    #[test]
    fn test_production_safety_mainnet_allows_safe_config() {
        let mut config = Config::from_env();
        config.mode = Mode::Mainnet;
        config.skip_signature_verification = false;
        config.skip_qc_verify = false;
        // Mainnet with safe flags should pass
        assert!(config.validate_production_safety().is_ok());
    }
}
