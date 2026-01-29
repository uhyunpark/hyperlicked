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

use tracing::{debug, error, info, warn};

use crate::config::Config;

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

    /// Observer mode (follow chain without voting/proposing)
    observer_mode: bool,

    /// Queue of blocks received from network (for observers)
    received_blocks: Vec<Block>,
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
            observer_mode: false,
            received_blocks: Vec::new(),
        }
    }

    /// Create a new consensus engine in observer mode
    ///
    /// Observer mode allows following the chain without voting or proposing.
    /// Used by RPC nodes that serve API requests but don't participate in consensus.
    pub fn new_observer(config: ConsensusConfig, app: A, store: S) -> Self {
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
            observer_mode: true,
            received_blocks: Vec::new(),
        }
    }

    /// Create engine with existing state (for recovery)
    pub fn with_state(
        config: ConsensusConfig,
        app: A,
        store: S,
        committed_height: u64,
        current_view: u64,
        observer_mode: bool,
    ) -> Self {
        let mut pacemaker = Pacemaker::default();
        pacemaker.set_view(current_view);

        Self {
            config,
            safety: Safety::new(),
            pacemaker,
            app,
            store,
            pending: HashMap::new(),
            committed_height,
            observer_mode,
            received_blocks: Vec::new(),
        }
    }

    /// Check if running in observer mode
    pub fn is_observer(&self) -> bool {
        self.observer_mode
    }

    /// Get reference to the application state
    pub fn app(&self) -> &A {
        &self.app
    }

    /// Get mutable reference to the application state
    pub fn app_mut(&mut self) -> &mut A {
        &mut self.app
    }

    /// Run one iteration of the consensus loop
    ///
    /// Returns the committed block if one was committed this round.
    pub fn tick(&mut self) -> Option<Block> {
        // Observer mode: process received blocks without voting/proposing
        if self.observer_mode {
            return self.run_observer();
        }

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

        // 3. Create block (justify is the QC that certifies our parent)
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
            justify: self.safety.high_qc().cloned(),
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

        // 6. For single node: self-vote (with BLS signature if enabled)
        let vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(view, block_hash, app_hash, self.config.node_id, &bls_sk)
        } else {
            Vote::new(view, block_hash, app_hash, self.config.node_id)
        };

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

    /// Observer logic: process received blocks without voting
    ///
    /// Observers follow the chain by:
    /// 1. Verifying QC on received blocks (Byzantine detection)
    /// 2. Executing them to maintain local state
    /// 3. Verifying app_hash matches (reject on mismatch)
    /// 4. Not participating in voting or proposing
    ///
    /// Processes one block per tick to mirror validator behavior.
    fn run_observer(&mut self) -> Option<Block> {
        // Process any blocks in the queue
        if self.received_blocks.is_empty() {
            return None;
        }

        // Sort blocks by height to process in order
        self.received_blocks.sort_by_key(|b| b.height);

        // Remove any blocks we've already committed
        self.received_blocks
            .retain(|b| b.height > self.committed_height);

        // Find the next block we can commit (must be sequential)
        let next_height = self.committed_height + 1;
        let block_idx = self
            .received_blocks
            .iter()
            .position(|b| b.height == next_height);

        let block_idx = match block_idx {
            Some(idx) => idx,
            None => return None, // No sequential block available
        };

        // Remove block from queue
        let block = self.received_blocks.remove(block_idx);

        // Verify QC before committing (unless SKIP_QC_VERIFY is set)
        if !Config::global().skip_qc_verify {
            if let Err(e) = self.verify_block_qc(&block) {
                error!(
                    height = block.height,
                    error = %e,
                    "Observer rejecting block: QC verification failed"
                );
                return None;
            }
        }

        // Execute the block
        debug!(
            height = block.height,
            view = block.view,
            hash = %hash_short(&block.hash()),
            "Observer executing block"
        );

        let app_hash = self.app.execute(&block);

        // Verify app hash matches (Byzantine detection) - REJECT on mismatch
        if app_hash != block.app_hash {
            error!(
                height = block.height,
                expected = %hash_short(&block.app_hash),
                got = %hash_short(&app_hash),
                "Observer rejecting block: app hash mismatch!"
            );
            // TODO: Add rollback capability to undo the execution
            // For now, reject the block and don't commit
            return None;
        }

        // Store and commit
        self.store.save(&block);
        self.store.set_committed(&block.hash());
        self.committed_height = block.height;

        info!(
            height = block.height,
            hash = %hash_short(&block.hash()),
            "Observer committed block"
        );

        Some(block)
    }

    /// Verify a block's QC (justify certificate)
    ///
    /// Returns Ok(()) if:
    /// - Block is genesis (height <= 1, no QC needed)
    /// - QC is present and structurally valid
    fn verify_block_qc(&self, block: &Block) -> Result<(), String> {
        // Genesis and first blocks don't need QC verification
        if block.height <= 1 {
            return Ok(());
        }

        // Non-genesis blocks must have a justify certificate
        let justify = block
            .justify
            .as_ref()
            .ok_or_else(|| format!("Block {} missing QC (justify certificate)", block.height))?;

        // QC must certify the parent block
        if justify.block_hash != block.parent {
            return Err(format!(
                "QC block_hash {} doesn't match parent {}",
                hash_short(&justify.block_hash),
                hash_short(&block.parent)
            ));
        }

        // Verify BLS signature structure if present
        if justify.is_bls() {
            self.verify_bls_certificate_structure(justify)?;
        } else if justify.voters.is_empty() && justify.votes.is_empty() {
            return Err(format!(
                "QC for block {} has no voters or votes",
                block.height
            ));
        }

        Ok(())
    }

    /// Verify BLS certificate has valid structure
    fn verify_bls_certificate_structure(&self, cert: &Certificate) -> Result<(), String> {
        use crate::crypto::bls::{BlsPublicKey, BlsSignature};

        // Must have voters
        if cert.voters.is_empty() {
            return Err("Certificate has no voters".to_string());
        }

        // Must have matching voters and pubkeys
        if cert.voters.len() != cert.bls_pubkeys.len() {
            return Err(format!(
                "Certificate voter/pubkey count mismatch: {} vs {}",
                cert.voters.len(),
                cert.bls_pubkeys.len()
            ));
        }

        // Validate aggregated signature format (96 bytes for BLS)
        if cert.agg_signature.len() != 96 {
            return Err(format!(
                "Invalid aggregate signature length: {} (expected 96)",
                cert.agg_signature.len()
            ));
        }

        // Try to parse aggregated signature
        BlsSignature::from_slice(&cert.agg_signature)
            .map_err(|_| "Failed to parse aggregate signature".to_string())?;

        // Validate all public keys
        for (i, pk_bytes) in cert.bls_pubkeys.iter().enumerate() {
            if pk_bytes.len() != 48 {
                return Err(format!(
                    "Invalid BLS pubkey length at index {}: {} (expected 48)",
                    i,
                    pk_bytes.len()
                ));
            }
            let mut pk_arr = [0u8; 48];
            pk_arr.copy_from_slice(pk_bytes);
            BlsPublicKey::from_bytes(&pk_arr)
                .map_err(|_| format!("Failed to parse BLS pubkey at index {}", i))?;
        }

        Ok(())
    }

    /// Queue a block for processing (used by sync client)
    pub fn queue_block(&mut self, block: Block) {
        // Don't queue blocks we've already committed
        if block.height <= self.committed_height {
            return;
        }

        // Don't queue duplicates
        if self.received_blocks.iter().any(|b| b.height == block.height) {
            return;
        }

        debug!(
            height = block.height,
            view = block.view,
            "Queuing block for observer"
        );
        self.received_blocks.push(block);
    }

    /// Queue multiple blocks (batch operation for sync)
    pub fn queue_blocks(&mut self, blocks: Vec<Block>) {
        for block in blocks {
            self.queue_block(block);
        }
    }

    /// Get the number of queued blocks
    pub fn queued_block_count(&self) -> usize {
        self.received_blocks.len()
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

        // 4. Create vote (with BLS signature if enabled)
        let vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(view, block.hash(), local_app_hash, self.config.node_id, &bls_sk)
        } else {
            Vote::new(view, block.hash(), local_app_hash, self.config.node_id)
        };
        Some(vote)
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

        // 2-chain commit rule (HotStuff-2):
        // When we have QC for block B, commit B's PARENT.
        // This ensures block N is only committed when N+1 has been certified,
        // providing stronger Byzantine fault tolerance.
        let certified_block = self.pending.get(&qc.block_hash)
            .cloned()
            .or_else(|| self.store.get(&qc.block_hash))?;

        // HotStuff-2 Locking Rule: QC on B means B.justify.block is locked.
        // When we see QC for block B, we lock on B's justify (the QC that B extends from).
        // This prevents voting for conflicting blocks in earlier views.
        if let Some(justify) = &certified_block.justify {
            self.safety.update_locked_qc(justify.clone());
        }

        // Don't commit genesis parent (height 0 has parent = [0u8; 32])
        if certified_block.height == 0 {
            return None;
        }

        self.try_commit(&certified_block.parent)
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

    /// Handle epoch transition by updating validator set
    ///
    /// Called by application layer when epoch boundary is reached.
    /// Updates consensus configuration with the new active validator set.
    pub fn on_epoch_transition(&mut self, update: crate::app::staking::ValidatorSetUpdate) {
        if update.is_empty() {
            info!("Epoch transition: no validators in update, keeping current set");
            return;
        }

        info!(
            validators = update.len(),
            "Epoch transition: updating validator set"
        );

        // Update config with new validators and BLS keys
        self.config.update_validators(update.node_ids, update.bls_pubkeys);

        debug!(
            validators = ?self.config.validators.len(),
            "Consensus config updated with new validator set"
        );
    }

    /// Get a mutable reference to the consensus config
    ///
    /// Used for external updates (e.g., epoch transitions)
    pub fn config_mut(&mut self) -> &mut ConsensusConfig {
        &mut self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::{MemoryBlockStore, NoOpApp};

    #[test]
    fn test_2_chain_commit_rule() {
        // HotStuff-2 2-chain commit: block N commits when QC for N+1 exists
        // Genesis (height 0) is already committed at initialization
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new(config, app, store);
        assert_eq!(engine.committed_height(), 0, "Genesis starts as committed");

        // Tick 1: Certify block 1, try to commit genesis (already committed, returns None)
        let committed = engine.tick();
        assert!(committed.is_none(), "Genesis was already committed at init");
        assert_eq!(engine.committed_height(), 0);

        // Tick 2: Certify block 2, commits block 1
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(committed.unwrap().height, 1, "Second QC should commit block 1");
        assert_eq!(engine.committed_height(), 1);

        // Tick 3: Certify block 3, commits block 2
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(committed.unwrap().height, 2, "Third QC should commit block 2");
        assert_eq!(engine.committed_height(), 2);
    }

    #[test]
    fn test_single_node_produces_blocks() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new(config, app, store);

        // With 2-chain commit:
        // - Tick 1: Certifies block 1, genesis already committed -> None
        // - Tick 2: Certifies block 2, commits block 1
        // - Tick 3: Certifies block 3, commits block 2
        let committed = engine.tick();
        assert!(committed.is_none()); // Genesis already committed
        assert_eq!(engine.committed_height(), 0);

        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(engine.committed_height(), 1);

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

        // With 2-chain commit: first tick returns None (genesis already committed)
        // Then committed heights are: 1, 2, 3, 4 for ticks 2-5
        let first = engine.tick();
        assert!(first.is_none(), "First tick: genesis already committed");

        for expected_height in 1..=4 {
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

    #[test]
    fn test_observer_mode_creation() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let engine = Engine::new_observer(config, app, store);
        assert!(engine.is_observer());
        assert_eq!(engine.committed_height(), 0);
    }

    #[test]
    fn test_observer_processes_queued_blocks() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new_observer(config, app, store);
        assert!(engine.is_observer());

        // Block 1 at height 1 is exempt from QC verification
        let block1 = Block {
            view: 1,
            height: 1,
            parent: Block::genesis().hash(),
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [0u8; 32], // NoOpApp returns [0u8; 32]
            timestamp: 1000,
            justify: None,
        };

        // Block 2 needs a QC certifying block 1, so add one
        let qc_for_block1 = Certificate::new(1, block1.hash(), vec![
            Vote::new(1, block1.hash(), [0u8; 32], [1u8; 32])
        ]);

        let block2 = Block {
            view: 2,
            height: 2,
            parent: block1.hash(),
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [0u8; 32],
            timestamp: 2000,
            justify: Some(qc_for_block1),
        };

        // Queue blocks out of order
        engine.queue_block(block2.clone());
        engine.queue_block(block1.clone());
        assert_eq!(engine.queued_block_count(), 2);

        // First tick should commit block 1
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(committed.unwrap().height, 1);
        assert_eq!(engine.committed_height(), 1);

        // Second tick should commit block 2 (has valid QC for parent)
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(committed.unwrap().height, 2);
        assert_eq!(engine.committed_height(), 2);

        // No more blocks to process
        let committed = engine.tick();
        assert!(committed.is_none());
    }

    #[test]
    fn test_observer_ignores_already_committed() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new_observer(config, app, store);

        // Height 1 blocks are exempt from QC verification
        let block1 = Block {
            view: 1,
            height: 1,
            parent: Block::genesis().hash(),
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1000,
            justify: None,
        };

        // Queue and process
        engine.queue_block(block1.clone());
        engine.tick();
        assert_eq!(engine.committed_height(), 1);

        // Try to queue again - should be ignored
        engine.queue_block(block1);
        assert_eq!(engine.queued_block_count(), 0);
    }

    #[test]
    fn test_observer_rejects_block_without_qc() {
        let config = ConsensusConfig::single_node();
        let app = NoOpApp;
        let store = MemoryBlockStore::new();

        let mut engine = Engine::new_observer(config, app, store);

        // Block 1 at height 1 is exempt from QC check
        let block1 = Block {
            view: 1,
            height: 1,
            parent: Block::genesis().hash(),
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1000,
            justify: None,
        };

        // Block 2 at height 2 REQUIRES a QC but doesn't have one
        let block2 = Block {
            view: 2,
            height: 2,
            parent: block1.hash(),
            payload: vec![],
            proposer: [1u8; 32],
            app_hash: [0u8; 32],
            timestamp: 2000,
            justify: None, // Missing required QC
        };

        // Process block 1 (should succeed)
        engine.queue_block(block1.clone());
        let committed = engine.tick();
        assert!(committed.is_some());
        assert_eq!(engine.committed_height(), 1);

        // Process block 2 (should be rejected due to missing QC)
        engine.queue_block(block2);
        let committed = engine.tick();
        assert!(committed.is_none(), "Block without QC should be rejected");
        assert_eq!(engine.committed_height(), 1, "Height should not advance");
    }
}
