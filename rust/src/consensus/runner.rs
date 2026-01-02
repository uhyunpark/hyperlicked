//! Async Consensus Runner
//!
//! Orchestrates the consensus engine with network I/O.
//! This is the main entry point for running a validator node.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::{AppHook, BlockStore, MemoryBlockStore, NoOpApp, Pacemaker, Safety};
use crate::network::{Network, TcpNetwork};
use crate::types::{
    hash_short, Block, Certificate, ConsensusConfig, Hash, Message, Prepare, Propose, View, Vote,
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
        store.save(&genesis);
        store.set_committed(&genesis.hash());

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
        })
    }

    /// Run the consensus loop
    pub async fn run(&mut self) -> Result<()> {
        info!(
            node = %hash_short(&self.config.node_id),
            "Starting consensus runner"
        );

        loop {
            let view = self.pacemaker.current_view();
            let is_leader = self.config.is_leader(view);

            if is_leader {
                self.run_leader_round(view).await?;
            } else {
                self.run_follower_round(view).await?;
            }

            // Small delay to prevent tight loop
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Run one round as leader
    async fn run_leader_round(&mut self, view: View) -> Result<()> {
        info!(view, "Running as LEADER");

        // 1. Get parent block
        let parent = self.get_proposal_parent();
        let parent_hash = parent.hash();

        // 2. Prepare payload
        let payload = self.app.prepare_payload(&parent);

        // 3. Create block
        let mut block = Block {
            view,
            height: parent.height + 1,
            parent: parent_hash,
            payload,
            proposer: self.config.node_id,
            app_hash: [0u8; 32],
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        };

        // 4. Execute to get app_hash
        let app_hash = self.app.execute(&block);
        block.app_hash = app_hash;

        let block_hash = block.hash();
        info!(
            view,
            height = block.height,
            hash = %hash_short(&block_hash),
            "Proposing block"
        );

        // 5. Store block
        self.store.save(&block);
        self.pending.insert(block_hash, block.clone());

        // 6. Broadcast proposal
        let propose = Propose {
            block: block.clone(),
            justify: self.safety.high_qc().cloned(),
        };
        self.network.broadcast_propose(propose).await?;

        // 7. Self-vote
        let self_vote = Vote::new(view, block_hash, app_hash, self.config.node_id);
        self.votes
            .entry(block_hash)
            .or_default()
            .push(self_vote);

        // 8. Collect votes from others
        let quorum = self.config.quorum();
        let collect_timeout = Duration::from_secs(3);

        let votes = self.collect_votes(block_hash, quorum, collect_timeout).await;

        if votes.len() >= quorum {
            info!(
                view,
                votes = votes.len(),
                "Collected quorum, forming QC"
            );

            // 9. Form QC
            let qc = Certificate::new(view, block_hash, votes);

            // 10. Broadcast prepare
            let prepare = Prepare { view, qc: qc.clone() };
            self.network.broadcast_prepare(prepare).await?;

            // 11. Process QC (may commit)
            self.process_qc(qc);

            // 12. Advance view
            if let Some(ref high_qc) = self.safety.high_qc() {
                self.pacemaker.advance_view(high_qc);
            }
        } else {
            warn!(
                view,
                votes = votes.len(),
                needed = quorum,
                "Failed to collect quorum"
            );
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
                self.pacemaker.record_timeout();
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
                    } else {
                        debug!(
                            got = propose.block.view,
                            expected = target_view,
                            "Wrong view proposal"
                        );
                    }
                }
                Message::Vote(vote) => {
                    // Store vote (might be leader collecting)
                    self.votes
                        .entry(vote.block_hash)
                        .or_default()
                        .push(vote);
                }
                Message::Prepare(prepare) => {
                    // Store for later processing
                    if prepare.view >= target_view {
                        self.process_prepare(prepare);
                    }
                }
            }
        }
    }

    /// Wait for a prepare for the given view
    async fn wait_for_prepare(&mut self, target_view: View) -> Result<Prepare> {
        loop {
            let (from, msg) = self.network.recv_msg().await?;

            match msg {
                Message::Prepare(prepare) => {
                    if prepare.view >= target_view {
                        return Ok(prepare);
                    }
                }
                Message::Vote(vote) => {
                    self.votes
                        .entry(vote.block_hash)
                        .or_default()
                        .push(vote);
                }
                Message::Propose(_) => {
                    // Ignore proposals while waiting for prepare
                }
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
            // Check if we have quorum
            let current_votes = self.votes.get(&block_hash).map(|v| v.len()).unwrap_or(0);
            if current_votes >= quorum {
                return self.votes.remove(&block_hash).unwrap_or_default();
            }

            // Check timeout
            if tokio::time::Instant::now() >= deadline {
                return self.votes.remove(&block_hash).unwrap_or_default();
            }

            // Wait for next message
            let remaining = deadline - tokio::time::Instant::now();
            match timeout(remaining, self.network.recv_msg()).await {
                Ok(Ok((from, Message::Vote(vote)))) => {
                    if vote.block_hash == block_hash {
                        debug!(
                            from = %hash_short(&from),
                            view = vote.view,
                            "Received vote"
                        );
                        self.votes.entry(block_hash).or_default().push(vote);
                    }
                }
                Ok(Ok((_, msg))) => {
                    // Handle other messages
                    debug!(?msg, "Received non-vote while collecting");
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "Error receiving message");
                }
                Err(_) => {
                    // Timeout
                    break;
                }
            }
        }

        self.votes.remove(&block_hash).unwrap_or_default()
    }

    /// Process a proposal
    fn process_proposal(&mut self, propose: Propose) -> Option<Vote> {
        let block = &propose.block;
        let view = block.view;

        debug!(
            view,
            height = block.height,
            hash = %hash_short(&block.hash()),
            "Processing proposal"
        );

        // Execute block
        let local_app_hash = self.app.execute(block);

        // Check safety
        if let Err(e) = self.safety.safe_to_vote(block, local_app_hash) {
            warn!(view, error = %e, "Unsafe to vote");
            return None;
        }

        // Record vote and store block
        self.safety.record_vote(view);
        self.store.save(block);
        self.pending.insert(block.hash(), block.clone());

        // Update high_qc if proposal includes one
        if let Some(justify) = propose.justify {
            self.safety.update_high_qc(justify);
        }

        // Create vote
        Some(Vote::new(
            view,
            block.hash(),
            local_app_hash,
            self.config.node_id,
        ))
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

        // Try to commit (simplified: commit the certified block)
        self.try_commit(&qc.block_hash);
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

        // Prune
        self.pending.retain(|_, b| b.height > self.committed_height);
        self.safety.prune_votes_below(block.view);

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
}
