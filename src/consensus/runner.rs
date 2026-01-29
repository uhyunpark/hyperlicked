//! Async Consensus Runner
//!
//! Orchestrates the consensus engine with network I/O.
//! This is the main entry point for running a validator node.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::message_handler::{handle_view_message, store_vote_with_equivocation_check};
use super::{
    create_signed_timeout, AppHook, BlockStore, EquivocationDetector, EquivocationProof,
    MemoryBlockStore, NoOpApp, Pacemaker, Safety, TimeoutCollector,
};
use crate::config::Config;
use crate::network::{Network, SyncClient, SyncHandler, TcpNetwork};
use crate::storage::{ConsensusState, PersistentStore};
use crate::types::{
    hash_short, Block, Certificate, ConsensusConfig, Hash, Message, NewView, Prepare, Propose,
    TimeoutCertificate, View, ViewChangeCertificate, Vote,
};

/// Async consensus runner
pub struct ConsensusRunner {
    /// Configuration
    config: ConsensusConfig,

    /// Safety module (voting rules)
    safety: Safety,

    /// Pacemaker (view advancement)
    pacemaker: Pacemaker,

    /// Application hook
    app: Box<dyn AppHook>,

    /// Block storage
    store: Box<dyn BlockStore>,

    /// Network
    network: TcpNetwork,

    /// Pending blocks
    pending: HashMap<Hash, Block>,

    /// Collected votes for current proposal
    votes: HashMap<Hash, Vec<Vote>>,

    /// Last committed height
    committed_height: u64,

    /// Last committed block hash
    committed_hash: Hash,

    /// Optional persistent store for crash recovery
    persistent_store: Option<Arc<dyn PersistentStore + Send + Sync>>,

    /// Timeout certificate collector (for BLS-signed timeout aggregation)
    timeout_collector: Option<TimeoutCollector>,

    /// Sync handler for responding to sync requests (if persistent store enabled)
    sync_handler: Option<SyncHandler>,

    /// Sync client for catching up with peers
    sync_client: SyncClient,

    /// Whether we're currently syncing
    syncing: bool,

    /// Equivocation detector for Byzantine fault detection
    equivocation_detector: EquivocationDetector,

    /// CRITICAL-7: Vote timestamps per validator for rate limiting
    vote_timestamps: HashMap<crate::types::NodeId, VecDeque<Instant>>,
}

impl ConsensusRunner {
    /// Create a new consensus runner
    pub async fn new(
        config: ConsensusConfig,
        network: TcpNetwork,
    ) -> Result<Self> {
        // Initialize with genesis block
        let store = Box::new(MemoryBlockStore::new());
        let genesis = Block::genesis();
        let genesis_hash = genesis.hash();
        store.save(&genesis);
        store.set_committed(&genesis_hash);

        // Create timeout collector if BLS is enabled
        let timeout_collector = Self::create_timeout_collector(&config);

        Ok(Self {
            config,
            safety: Safety::new(),
            pacemaker: Pacemaker::new(Duration::from_secs(3)),
            app: Box::new(NoOpApp),
            store,
            network,
            pending: HashMap::new(),
            votes: HashMap::new(),
            committed_height: 0,
            committed_hash: genesis_hash,
            persistent_store: None,
            timeout_collector,
            sync_handler: None, // No persistent store for basic constructor
            sync_client: SyncClient::new(0),
            syncing: false,
            equivocation_detector: EquivocationDetector::new(),
            vote_timestamps: HashMap::new(),
        })
    }

    /// Create timeout collector from config if BLS is enabled
    fn create_timeout_collector(config: &ConsensusConfig) -> Option<TimeoutCollector> {
        use crate::crypto::bls::BlsPublicKey;

        if !config.bls_enabled() {
            return None;
        }

        let mut validator_pubkeys = HashMap::new();
        for (i, node_id) in config.validators.iter().enumerate() {
            if let Some(pk_bytes) = config.bls_pubkeys.get(i) {
                // Convert Vec<u8> to [u8; 48] for BLS public key
                if pk_bytes.len() == 48 {
                    let mut pk_array = [0u8; 48];
                    pk_array.copy_from_slice(pk_bytes);
                    if let Ok(pk) = BlsPublicKey::from_bytes(&pk_array) {
                        validator_pubkeys.insert(*node_id, pk);
                    }
                }
            }
        }

        if validator_pubkeys.is_empty() {
            return None;
        }

        Some(TimeoutCollector::new(config.quorum(), validator_pubkeys))
    }

