//! Visor - Process supervisor for hyperlicked validator nodes
//!
//! Similar to:
//! - hl-visor (Hyperliquid)
//! - Cosmovisor (Cosmos SDK)
//!
//! ## Features
//!
//! - Process supervision with automatic restart
//! - GPG signature verification for binaries
//! - Coordinated upgrades at specific block heights
//! - Health monitoring via HTTP endpoint
//!
//! ## Directory Structure
//!
//! ```text
//! ~/hyperlicked/
//! ├── visor/
//! │   ├── hl-visor              # Visor binary
//! │   ├── visor_config.json     # Configuration
//! │   └── pub_key.asc           # GPG public key
//! ├── binaries/
//! │   ├── current -> v1.0.0     # Symlink to current version
//! │   └── v1.0.0/
//! │       └── hl-server
//! ├── data/
//! │   └── visor_logs/
//! │       ├── child_stdout.log
//! │       ├── child_stderr.log
//! │       └── crash_reports/
//! └── upgrade-info.json         # Upgrade trigger file
//! ```

pub mod config;
pub mod health;
pub mod process;
pub mod upgrade;
pub mod verify;

pub use config::VisorConfig;
pub use health::HealthChecker;
pub use process::ProcessManager;
pub use upgrade::{UpgradeInfo, UpgradeManager};
pub use verify::verify_binary;
