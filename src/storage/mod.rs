//! Persistent Storage
//!
//! RocksDB-based persistence for blocks, consensus state, and app snapshots.
//!
//! ## Architecture
//!
//! Uses hybrid approach:
//! - Always persist: Blocks, consensus safety state (QCs, voted views)
//! - Periodic snapshots: App state every N blocks
//! - On recovery: Load snapshot + replay blocks since snapshot

pub mod recovery;
pub mod rocks;
pub mod snapshot;
pub mod verify;

pub use recovery::{recover_from_storage, RecoveryResult};
pub use rocks::RocksDbStore;
pub use snapshot::AppSnapshot;
pub use verify::{
    compute_snapshot_hash, verify_block_chain, verify_block_chain_internal, verify_snapshot,
    verify_snapshot_height, ChainVerifyResult,
};

use serde::{Deserialize, Serialize};

use crate::app::candles::Candle;
use crate::consensus::BlockStore;
use crate::types::{Block, Certificate, Hash, View};

/// Consensus state that must survive crashes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusState {
    /// Highest QC seen (determines valid chain extension)
    pub high_qc: Option<Certificate>,
    /// Locked QC (for HotStuff-2 liveness)
    pub locked_qc: Option<Certificate>,
    /// Views we've voted in (safety - prevent double voting)
    pub voted_views: Vec<View>,
    /// Current view
    pub current_view: View,
    /// Last committed block height
    pub committed_height: u64,
    /// Last committed block hash
    pub committed_hash: Hash,
    /// Consecutive timeout count (for exponential backoff persistence)
    #[serde(default)]
    pub consecutive_timeouts: u32,
    /// View for which we've sent a ViewChange (prevent double-send after crash)
    #[serde(default)]
    pub vc_sent_for_view: Option<View>,
}

impl ConsensusState {
    /// Genesis consensus state (no prior history)
    pub fn genesis() -> Self {
        Self {
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: [0u8; 32],
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        }
    }
}

/// Extended BlockStore with persistence features
pub trait PersistentStore: BlockStore {
    /// Persist consensus safety state atomically
    fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()>;

    /// Load consensus safety state
    fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>>;

    /// Save app state snapshot at height
    fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()>;

    /// Load latest snapshot at or before given height
    fn load_latest_snapshot(&self, before_height: u64) -> anyhow::Result<Option<(u64, AppSnapshot)>>;

    /// Get all committed blocks from height (inclusive) for replay
    fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>>;

    /// Atomic commit: block + consensus state together
    fn commit_block(&self, block: &Block, state: &ConsensusState) -> anyhow::Result<()>;

    /// Save a batch of candles (key-value pairs already serialized)
    fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()>;

    /// Load candles for a (symbol, interval) pair
    fn load_candles(&self, symbol: &str, interval_str: &str, limit: usize) -> anyhow::Result<Vec<Candle>>;
}