    /// Create a consensus runner with crash recovery support.
    ///
    /// If `persistent_store` contains prior state, the runner will recover:
    /// - high_qc and locked_qc for chain extension
    /// - voted_views to prevent double-voting (CRITICAL for Byzantine safety)
    /// - current_view and committed height/hash
    pub async fn new_with_recovery(
        config: ConsensusConfig,
        network: TcpNetwork,
        persistent_store: Arc<dyn PersistentStore + Send + Sync>,
    ) -> Result<Self> {
        // Try to load prior consensus state
        let recovered_state = persistent_store.load_consensus_state()?;

        let (safety, pacemaker, committed_height, committed_hash) = if let Some(state) = recovered_state {
            info!(
                view = state.current_view,
                height = state.committed_height,
                voted_views = state.voted_views.len(),
                "Recovered consensus state from storage"
            );

            let safety = Safety::with_state(
                state.high_qc,
                state.locked_qc,
                &state.voted_views,
            );

            let mut pacemaker = Pacemaker::new(Duration::from_secs(3));
            // Advance pacemaker to recovered view
            pacemaker.set_view(state.current_view);
            // Restore timeout state for exponential backoff and ViewChange tracking
            pacemaker.set_timeout_state(state.consecutive_timeouts, state.vc_sent_for_view);

            (safety, pacemaker, state.committed_height, state.committed_hash)
        } else {
            // No prior state - start fresh
            let genesis = Block::genesis();
            let genesis_hash = genesis.hash();
            persistent_store.save(&genesis);
            persistent_store.set_committed(&genesis_hash);

            (
                Safety::new(),
                Pacemaker::new(Duration::from_secs(3)),
                0,
                genesis_hash,
            )
        };

        // Create timeout collector if BLS is enabled
        let timeout_collector = Self::create_timeout_collector(&config);

        // Create sync handler with persistent store
        use std::sync::atomic::AtomicU64;
        let height_tracker = Arc::new(AtomicU64::new(committed_height));
        let sync_handler = SyncHandler::new(
            persistent_store.clone(),
            height_tracker,
        );

        Ok(Self {
            config,
            safety,
            pacemaker,
            app: Box::new(NoOpApp),
            store: Box::new(MemoryBlockStore::new()), // Use persistent_store for blocks
            network,
            pending: HashMap::new(),
            votes: HashMap::new(),
            committed_height,
            committed_hash,
            persistent_store: Some(persistent_store),
            timeout_collector,
            sync_handler: Some(sync_handler),
            sync_client: SyncClient::new(committed_height),
            syncing: false,
            equivocation_detector: EquivocationDetector::new(),
            vote_timestamps: HashMap::new(),
        })
    }

