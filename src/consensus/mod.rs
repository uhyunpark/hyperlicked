//! HotStuff-2 Consensus Implementation
//!
//! This module implements the HotStuff-2 BFT consensus protocol.
//!
//! ## Components
//!
//! - `engine`: Main consensus loop (leader/follower logic)
//! - `safety`: Voting rules (when it's safe to vote)
//! - `pacemaker`: View advancement (timeouts, view changes)
//!
//! ## Protocol Overview
//!
//! HotStuff-2 uses a 2-chain commit rule:
//! 1. Block N is proposed, gets votes, forms QC_N
//! 2. Block N+1 is proposed (extends N), gets votes, forms QC_N+1
//! 3. When QC_N+1 exists, Block N is COMMITTED
//!
//! See `docs/specs/consensus.md` for full specification.

mod aggregator;
mod engine;
mod message_handler;
mod pacemaker;
pub mod runner;
mod safety;
pub mod timeout;
pub mod view_change;

pub use aggregator::VoteAggregator;
pub use engine::Engine;
pub use pacemaker::Pacemaker;
pub use runner::ConsensusRunner;
pub use safety::Safety;
pub use timeout::{
    create_signed_timeout, verify_timeout_certificate, TimeoutCollector, TimeoutError,
};
pub use view_change::{
    create_signed_view_change, validate_view_change, validate_view_change_with_sig,
    ViewChangeCollector, ViewChangeError,
};

use crate::types::{Block, Hash};

// =============================================================================
// Traits (Module Boundaries)
// =============================================================================

/// Application hook - consensus calls this to execute blocks
pub trait AppHook: Send + Sync {
    /// Prepare payload for next block (called by leader)
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;

    /// Execute block and return state hash (called after commit)
    fn execute(&mut self, block: &Block) -> Hash;
}

/// Block storage abstraction
pub trait BlockStore: Send + Sync {
    /// Save a block
    fn save(&self, block: &Block);

    /// Get block by hash
    fn get(&self, hash: &Hash) -> Option<Block>;

    /// Get block by height
    fn get_by_height(&self, height: u64) -> Option<Block>;

    /// Mark block as committed
    fn set_committed(&self, hash: &Hash);

    /// Get the highest committed block
    fn get_committed_head(&self) -> Option<Block>;
}

/// Simple in-memory block store for testing
#[derive(Default)]
pub struct MemoryBlockStore {
    blocks: std::sync::RwLock<std::collections::HashMap<Hash, Block>>,
    by_height: std::sync::RwLock<std::collections::HashMap<u64, Hash>>,
    committed: std::sync::RwLock<Option<Hash>>,
}

impl MemoryBlockStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlockStore for MemoryBlockStore {
    fn save(&self, block: &Block) {
        let hash = block.hash();
        self.blocks.write().unwrap().insert(hash, block.clone());
        self.by_height.write().unwrap().insert(block.height, hash);
    }

    fn get(&self, hash: &Hash) -> Option<Block> {
        self.blocks.read().unwrap().get(hash).cloned()
    }

    fn get_by_height(&self, height: u64) -> Option<Block> {
        let hash = self.by_height.read().unwrap().get(&height).copied()?;
        self.get(&hash)
    }

    fn set_committed(&self, hash: &Hash) {
        *self.committed.write().unwrap() = Some(*hash);
    }

    fn get_committed_head(&self) -> Option<Block> {
        let hash = self.committed.read().unwrap().as_ref().copied()?;
        self.get(&hash)
    }
}

/// Simple no-op application for testing consensus in isolation
pub struct NoOpApp;

impl AppHook for NoOpApp {
    fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
        vec![] // Empty payload
    }

    fn execute(&mut self, _block: &Block) -> Hash {
        [0u8; 32] // Constant state hash
    }
}
