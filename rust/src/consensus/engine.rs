//! Consensus Engine: Main Loop
//!
//! The engine orchestrates the HotStuff-2 protocol:
//! - Leader: proposes blocks, collects votes, broadcasts QCs
//! - Follower: votes on valid proposals, commits on QC
//!
//! For Phase 1 (single node), we simplify:
//! - Always the leader
//! - No network (votes are self-generated)
//! - Focus on block production and commit logic

use std::collections::HashMap;

use tracing::{debug, info, warn};

use super::{AppHook, BlockStore, Pacemaker, Safety};
use crate::types::{
    hash_short, Block, Certificate, ConsensusConfig, Hash, Propose, View, Vote,
};

/// Consensus engine state
pub struct Engine<A, S>
where
    A: AppHook,
    S: BlockStore,
{
    /// Configuration
    config: ConsensusConfig,

    /// Safety module (voting rules)
    safety: Safety,

    /// Pacemaker (view advancement)
    pacemaker: Pacemaker,

    /// Application hook
    app: A,

    /// Block storage
    store: S,

    /// Pending blocks (received but not committed)
    pending: HashMap<Hash, Block>,

    /// Last committed height
    committed_height: u64,
}

impl<A, S> Engine<A, S>
where
    A: AppHook,
    S: BlockStore,
{
    /// Create a new consensus engine
    pub fn new(config: ConsensusConfig, app: A, store: S) -> Self {
        // Initialize with genesis block
        let genesis = Block::genesis();
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        Self {
            config,
            safety: Safety::new(),
            pacemaker: Pacemaker::default(),
            app,
            store,
            pending: HashMap::new(),
            committed_height: 0,
        }
    }

    /// Run one iteration of the consensus loop
    ///
    /// Returns the committed block if one was committed this round.
    pub fn tick(&mut self) -> Option<Block> {
        let view = self.pacemaker.current_view();

        if self.config.is_leader(view) {
            self.run_leader(view)
        } else {
            self.run_follower(view)
        }
    }

    /// Leader logic: propose block, collect votes, form QC
    fn run_leader(&mut self, view: View) -> Option<Block> {
        info!(view, "Running as leader");

        // 1. Get parent block (from high_qc or genesis)
        let parent = self.get_proposal_parent();
        let parent_hash = parent.hash();

        // 2. Prepare payload from app
        let payload = self.app.prepare_payload(&parent);

        // 3. Create block
        let mut block = Block {
            view,
            height: parent.height + 1,
            parent: parent_hash,
            payload,
            proposer: self.config.node_id,
            app_hash: [0u8; 32], // Will be set after execution
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };

        // 4. Execute to get app_hash
        let app_hash = self.app.execute(&block);
        block.app_hash = app_hash;

        let block_hash = block.hash();
        debug!(
            view,
            height = block.height,
            hash = %hash_short(&block_hash),
            "Proposed block"
        );

        // 5. Store pending block
        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        // 6. For single node: self-vote
        let vote = Vote::new(view, block_hash, app_hash, self.config.node_id);

        // 7. Form QC (single node = 1 vote is quorum)
        let qc = Certificate::new(view, block_hash, vec![vote]);

        // 8. Process QC (may commit previous block)
        let committed = self.process_qc(qc);

        // 9. Advance view
        if let Some(ref qc) = self.safety.high_qc() {
            self.pacemaker.advance_view(qc);
        }

        committed
    }

    /// Follower logic: wait for proposal, vote if safe
    fn run_follower(&mut self, view: View) -> Option<Block> {
        // In Phase 1 (single node), we're always leader
        // This is placeholder for Phase 2
        debug!(view, "Would run as follower (not implemented yet)");
        None
    }

    /// Process a received proposal
    pub fn on_propose(&mut self, propose: Propose) -> Option<Vote> {
        let block = &propose.block;
        let view = block.view;

        debug!(
            view,
            height = block.height,
            hash = %hash_short(&block.hash()),
            "Received proposal"
        );

        // 1. Execute block locally
        let local_app_hash = self.app.execute(block);

        // 2. Check safety
        if let Err(e) = self.safety.safe_to_vote(block, local_app_hash) {
            warn!(view, error = %e, "Unsafe to vote");
            return None;
        }

        // 3. Record vote and store block
        self.safety.record_vote(view);
        self.store.save(block);
        self.pending.insert(block.hash(), block.clone());

        // 4. Create vote
        Some(Vote::new(
            view,
            block.hash(),
            local_app_hash,
            self.config.node_id,
        ))
    }

    /// Process a quorum certificate
    fn process_qc(&mut self, qc: Certificate) -> Option<Block> {
        debug!(
            view = qc.view,
            hash = %hash_short(&qc.block_hash),
            votes = qc.vote_count(),
            "Processing QC"
        );

        // Update high_qc
        self.safety.update_high_qc(qc.clone());

        // 2-chain commit rule:
        // If we have QC for block B, and QC for B's parent (high_qc before this),
        // then B's parent is committed.

        // For simplicity in Phase 1: commit the block that this QC certifies
        // (This is slightly simplified; real 2-chain would commit parent)
        self.try_commit(&qc.block_hash)
    }

    /// Try to commit a block and its ancestors
    fn try_commit(&mut self, block_hash: &Hash) -> Option<Block> {
        let block = match self.pending.remove(block_hash) {
            Some(b) => b,
            None => self.store.get(block_hash)?,
        };

        // Only commit if height is greater than committed
        if block.height <= self.committed_height {
            return None;
        }

        // Commit ancestors first (recursive)
        if block.height > self.committed_height + 1 {
            self.try_commit(&block.parent);
        }

        // Commit this block
        info!(
            height = block.height,
            hash = %hash_short(block_hash),
            "Committed block"
        );

        self.store.set_committed(block_hash);
        self.committed_height = block.height;

        // Prune old pending blocks
        self.pending.retain(|_, b| b.height > self.committed_height);
        self.safety.prune_votes_below(block.view);

        Some(block)
    }

    /// Get parent block for new proposal
    fn get_proposal_parent(&self) -> Block {
        // Use block from high_qc if available
        if let Some(qc) = self.safety.high_qc() {
            if let Some(block) = self.store.get(&qc.block_hash) {
                return block;
            }
        }

        // Fall back to genesis
        self.store.get_by_height(0).unwrap_or_else(Block::genesis)
    }

    /// Get current view
    pub fn current_view(&self) -> View {
        self.pacemaker.current_view()
    }

    /// Get committed height
    pub fn committed_height(&self) -> u64 {
        self.committed_height
    }

    /// Check if we're the leader for current view
    pub fn is_leader(&self) -> bool {
        self.config.is_leader(self.pacemaker.current_view())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{MemoryBlockStore, NoOpApp};

    #[test]
    fn test_single_node_produces_blocks() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new(config, app, store);

        // First tick should produce a block
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(engine.committed_height(), 1);

        // Second tick should produce another block
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(engine.committed_height(), 2);
    }

    #[test]
    fn test_blocks_chain_correctly() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new(config, app, store);

        // Produce several blocks
        for expected_height in 1..=5 {
            let block = engine.tick().expect("should produce block");
            assert_eq!(block.height, expected_height);
        }
    }

    #[test]
    fn test_views_advance() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new(config, app, store);

        let view_before = engine.current_view();
        engine.tick();
        let view_after = engine.current_view();

        assert!(view_after > view_before);
    }
}