    /// Run the consensus loop
    pub async fn run(&mut self) -> Result<()> {
        info!(
            node = %hash_short(&self.config.node_id),
            "Starting consensus runner"
        );

        // Enable view change protocol
        self.pacemaker.with_view_change(self.config.quorum());

        loop {
            let view = self.pacemaker.current_view();
            let is_leader = self.config.is_leader(view);

            if is_leader {
                self.run_leader_round(view).await?;
            } else {
                self.run_follower_round(view).await?;
            }

            // Configurable delay to prevent tight loop (0 = yield only)
            let delay_ms = Config::global().consensus_loop_delay_ms;
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
    }

    /// Run one round as leader
    async fn run_leader_round(&mut self, view: View) -> Result<()> {
        info!(view, "Running as LEADER");

        let parent = self.get_proposal_parent();
        let payload = self.app.prepare_payload(&parent);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();

        // Create block with justify (QC that certifies our parent)
        let justify = self.safety.high_qc().cloned();
        let mut block = Block {
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload,
            proposer: self.config.node_id,
            app_hash: [0u8; 32],
            timestamp: now.as_millis() as u64,
            justify: justify.clone(),
        };
        block.app_hash = self.app.execute(&block);

        let block_hash = block.hash();
        info!(view, height = block.height, hash = %hash_short(&block_hash), "Proposing block");

        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        let propose = Propose { block: block.clone(), justify };
        self.network.broadcast_propose(propose).await?;

        // Self-vote (with BLS signature if enabled)
        let self_vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(view, block_hash, block.app_hash, self.config.node_id, &bls_sk)
        } else {
            Vote::new(view, block_hash, block.app_hash, self.config.node_id)
        };
        self.votes.entry(block_hash).or_default().push(self_vote);

        // Collect votes
        let quorum = self.config.quorum();
        let votes = self.collect_votes(block_hash, quorum, Duration::from_secs(3)).await;

        if votes.len() >= quorum {
            info!(view, votes = votes.len(), "Collected quorum, forming QC");
            let qc = Certificate::new(view, block_hash, votes);
            let prepare = Prepare { view, qc: qc.clone() };
            self.network.broadcast_prepare(prepare).await?;
            self.process_qc(qc);
            if let Some(ref high_qc) = self.safety.high_qc() {
                self.pacemaker.advance_view(high_qc);
            }
        } else {
            warn!(view, votes = votes.len(), needed = quorum, "Failed to collect quorum");
            self.pacemaker.record_timeout();
        }

        Ok(())
    }

    /// Run one round as follower
    async fn run_follower_round(&mut self, view: View) -> Result<()> {
        debug!(view, "Running as FOLLOWER");

        let round_timeout = self.pacemaker.current_timeout();

        // Wait for proposal or timeout
        match timeout(round_timeout, self.wait_for_proposal(view)).await {
            Ok(Ok(propose)) => {
                // Got proposal, process it
                if let Some(vote) = self.process_proposal(propose) {
                    // Send vote to leader
                    let leader = self.config.leader_of(view);
                    if let Err(e) = self.network.send_vote(leader, vote).await {
                        warn!(error = %e, "Failed to send vote");
                    }
                }

                // Wait for prepare
                match timeout(round_timeout, self.wait_for_prepare(view)).await {
                    Ok(Ok(prepare)) => {
                        self.process_prepare(prepare);
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "Error waiting for prepare");
                    }
                    Err(_) => {
                        debug!(view, "Timeout waiting for prepare");
                        self.pacemaker.record_timeout();
                    }
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Error waiting for proposal");
                self.pacemaker.record_timeout();
            }
            Err(_) => {
                debug!(view, "Timeout waiting for proposal");
                self.handle_timeout().await?;
            }
        }

        Ok(())
    }

    /// Handle timeout by broadcasting ViewChange and Timeout (for TC)
    async fn handle_timeout(&mut self) -> Result<()> {
        let view = self.pacemaker.current_view();
        debug!(view, "Handling timeout with view change");

        // Broadcast BLS-signed Timeout for TimeoutCertificate collection
        if let Some(bls_sk) = self.config.bls_secret_key() {
            let timeout = create_signed_timeout(
                view,
                self.safety.high_qc_view(),
                self.config.node_id,
                &bls_sk,
            );

            // Broadcast timeout
            if let Err(e) = self.network.broadcast(&Message::Timeout(timeout.clone())).await {
                warn!(error = %e, "Failed to broadcast Timeout");
            }

            // Process our own timeout (might reach quorum)
            if let Some(tc) = self.process_timeout(timeout) {
                self.handle_timeout_certificate(tc).await?;
            }
        }

        // Create and broadcast ViewChange (legacy/fallback protocol)
        if let Some(vc) = self.pacemaker.create_view_change(
            self.config.node_id,
            self.safety.high_qc().cloned(),
        ) {
            info!(
                from_view = vc.from_view,
                to_view = vc.to_view,
                "Broadcasting ViewChange"
            );

            // Broadcast to all validators
            if let Err(e) = self.network.broadcast_view_change(vc.clone()).await {
                warn!(error = %e, "Failed to broadcast ViewChange");
            }

            // Process our own view change (might reach quorum)
            if let Some(vcc) = self.pacemaker.on_view_change(vc) {
                self.handle_view_change_certificate(vcc).await?;
            }
        }

        self.pacemaker.record_timeout();
        Ok(())
    }

    /// Process a received Timeout message
    fn process_timeout(&mut self, timeout: crate::types::Timeout) -> Option<TimeoutCertificate> {
        let collector = self.timeout_collector.as_mut()?;

        match collector.add(timeout) {
            Ok(Some(tc)) => {
                info!(view = tc.view, signers = tc.signers.len(), "Timeout quorum reached");
                Some(tc)
            }
            Ok(None) => None,
            Err(e) => {
                warn!(error = %e, "Failed to process timeout");
                None
            }
        }
    }

    /// Handle TimeoutCertificate when quorum reached
    async fn handle_timeout_certificate(&mut self, tc: TimeoutCertificate) -> Result<()> {
        let new_view = tc.view + 1;
        info!(
            timed_out_view = tc.view,
            new_view,
            high_qc_view = tc.high_qc_view,
            "TimeoutCertificate formed, advancing view"
        );

        // Advance to new view
        self.pacemaker.advance_to(new_view);

        // Prune old timeouts
        if let Some(ref mut collector) = self.timeout_collector {
            collector.prune_below(tc.view);
        }

        Ok(())
    }

    /// Handle ViewChangeCertificate when quorum reached
    async fn handle_view_change_certificate(&mut self, vcc: ViewChangeCertificate) -> Result<()> {
        let new_view = vcc.view;
        let new_leader = self.config.leader_of(new_view);

        info!(
            new_view,
            new_leader = %hash_short(&new_leader),
            "ViewChange quorum reached"
        );

        // If we're the new leader, broadcast NewView
        if new_leader == self.config.node_id {
            let high_qc = vcc.highest_qc().cloned();

            let nv = NewView {
                view: new_view,
                high_qc: high_qc.clone(),
                view_change_cert: vcc,
            };

            info!(view = new_view, "Broadcasting NewView as new leader");
            self.network.broadcast_new_view(nv.clone()).await?;

            // Update our own state
            self.pacemaker.on_new_view(&nv);
            if let Some(qc) = high_qc {
                self.safety.update_high_qc(qc);
            }
        }

        Ok(())
    }

    /// Wait for a proposal for the given view
    async fn wait_for_proposal(&mut self, target_view: View) -> Result<Propose> {
        loop {
            let (from, msg) = self.network.recv_msg().await?;

            match msg {
                Message::Propose(propose) => {
                    if propose.block.view == target_view {
                        return Ok(propose);
                    }
                    debug!(got = propose.block.view, expected = target_view, "Wrong view proposal");
                }
                Message::Vote(vote) => {
                    // CRITICAL-7: Rate limit votes to prevent DoS
                    if self.is_vote_rate_limited(&vote.voter) {
                        continue;
                    }
                    if let Some(proof) = store_vote_with_equivocation_check(
                        &mut self.votes,
                        vote,
                        &mut self.equivocation_detector,
                    ) {
                        self.handle_equivocation(proof);
                    }
                }
                Message::Prepare(prepare) if prepare.view >= target_view => {
                    self.process_prepare(prepare);
                }
                Message::Timeout(timeout) => {
                    if let Some(tc) = self.process_timeout(timeout) {
                        // TC formed - advance view
                        self.pacemaker.advance_to(tc.view + 1);
                    }
                }
                Message::SyncRequest(req) => {
                    self.handle_sync_request(from, req).await;
                }
                Message::SyncResponse(resp) => {
                    self.handle_sync_response(resp).await;
                }
                ref m @ (Message::ViewChange(_) | Message::NewView(_)) => {
                    handle_view_message(m, &from, &mut self.pacemaker, &mut self.safety);
                }
                _ => {}
            }
        }
    }

    /// Wait for a prepare for the given view
    async fn wait_for_prepare(&mut self, target_view: View) -> Result<Prepare> {
        loop {
            let (from, msg) = self.network.recv_msg().await?;

            match msg {
                Message::Prepare(prepare) if prepare.view >= target_view => return Ok(prepare),
                Message::Vote(vote) => {
                    // CRITICAL-7: Rate limit votes to prevent DoS
                    if self.is_vote_rate_limited(&vote.voter) {
                        continue;
                    }
                    if let Some(proof) = store_vote_with_equivocation_check(
                        &mut self.votes,
                        vote,
                        &mut self.equivocation_detector,
                    ) {
                        self.handle_equivocation(proof);
                    }
                }
                Message::Timeout(timeout) => {
                    if let Some(tc) = self.process_timeout(timeout) {
                        self.pacemaker.advance_to(tc.view + 1);
                    }
                }
                Message::SyncRequest(req) => {
                    self.handle_sync_request(from, req).await;
                }
                Message::SyncResponse(resp) => {
                    self.handle_sync_response(resp).await;
                }
                ref m @ (Message::ViewChange(_) | Message::NewView(_)) => {
                    handle_view_message(m, &from, &mut self.pacemaker, &mut self.safety);
                }
                _ => {}
            }
        }
    }

    /// Collect votes until quorum or timeout
    async fn collect_votes(
        &mut self,
        block_hash: Hash,
        quorum: usize,
        timeout_duration: Duration,
    ) -> Vec<Vote> {
        let deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            let current_votes = self.votes.get(&block_hash).map(|v| v.len()).unwrap_or(0);
            if current_votes >= quorum || tokio::time::Instant::now() >= deadline {
                return self.votes.remove(&block_hash).unwrap_or_default();
            }

            let remaining = deadline - tokio::time::Instant::now();
            match timeout(remaining, self.network.recv_msg()).await {
                Ok(Ok((from, msg))) => match msg {
                    Message::Vote(vote) if vote.block_hash == block_hash => {
                        // CRITICAL-7: Rate limit votes to prevent DoS
                        if self.is_vote_rate_limited(&vote.voter) {
                            continue;
                        }
                        debug!(from = %hash_short(&from), view = vote.view, "Received vote");
                        // Check for equivocation before storing
                        if let Some(proof) = store_vote_with_equivocation_check(
                            &mut self.votes,
                            vote,
                            &mut self.equivocation_detector,
                        ) {
                            self.handle_equivocation(proof);
                        }
                    }
                    Message::Vote(vote) => {
                        // CRITICAL-7: Rate limit votes to prevent DoS
                        if self.is_vote_rate_limited(&vote.voter) {
                            continue;
                        }
                        // Vote for different block - still check for equivocation
                        if let Some(proof) = store_vote_with_equivocation_check(
                            &mut self.votes,
                            vote,
                            &mut self.equivocation_detector,
                        ) {
                            self.handle_equivocation(proof);
                        }
                    }
                    Message::Timeout(timeout_msg) => {
                        if let Some(tc) = self.process_timeout(timeout_msg) {
                            self.pacemaker.advance_to(tc.view + 1);
                        }
                    }
                    Message::SyncRequest(req) => {
                        self.handle_sync_request(from, req).await;
                    }
                    Message::SyncResponse(resp) => {
                        self.handle_sync_response(resp).await;
                    }
                    ref m @ (Message::ViewChange(_) | Message::NewView(_)) => {
                        handle_view_message(m, &from, &mut self.pacemaker, &mut self.safety);
                    }
                    _ => {}
                },
                Ok(Err(e)) => warn!(error = %e, "Error receiving message"),
                Err(_) => break,
            }
        }

        self.votes.remove(&block_hash).unwrap_or_default()
    }

    /// Process a proposal
    fn process_proposal(&mut self, propose: Propose) -> Option<Vote> {
        let mut block = propose.block;
        let view = block.view;

        debug!(
            view,
            height = block.height,
            hash = %hash_short(&block.hash()),
            "Processing proposal"
        );

        // Execute block
        let local_app_hash = self.app.execute(&block);

        // Check safety
        if let Err(e) = self.safety.safe_to_vote(&block, local_app_hash) {
            warn!(view, error = %e, "Unsafe to vote");
            return None;
        }

        // Copy justify from proposal to block (for locked_qc tracking)
        if block.justify.is_none() {
            block.justify = propose.justify.clone();
        }

        // Record vote and store block with justify
        self.safety.record_vote(view);
        self.store.save(&block);
        self.pending.insert(block.hash(), block.clone());

        // CRITICAL: Persist voted_views immediately after recording vote.
        // This prevents double-voting after crash recovery.
        // SAFETY: If persistence fails, we MUST halt to prevent Byzantine failure.
        // A validator that continues without persisting voted_views could double-vote
        // after a crash, violating BFT safety assumptions.
        if let Err(e) = self.persist_consensus_state() {
            // This is a CRITICAL safety violation - panic to halt the validator
            panic!(
                "CRITICAL: Failed to persist consensus state after vote in view {}: {}. \
                Halting to prevent potential double-voting after crash recovery.",
                view, e
            );
        }

        // Update high_qc if proposal includes one
        if let Some(justify) = propose.justify {
            self.safety.update_high_qc(justify);
        }

        // Create vote (with BLS signature if enabled)
        let vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(
                view,
                block.hash(),
                local_app_hash,
                self.config.node_id,
                &bls_sk,
            )
        } else {
            Vote::new(
                view,
                block.hash(),
                local_app_hash,
                self.config.node_id,
            )
        };
        Some(vote)
    }

    /// Process a prepare message
    fn process_prepare(&mut self, prepare: Prepare) {
        debug!(view = prepare.view, "Processing prepare");

        // Update high_qc
        self.safety.update_high_qc(prepare.qc.clone());

        // Try to commit
        self.process_qc(prepare.qc);

        // Advance view
        if let Some(ref high_qc) = self.safety.high_qc() {
            self.pacemaker.advance_view(high_qc);
        }
    }

    /// Process a quorum certificate
    fn process_qc(&mut self, qc: Certificate) {
        debug!(
            view = qc.view,
            hash = %hash_short(&qc.block_hash),
            "Processing QC"
        );

        // Update high_qc
        self.safety.update_high_qc(qc.clone());

        // 2-chain commit rule (HotStuff-2):
        // When we have QC for block B, commit B's PARENT.
        // This ensures block N is only committed when N+1 has been certified.
        let certified_block = self.pending.get(&qc.block_hash)
            .cloned()
            .or_else(|| self.store.get(&qc.block_hash));

        if let Some(block) = certified_block {
            // HotStuff-2 Locking Rule: QC on B means B.justify.block is locked.
            // When we see QC for block B, we lock on B's justify (the QC that B extends from).
            // This prevents voting for conflicting blocks in earlier views.
            if let Some(justify) = &block.justify {
                self.safety.update_locked_qc(justify.clone());
            }

            // Don't commit genesis parent (height 0 has parent = [0u8; 32])
            if block.height > 0 {
                self.try_commit(&block.parent);
            }
        }
    }

    /// Try to commit a block
    fn try_commit(&mut self, block_hash: &Hash) -> Option<Block> {
        let block = match self.pending.remove(block_hash) {
            Some(b) => b,
            None => self.store.get(block_hash)?,
        };

        if block.height <= self.committed_height {
            return None;
        }

        // Commit ancestors first
        if block.height > self.committed_height + 1 {
            self.try_commit(&block.parent);
        }

        // Commit
        info!(
            height = block.height,
            hash = %hash_short(block_hash),
            "COMMITTED block"
        );

        self.store.set_committed(block_hash);
        self.committed_height = block.height;
        self.committed_hash = *block_hash;

        // Persist state if we have a persistent store
        // Note: Commit persistence is less critical than vote persistence,
        // but we still halt on failure to ensure consistent recovery.
        if let Err(e) = self.persist_consensus_state() {
            panic!(
                "CRITICAL: Failed to persist consensus state after commit at height {}: {}. \
                Halting to prevent inconsistent state recovery.",
                block.height, e
            );
        }

        // Prune
        self.pending.retain(|_, b| b.height > self.committed_height);
        self.safety.prune_votes_below(block.view);
        self.equivocation_detector.prune_below(block.view);
        // CRITICAL-7: Prune old vote collections to prevent unbounded memory growth
        self.prune_old_votes(block.view);

        Some(block)
    }

    /// Get parent for new proposal
    fn get_proposal_parent(&self) -> Block {
        if let Some(qc) = self.safety.high_qc() {
            if let Some(block) = self.store.get(&qc.block_hash) {
                return block;
            }
        }
        self.store.get_by_height(0).unwrap_or_else(Block::genesis)
    }

    /// Get current committed height
    pub fn committed_height(&self) -> u64 {
        self.committed_height
    }

    /// Set a custom application hook
    pub fn with_app<A: AppHook + 'static>(mut self, app: A) -> Self {
        self.app = Box::new(app);
        self
    }

    /// Persist consensus state to storage.
    ///
    /// CRITICAL: This must be called after each vote to prevent double-voting
    /// after crash recovery. The voted_views set must survive crashes.
    fn persist_consensus_state(&self) -> Result<()> {
        if let Some(ref store) = self.persistent_store {
            let (consecutive_timeouts, vc_sent_for_view) = self.pacemaker.timeout_state();
            let state = ConsensusState {
                high_qc: self.safety.high_qc().cloned(),
                locked_qc: self.safety.locked_qc().cloned(),
                voted_views: self.safety.voted_views(),
                current_view: self.pacemaker.current_view(),
                committed_height: self.committed_height,
                committed_hash: self.committed_hash,
                consecutive_timeouts,
                vc_sent_for_view,
            };
            store.save_consensus_state(&state)?;
        }
        Ok(())
    }

    /// Handle detected equivocation (Byzantine fault).
    ///
    /// This is called when a validator is caught voting for two different blocks
    /// in the same view. The evidence can be submitted to the staking system
    /// for slashing.
    fn handle_equivocation(&self, proof: EquivocationProof) {
        // Log the equivocation - this is a CRITICAL security event
        warn!(
            view = proof.view,
            offender = %hash_short(&proof.offender),
            hash_a = %hash_short(&proof.hash_a),
            hash_b = %hash_short(&proof.hash_b),
            "BYZANTINE FAULT: Equivocation detected! Validator voted for conflicting blocks."
        );

        // TODO: Submit evidence to app layer for slashing
        // The app layer (AppState) should have a method to receive equivocation evidence:
        //
        // let evidence = Evidence {
        //     evidence_type: EvidenceType::DoubleVote,
        //     offender: proof.offender,
        //     height: proof.view,
        //     timestamp: current_timestamp(),
        //     hash_a: proof.hash_a,
        //     hash_b: proof.hash_b,
        //     signature_a: proof.signature_a,
        //     signature_b: proof.signature_b,
        // };
        // self.app.submit_evidence(evidence);
        //
        // For now, we just log. Integration with AppState/StakingState
        // requires passing the evidence through the consensus boundary.
    }

    /// Get equivocation statistics for monitoring
    pub fn equivocation_stats(&self) -> super::EquivocationStats {
        self.equivocation_detector.stats()
    }

    /// Get all detected equivocations (for operator visibility)
    pub fn get_equivocations(&self) -> Vec<EquivocationProof> {
        self.equivocation_detector.get_equivocations()
    }

    /// CRITICAL-7: Check if a voter is rate-limited.
    ///
    /// Returns true if the voter has exceeded MAX_VOTES_PER_VALIDATOR_PER_SECOND
    /// and the vote should be dropped. This prevents vote spam DoS attacks.
    fn is_vote_rate_limited(&mut self, voter: &crate::types::NodeId) -> bool {
        use super::MAX_VOTES_PER_VALIDATOR_PER_SECOND;

        let now = Instant::now();
        let one_second_ago = now - Duration::from_secs(1);

        let timestamps = self.vote_timestamps.entry(*voter).or_default();

        // Remove timestamps older than 1 second
        while timestamps.front().map(|t| *t < one_second_ago).unwrap_or(false) {
            timestamps.pop_front();
        }

        // Check if at limit
        if timestamps.len() >= MAX_VOTES_PER_VALIDATOR_PER_SECOND {
            debug!(
                voter = %hash_short(voter),
                count = timestamps.len(),
                limit = MAX_VOTES_PER_VALIDATOR_PER_SECOND,
                "Vote rate limited"
            );
            return true;
        }

        // Record this vote timestamp
        timestamps.push_back(now);
        false
    }

    /// CRITICAL-7: Prune old votes to prevent unbounded memory growth.
    ///
    /// Called after each commit to remove votes older than VOTE_RETENTION_VIEWS.
    fn prune_old_votes(&mut self, committed_view: crate::types::View) {
        use super::VOTE_RETENTION_VIEWS;

        let min_view = committed_view.saturating_sub(VOTE_RETENTION_VIEWS);

        // Prune vote collections for old views
        self.votes.retain(|_, votes| {
            votes.first().map(|v| v.view >= min_view).unwrap_or(false)
        });
    }

    // =========================================================================
    // Block Sync Protocol
    // =========================================================================

    /// Handle incoming SyncRequest from a peer
    async fn handle_sync_request(&self, from: crate::types::NodeId, req: crate::types::SyncRequest) {
        if let Some(ref handler) = self.sync_handler {
            let response = handler.handle_sync_request(req);
            debug!(
                from = %hash_short(&from),
                blocks = response.blocks.len(),
                "Responding to sync request"
            );
            if let Err(e) = self.network.send_to(from, &Message::SyncResponse(response)).await {
                warn!(error = %e, "Failed to send sync response");
            }
        }
    }

    /// Handle incoming SyncResponse from a peer
    async fn handle_sync_response(&mut self, response: crate::types::SyncResponse) {
        if response.blocks.is_empty() {
            return;
        }

        let block_count = response.blocks.len();
        debug!(
            blocks = block_count,
            peer_height = response.peer_height,
            "Processing sync response"
        );

        // Execute and store each block
        for block in &response.blocks {
            // Save block
            self.store.save(block);
            self.pending.insert(block.hash(), block.clone());

            // Execute block
            let _ = self.app.execute(block);

            // Update committed height if this block extends our chain
            if block.height > self.committed_height {
                self.committed_height = block.height;
                self.committed_hash = block.hash();
                self.store.set_committed(&self.committed_hash);
            }
        }

        // Update sync client height
        self.sync_client.update_height(self.committed_height);

        // Check if we need more blocks
        if self.sync_client.needs_more(&response) {
            // Request more blocks from the first validator that's not us
            for validator in &self.config.validators {
                if *validator != self.config.node_id {
                    let req = self.sync_client.create_sync_request();
                    if let Err(e) = self.network.send_to(*validator, &Message::SyncRequest(req)).await {
                        warn!(error = %e, "Failed to send sync request");
                    }
                    break;
                }
            }
        } else {
            self.syncing = false;
            info!(height = self.committed_height, "Sync complete");
        }
    }

    /// Check if we're behind peers and need to sync
    pub fn detect_sync_needed(&self, peer_height: u64) -> bool {
        // Need sync if peer is more than 10 blocks ahead
        peer_height > self.committed_height + 10
    }

    /// Start syncing from peers
    pub async fn start_sync(&mut self) -> Result<()> {
        if self.syncing {
            return Ok(()); // Already syncing
        }

        self.syncing = true;
        info!(from_height = self.committed_height, "Starting block sync");

        // Request blocks from the first validator that's not us
        for validator in &self.config.validators {
            if *validator != self.config.node_id {
                let req = self.sync_client.create_sync_request();
                self.network.send_to(*validator, &Message::SyncRequest(req)).await?;
                break;
            }
        }

        Ok(())
    }
}
