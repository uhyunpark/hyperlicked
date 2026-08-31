//! Async Consensus Runner
//!
//! Orchestrates the consensus engine with network I/O.
//! This is the main entry point for running a validator node.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use super::message_handler::store_vote_with_context;
use super::timeout::create_signed_timeout_with_context;
use super::view_change::{
    validate_view_change_certificate_with_committee_and_context,
    validate_view_change_with_committee_and_context, MAX_FUTURE_VIEWS,
};
use super::{
    form_certificate, verify_certificate, verify_vote, AppHook, BlockStore, EquivocationDetector,
    EquivocationProof, MemoryBlockStore, NoOpApp, Pacemaker, Safety, TimeoutCollector,
};
use crate::app::{ConsensusTransaction, SignedEnvelope, Transaction};
use crate::config::Config;
use crate::consensus::EpochTransitionProof;
use crate::crypto::bls::BlsPublicKey;
use crate::network::{Network, SyncClient, SyncHandler, TcpNetwork};
use crate::storage::{ConsensusState, PersistentStore};
use crate::types::{
    hash_short, Block, Certificate, Committee, ConsensusConfig, ConsensusContext, Hash, Message,
    NewView, NodeId, Prepare, Propose, TimeoutCertificate, View, ViewChange, ViewChangeCertificate,
    Vote,
};

/// Fixed hard upper bound for in-memory speculative block headers/payloads. A
/// verified-QC proposal may use the final reserved entry; ordinary proposals
/// use [`MAX_PENDING_SOFT_BLOCKS`] so delayed certified continuations retain a
/// slot.
pub(crate) const MAX_PENDING_BLOCKS: usize = crate::consensus::MAX_SPECULATIVE_STORE_BLOCKS;
pub(crate) const MAX_PENDING_SOFT_BLOCKS: usize =
    crate::consensus::MAX_SPECULATIVE_STORE_SOFT_BLOCKS;

/// Fixed hard on-disk/in-memory speculative block-byte budget. This bounds the
/// serialized payload journal as well as its entry count; ordinary proposals
/// use [`MAX_PENDING_SOFT_BYTES`].
pub(crate) const MAX_PENDING_BYTES: usize = crate::consensus::MAX_SPECULATIVE_STORE_BYTES;
pub(crate) const MAX_PENDING_SOFT_BYTES: usize = crate::consensus::MAX_SPECULATIVE_STORE_SOFT_BYTES;

/// Fixed maximum distance from the committed head for pending blocks. The
/// replay closure is limited to 15 ancestors, leaving the application's 16th
/// candidate slot for the block being preflighted.
pub(crate) const MAX_PENDING_DEPTH: u64 = 16;

/// Async consensus runner
pub struct ConsensusRunner {
    /// Configuration
    config: ConsensusConfig,

    /// Static epoch-0 consensus authentication context.
    context: ConsensusContext,

    /// Safety module (voting rules)
    safety: Safety,

    /// Pacemaker (view advancement)
    pacemaker: Pacemaker,

    /// Application hook
    app: Box<dyn AppHook>,

    /// Live consensus must explicitly attach its application hook. Keeping a
    /// constructor fallback is useful for assembling the runner, but it must
    /// never silently become the state machine used by `run`.
    app_attached: bool,

    /// Recovery reconciliation is performed once after the application
    /// handshake.  The first round consumes this marker so normal runs do not
    /// perform the same GC twice before processing network input.
    reconciled_after_recovery: bool,

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

    /// Rotating cursor for bounded durable-evidence rebroadcasts.
    equivocation_broadcast_cursor: usize,

    /// CRITICAL-7: Vote timestamps per validator for rate limiting
    vote_timestamps: HashMap<crate::types::NodeId, VecDeque<Instant>>,
}

fn should_emit_view_change_after_timeout_certificate(formed: bool) -> bool {
    !formed
}

fn should_rebroadcast_equivocation_batch(gossip_enabled: bool) -> bool {
    gossip_enabled
}

fn is_view_in_bounded_window(current_view: View, message_view: View) -> bool {
    message_view >= current_view && message_view <= current_view.saturating_add(MAX_FUTURE_VIEWS)
}

fn recovery_resume_view(state: &ConsensusState) -> View {
    state
        .high_qc
        .as_ref()
        .map(|qc| qc.view.saturating_add(1))
        .unwrap_or(state.current_view)
        .max(state.current_view)
}

fn verify_high_qc_against_block(
    committee: &Committee,
    context: ConsensusContext,
    qc: &Certificate,
    certified_block: Option<&Block>,
    require_bls_signature: bool,
) -> Result<(), String> {
    let block = certified_block
        .ok_or_else(|| "high QC certified block is not available locally".to_string())?;
    block.validate_context(context)?;
    qc.validate_context(context)?;
    let block_hash = block.hash();
    if block_hash != qc.block_hash {
        return Err("high QC block hash does not match the local certified block".to_string());
    }
    if block.view != qc.view {
        return Err(format!(
            "high QC view {} does not match local certified block view {}",
            qc.view, block.view
        ));
    }

    verify_certificate(
        committee,
        qc,
        context,
        block.view,
        &block_hash,
        Some(&block.app_hash),
        require_bls_signature,
    )
}

fn proof_from_committed_evidence(
    evidence: &crate::app::staking::Evidence,
) -> Result<Option<EquivocationProof>, String> {
    if evidence.evidence_type != crate::app::staking::EvidenceType::DoubleVote {
        return Ok(None);
    }
    if evidence.timestamp != 0 {
        return Err("committed equivocation evidence has a nonzero timestamp".to_string());
    }
    let proof = EquivocationProof {
        context: evidence.context,
        offender: evidence.offender,
        view: evidence.view,
        hash_a: evidence.hash_a,
        app_hash_a: evidence.app_hash_a,
        hash_b: evidence.hash_b,
        app_hash_b: evidence.app_hash_b,
        signature_a: evidence.signature_a.clone(),
        signature_b: evidence.signature_b.clone(),
    };
    proof.validate_canonical()?;
    Ok(Some(proof))
}

fn committed_equivocation_proofs(block: &Block) -> Result<Vec<EquivocationProof>, String> {
    let entries = crate::app::AppState::decode_consensus_payload(&block.payload)?;
    let mut proofs = Vec::new();
    for entry in entries {
        let ConsensusTransaction::System(Transaction::SubmitEvidence {
            submitter,
            evidence,
        }) = entry
        else {
            continue;
        };
        let expected_submitter = format!("system:equivocation:{}", hex::encode(evidence.offender));
        if submitter != expected_submitter {
            return Err(
                "committed equivocation evidence has an invalid system submitter".to_string(),
            );
        }
        if let Some(proof) = proof_from_committed_evidence(&evidence)? {
            proofs.push(proof);
        }
    }
    Ok(proofs)
}

/// Validate the complete finalized chain and every persisted QC reference
/// before exposing any recovered consensus state to the runner.
///
/// App state is deliberately not reconstructed here: `AppSnapshot` omits
/// orderbook state, so the canonical node replays these blocks through its
/// application hook before starting consensus.  This function only proves
/// that the durable consensus chain and its authenticated references are
/// complete and internally consistent.
fn validate_recovered_chain<S: PersistentStore + ?Sized>(
    store: &S,
    context: ConsensusContext,
    committee: &Committee,
    state: &ConsensusState,
    require_bls_signature: bool,
) -> Result<Vec<Block>> {
    if state.committed_hash == [0u8; 32] && state.committed_height != 0 {
        return Err(anyhow::anyhow!(
            "persisted committed height has an empty committed hash"
        ));
    }

    let mut blocks: Vec<_> = store
        .blocks_from_height(0)?
        .into_iter()
        .filter(|block| block.height <= state.committed_height)
        .collect();
    blocks.sort_by_key(|block| block.height);

    let expected_count = state
        .committed_height
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("persisted committed height overflows"))?;
    let expected_count_usize = usize::try_from(expected_count)
        .map_err(|_| anyhow::anyhow!("persisted committed height does not fit platform usize"))?;
    if blocks.len() != expected_count_usize {
        return Err(anyhow::anyhow!(
            "persisted finalized chain is incomplete: expected {} blocks, found {}",
            expected_count,
            blocks.len()
        ));
    }

    let committed_head = store
        .get_committed_head()
        .ok_or_else(|| anyhow::anyhow!("persisted committed metadata has no block head"))?;
    let committed_height_meta = store
        .load_committed_height()?
        .ok_or_else(|| anyhow::anyhow!("persisted committed height metadata is missing"))?;
    if committed_height_meta != state.committed_height {
        return Err(anyhow::anyhow!(
            "persisted committed height metadata does not match consensus state"
        ));
    }
    if committed_head.height != state.committed_height
        || committed_head.hash() != state.committed_hash
    {
        return Err(anyhow::anyhow!(
            "persisted committed metadata does not match the committed head"
        ));
    }

    let genesis = Block::genesis(context);
    let mut by_hash = HashMap::with_capacity(blocks.len());
    let mut previous = genesis.clone();
    for (index, block) in blocks.iter().enumerate() {
        block.validate().map_err(|error| {
            anyhow::anyhow!("persisted block {} is invalid: {error}", block.height)
        })?;
        block.validate_context(context).map_err(|error| {
            anyhow::anyhow!(
                "persisted block {} has the wrong context: {error}",
                block.height
            )
        })?;

        if index == 0 {
            if block.height != 0 || block.hash() != genesis.hash() || block.parent != genesis.parent
            {
                return Err(anyhow::anyhow!(
                    "persisted chain does not start with the canonical genesis block"
                ));
            }
        } else {
            if block.height != index as u64 {
                return Err(anyhow::anyhow!(
                    "persisted chain height {} is not sequential at index {}",
                    block.height,
                    index
                ));
            }
            if block.parent != previous.hash() {
                return Err(anyhow::anyhow!(
                    "persisted block {} has a broken parent link",
                    block.height
                ));
            }
            block
                .validate_parent_timestamp(previous.timestamp)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "persisted block {} has an invalid timestamp: {error}",
                        block.height
                    )
                })?;
            if block.height == 1 {
                if block.parent != genesis.hash() || block.justify.is_some() {
                    return Err(anyhow::anyhow!(
                        "persisted height-one block must extend genesis without a QC"
                    ));
                }
            } else {
                let justify = block.justify.as_ref().ok_or_else(|| {
                    anyhow::anyhow!(
                        "persisted non-genesis block {} is missing its QC",
                        block.height
                    )
                })?;
                if justify.block_hash != block.parent {
                    return Err(anyhow::anyhow!(
                        "persisted block {} QC does not certify its parent",
                        block.height
                    ));
                }
                let parent = &blocks[index - 1];
                if justify.view != parent.view {
                    return Err(anyhow::anyhow!(
                        "persisted block {} QC view does not match its parent",
                        block.height
                    ));
                }
                verify_certificate(
                    committee,
                    justify,
                    context,
                    parent.view,
                    &block.parent,
                    Some(&parent.app_hash),
                    require_bls_signature,
                )
                .map_err(|error| {
                    anyhow::anyhow!(
                        "persisted block {} has an invalid parent QC: {error}",
                        block.height
                    )
                })?;
            }
        }

        by_hash.insert(block.hash(), block.clone());
        previous = block.clone();
    }

    if previous.height != state.committed_height || previous.hash() != state.committed_hash {
        return Err(anyhow::anyhow!(
            "persisted committed metadata does not match the finalized chain head"
        ));
    }

    let validate_qc_reference = |name: &str, qc: &Certificate| -> Result<()> {
        qc.validate_context(context)
            .map_err(|error| anyhow::anyhow!("persisted {name} has the wrong context: {error}"))?;
        let block = by_hash
            .get(&qc.block_hash)
            .cloned()
            .or_else(|| store.get(&qc.block_hash))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "persisted {name} references missing block {}",
                    hash_short(&qc.block_hash)
                )
            })?;
        block.validate_context(context).map_err(|error| {
            anyhow::anyhow!("persisted {name} target block has the wrong context: {error}")
        })?;
        block.validate().map_err(|error| {
            anyhow::anyhow!("persisted {name} target block is invalid: {error}")
        })?;
        if block.height <= state.committed_height {
            let canonical = by_hash.get(&qc.block_hash).ok_or_else(|| {
                anyhow::anyhow!(
                    "persisted {name} targets a non-canonical committed block at height {}",
                    block.height
                )
            })?;
            if canonical.hash() != block.hash() {
                return Err(anyhow::anyhow!(
                    "persisted {name} conflicts with the committed chain"
                ));
            }
        } else {
            let mut cursor = block.clone();
            loop {
                if cursor.parent == state.committed_hash {
                    if cursor.height != state.committed_height.saturating_add(1) {
                        return Err(anyhow::anyhow!(
                            "persisted {name} skips the committed chain height"
                        ));
                    }
                    break;
                }
                let parent = by_hash
                    .get(&cursor.parent)
                    .cloned()
                    .or_else(|| store.get(&cursor.parent))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "persisted {name} target is not connected to the committed head"
                        )
                    })?;
                if parent.height <= state.committed_height {
                    return Err(anyhow::anyhow!(
                        "persisted {name} target is not connected to the committed head"
                    ));
                }
                if cursor.height != parent.height.saturating_add(1) {
                    return Err(anyhow::anyhow!(
                        "persisted {name} target has a broken parent link"
                    ));
                }
                cursor = parent;
            }
        }
        verify_certificate(
            committee,
            qc,
            context,
            block.view,
            &block.hash(),
            Some(&block.app_hash),
            require_bls_signature,
        )
        .map_err(|error| anyhow::anyhow!("persisted {name} is invalid: {error}"))
    };

    if let Some(qc) = &state.high_qc {
        validate_qc_reference("high QC", qc)?;
    }
    if let Some(qc) = &state.locked_qc {
        validate_qc_reference("locked QC", qc)?;
    }

    let mut voted_views = state.voted_views.clone();
    voted_views.sort_unstable();
    if voted_views.windows(2).any(|views| views[0] == views[1]) {
        return Err(anyhow::anyhow!("persisted voted_views contains duplicates"));
    }
    if voted_views.iter().any(|view| *view > state.current_view) {
        return Err(anyhow::anyhow!(
            "persisted voted_views contains a future view"
        ));
    }

    Ok(blocks)
}

impl ConsensusRunner {
    /// Admit an inbound signed user transaction through the canonical
    /// application path.  Only an envelope accepted by the application is
    /// handed back to transport for relay; nonce/mempool rejection therefore
    /// cannot poison the transport's stable seen cache.
    async fn handle_user_transaction(&mut self, from: NodeId, envelope: SignedEnvelope) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_millis() as u64;
        match self
            .app
            .submit_user_transaction(envelope.clone(), timestamp)
        {
            Ok(hash) => {
                if let Err(error) = self
                    .network
                    .broadcast(&Message::UserTransaction(envelope))
                    .await
                {
                    warn!(
                        from = %hash_short(&from),
                        tx_hash = %hex::encode(hash),
                        error = %error,
                        "admitted user transaction could not be relayed"
                    );
                }
            }
            Err(error) => {
                debug!(
                    from = %hash_short(&from),
                    error,
                    "rejected inbound user transaction at canonical admission"
                );
            }
        }
    }

    fn known_block(&self, hash: &Hash) -> Option<Block> {
        self.pending
            .get(hash)
            .cloned()
            .or_else(|| self.store.get(hash))
            .or_else(|| {
                self.persistent_store
                    .as_ref()
                    .and_then(|store| store.get(hash))
            })
    }

    /// Load a bounded, contiguous branch from the durable/local block stores.
    /// The caller may replay it privately before deciding whether a live
    /// candidate branch can be pruned.
    fn load_speculative_branch(&self, target_hash: Hash) -> Result<(Block, Vec<Block>), String> {
        let committed_head = self
            .known_block(&self.committed_hash)
            .or_else(|| (self.committed_height == 0).then(|| Block::genesis(self.context)))
            .ok_or_else(|| "committed application head is unavailable".to_string())?;
        if target_hash == self.committed_hash {
            return Ok((committed_head, Vec::new()));
        }

        let mut reverse = Vec::new();
        let mut current_hash = target_hash;
        let mut visited = HashSet::new();
        while current_hash != self.committed_hash {
            if reverse.len() >= MAX_PENDING_DEPTH as usize {
                return Err(format!(
                    "speculative replay exceeds depth bound {}",
                    MAX_PENDING_DEPTH
                ));
            }
            if !visited.insert(current_hash) {
                return Err("speculative replay contains a cyclic parent link".to_string());
            }
            let block = self
                .known_block(&current_hash)
                .ok_or_else(|| "speculative replay parent is unavailable".to_string())?;
            block.validate_context(self.context)?;
            if block.height <= self.committed_height {
                return Err("speculative replay crosses below committed height".to_string());
            }
            reverse.push(block.clone());
            current_hash = block.parent;
        }
        reverse.reverse();
        let mut expected_height = committed_head.height.saturating_add(1);
        let mut parent_hash = committed_head.hash();
        for block in &reverse {
            if block.height != expected_height || block.parent != parent_hash {
                return Err("speculative replay has a broken parent closure".to_string());
            }
            parent_hash = block.hash();
            expected_height = expected_height.saturating_add(1);
        }
        Ok((committed_head, reverse))
    }

    fn preflight_application_branch(&self, block: &Block) -> Result<(Block, Vec<Block>), String> {
        let (committed_head, ancestors) = self.load_speculative_branch(block.parent)?;
        self.app.preflight_block_with_speculative_branch(
            self.context,
            block,
            &committed_head,
            &ancestors,
        )?;
        Ok((committed_head, ancestors))
    }

    fn restore_application_branch_for_admission(
        &mut self,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        reserve_slots: usize,
    ) -> Result<(), String> {
        self.app.restore_speculative_branch_for_admission(
            self.context,
            committed_head,
            ancestors,
            protected_roots,
            reserve_slots,
        )
    }

    fn check_application_branch_admission(
        &self,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        reserve_slots: usize,
    ) -> Result<(), String> {
        self.app.check_speculative_branch_admission(
            self.context,
            committed_head,
            ancestors,
            protected_roots,
            reserve_slots,
        )
    }

    fn prune_speculative_stores(&self, extra_root: Option<Hash>) -> Result<(), String> {
        self.prune_speculative_stores_with_limits(extra_root, MAX_PENDING_BLOCKS, MAX_PENDING_BYTES)
    }

    fn prune_speculative_stores_with_limits(
        &self,
        extra_root: Option<Hash>,
        max_blocks: usize,
        max_bytes: usize,
    ) -> Result<(), String> {
        let roots = self.protected_speculative_roots(extra_root);
        if let Some(store) = self.persistent_store.as_ref() {
            store
                .prune_speculative(&roots, max_blocks, max_bytes)
                .map_err(|error| format!("persistent speculative prune failed: {error}"))?;
        }
        self.store
            .prune_speculative(&roots, max_blocks, max_bytes)
            .map_err(|error| format!("in-memory speculative prune failed: {error}"))?;
        Ok(())
    }

    #[cfg(test)]
    fn ensure_speculative_store_capacity_with_limits(
        &self,
        block: &Block,
        max_blocks: usize,
        max_bytes: usize,
    ) -> Result<(), String> {
        self.store
            .ensure_speculative_capacity(block, max_blocks, max_bytes)
            .map_err(|error| format!("in-memory speculative capacity rejected: {error}"))?;
        if let Some(store) = self.persistent_store.as_ref() {
            store
                .ensure_speculative_capacity(block, max_blocks, max_bytes)
                .map_err(|error| format!("persistent speculative capacity rejected: {error}"))?;
        }
        Ok(())
    }

    fn speculative_store_admission_limits(block: &Block) -> (usize, usize) {
        if block.justify.is_some() {
            (MAX_PENDING_BLOCKS, MAX_PENDING_BYTES)
        } else {
            (MAX_PENDING_SOFT_BLOCKS, MAX_PENDING_SOFT_BYTES)
        }
    }

    #[cfg(test)]
    fn ensure_speculative_store_admission_capacity(&self, block: &Block) -> Result<(), String> {
        let (max_blocks, max_bytes) = Self::speculative_store_admission_limits(block);
        self.ensure_speculative_store_capacity_with_limits(block, max_blocks, max_bytes)
    }

    /// Admit the validated proposal as one rolling transition per store.  The
    /// durable journal is written first; the production cache follows only
    /// after RocksDB has acknowledged the synced batch.  If the cache cannot
    /// mirror a durable admission, fail-stop rather than voting with a cache
    /// that could resurrect an evicted branch.
    fn admit_speculative_stores(
        &self,
        block: &Block,
        protected_roots: &[Hash],
    ) -> Result<(), String> {
        let (max_blocks, max_bytes) = Self::speculative_store_admission_limits(block);
        if let Some(store) = self.persistent_store.as_ref() {
            store
                .admit_speculative_with_rolling_victim(
                    block,
                    protected_roots,
                    max_blocks,
                    max_bytes,
                )
                .map_err(|error| format!("persistent speculative admission rejected: {error}"))?;
        }
        self.store
            .admit_speculative_with_rolling_victim(
                block,
                protected_roots,
                max_blocks,
                max_bytes,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "CRITICAL: in-memory speculative cache could not mirror durable admission: {error}"
                )
            });
        Ok(())
    }

    /// Return whether a block is connected to the exact committed head.
    ///
    /// Heights alone are not a sufficient chain anchor: a cryptographically
    /// valid QC can still certify a branch whose parent at the next height is
    /// different from the locally finalized block.  Walk the locally known
    /// parent links and fail closed on missing, cyclic, or non-sequential
    /// ancestry.
    fn is_chain_connected_to_committed_head(&self, block_hash: &Hash) -> bool {
        let mut current_hash = *block_hash;
        let mut visited = HashSet::new();

        loop {
            if current_hash == self.committed_hash {
                return true;
            }
            if !visited.insert(current_hash) {
                return false;
            }

            let block = self.known_block(&current_hash);
            let Some(block) = block else {
                return false;
            };
            if block.height <= self.committed_height {
                return false;
            }

            if block.parent == self.committed_hash {
                return block.height == self.committed_height.saturating_add(1);
            }

            let parent = self.known_block(&block.parent);
            let Some(parent) = parent else {
                return false;
            };
            if block.height != parent.height.saturating_add(1) {
                return false;
            }
            current_hash = block.parent;
        }
    }

    fn ensure_pending_capacity(&self, block: &Block) -> Result<(), String> {
        if block.height > self.committed_height.saturating_add(MAX_PENDING_DEPTH) {
            return Err(format!(
                "pending block height {} exceeds committed height {} by more than {}",
                block.height, self.committed_height, MAX_PENDING_DEPTH
            ));
        }
        if self.pending.contains_key(&block.hash()) {
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING_BLOCKS {
            return Err(format!(
                "pending block limit {} reached; refusing an unprotected branch",
                MAX_PENDING_BLOCKS
            ));
        }
        Ok(())
    }

    fn insert_pending(&mut self, block: Block) -> Result<(), String> {
        self.ensure_pending_capacity(&block)?;
        self.pending.insert(block.hash(), block);
        Ok(())
    }

    /// Retain only complete ancestor closures for the current consensus roots.
    /// A root can be a high/locked QC target or the parent selected for the
    /// next proposal. No branch is evicted by age: if all retained entries are
    /// protected, admission fails closed at the applicable pending cap.
    fn protected_speculative_roots(&self, extra_root: Option<Hash>) -> Vec<Hash> {
        let mut roots = self.protected_speculative_roots_for_safety(&self.safety, extra_root);
        roots.push(self.get_proposal_parent().hash());
        roots.sort_unstable();
        roots.dedup();
        roots
    }

    fn protected_speculative_roots_for_safety(
        &self,
        safety: &super::Safety,
        extra_root: Option<Hash>,
    ) -> Vec<Hash> {
        let mut roots = HashSet::new();
        if let Some(qc) = safety.high_qc() {
            roots.insert(qc.block_hash);
        }
        if let Some(qc) = safety.locked_qc() {
            roots.insert(qc.block_hash);
        }
        if let Some(root) = extra_root {
            roots.insert(root);
        }
        roots.into_iter().collect()
    }

    fn prune_pending_unprotected_branches(&mut self, extra_root: Option<Hash>) {
        let roots = self.protected_speculative_roots(extra_root);
        self.prune_pending_unprotected_branches_with_roots(&roots);
    }

    fn protected_pending_hashes(&self, roots: &[Hash]) -> HashSet<Hash> {
        let mut protected = HashSet::new();
        for root in roots {
            let mut current = *root;
            let mut visited = HashSet::new();
            loop {
                if !visited.insert(current) {
                    break;
                }
                let block = self.known_block(&current);
                let Some(block) = block else {
                    break;
                };
                if block.height <= self.committed_height {
                    break;
                }
                if self.pending.contains_key(&current) {
                    protected.insert(current);
                }
                current = block.parent;
            }
        }
        protected
    }

    fn prune_pending_unprotected_branches_with_roots(&mut self, roots: &[Hash]) {
        let protected = self.protected_pending_hashes(roots);
        self.pending
            .retain(|candidate_hash, _| protected.contains(candidate_hash));
    }

    fn pending_admission_available(&self, block: &Block, roots: &[Hash]) -> bool {
        if block.height > self.committed_height.saturating_add(MAX_PENDING_DEPTH) {
            return false;
        }
        let max_blocks = Self::pending_admission_limit(block);
        if self.pending.contains_key(&block.hash()) || self.pending.len() < max_blocks {
            return true;
        }
        self.protected_pending_hashes(roots).len() < max_blocks
    }

    fn pending_admission_limit(block: &Block) -> usize {
        if block.justify.is_some() {
            MAX_PENDING_BLOCKS
        } else {
            MAX_PENDING_SOFT_BLOCKS
        }
    }

    fn connected_high_qc(&self) -> Option<Certificate> {
        let qc = self.safety.high_qc()?.clone();
        let block = self.known_block(&qc.block_hash)?;
        if block.hash() == qc.block_hash && self.is_chain_connected_to_committed_head(&block.hash())
        {
            Some(qc)
        } else {
            None
        }
    }

    fn validate_node_identity(config: &ConsensusConfig, network: &TcpNetwork) -> Result<()> {
        if config.node_id != network.node_id() {
            return Err(anyhow::anyhow!(
                "consensus node ID does not match network node ID"
            ));
        }
        Ok(())
    }

    fn view_timeout(config: &ConsensusConfig) -> Result<Duration> {
        if config.view_timeout_ms == 0 {
            return Err(anyhow::anyhow!(
                "consensus view timeout must be greater than zero"
            ));
        }
        Ok(Duration::from_millis(config.view_timeout_ms))
    }

    fn validate_authenticated_transport(network: &TcpNetwork) -> Result<()> {
        if !network.requires_authenticated_peers() {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires authenticated TCP transport"
            ));
        }
        Ok(())
    }

    fn validate_network_config(config: &ConsensusConfig) -> Result<Committee> {
        let committee = config
            .committee()
            .map_err(|error| anyhow::anyhow!("invalid active committee: {error}"))?;
        if !committee.bls_enabled() {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires a BLS key for every committee member"
            ));
        }

        for member in committee.members() {
            let bytes = member
                .bls_pubkey
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("committee member is missing a BLS key"))?;
            if bytes.len() != 48 {
                return Err(anyhow::anyhow!(
                    "committee member {} has an invalid BLS key length",
                    hash_short(&member.node_id)
                ));
            }
            let mut key_bytes = [0u8; 48];
            key_bytes.copy_from_slice(bytes);
            BlsPublicKey::from_bytes(&key_bytes).map_err(|_| {
                anyhow::anyhow!(
                    "committee member {} has an invalid BLS public key",
                    hash_short(&member.node_id)
                )
            })?;
        }

        let secret_key = config.bls_secret_key().ok_or_else(|| {
            anyhow::anyhow!("live ConsensusRunner requires a local BLS secret key")
        })?;
        let configured_key = committee
            .bls_pubkey(&config.node_id)
            .ok_or_else(|| anyhow::anyhow!("local node is not in the active committee"))?;
        let local_key = secret_key.public_key().to_bytes();
        if configured_key != local_key.as_slice() {
            return Err(anyhow::anyhow!(
                "local BLS secret key does not match the configured committee key"
            ));
        }

        Ok(committee)
    }

    fn verify_high_qc_locally(
        &self,
        committee: &Committee,
        qc: &Certificate,
    ) -> Result<(), String> {
        if !self.is_chain_connected_to_committed_head(&qc.block_hash) {
            return Err("high QC target is not connected to the committed head".to_string());
        }
        let certified_block = self.known_block(&qc.block_hash);
        verify_high_qc_against_block(
            committee,
            self.context,
            qc,
            certified_block.as_ref(),
            self.config.bls_configured(),
        )
    }

    fn verify_vcc_high_qcs_locally(
        &self,
        committee: &Committee,
        vcc: &ViewChangeCertificate,
    ) -> Result<(), String> {
        vcc.validate_context(self.context)?;
        for view_change in &vcc.view_changes {
            view_change.validate_context(self.context)?;
            if let Some(qc) = &view_change.high_qc {
                self.verify_high_qc_locally(committee, qc)?;
            }
        }
        Ok(())
    }

    /// Create a non-persistent consensus runner for tests and local protocol
    /// fixtures only.
    ///
    /// A live validator must use [`Self::new_with_recovery`] with a persistent
    /// store.  This constructor intentionally has no durable consensus state
    /// or equivocation journal and must not be used for a production node.
    pub async fn new(config: ConsensusConfig, network: TcpNetwork) -> Result<Self> {
        Self::validate_node_identity(&config, &network)?;
        Self::validate_authenticated_transport(&network)?;
        let view_timeout = Self::view_timeout(&config)?;
        Self::validate_network_config(&config)?;
        let context = config
            .context()
            .map_err(|error| anyhow::anyhow!("invalid consensus context: {error}"))?;
        if !context.has_genesis_domain() {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires a nonzero validated genesis domain"
            ));
        }

        // Initialize with genesis block
        let store = Box::new(MemoryBlockStore::new());
        let genesis = Block::genesis(context);
        let genesis_hash = genesis.hash();
        store.save(&genesis);
        store.set_committed(&genesis_hash);

        // Create the committee-bound timeout collector.
        let timeout_collector = Self::create_timeout_collector(&config);
        let mut pacemaker = Pacemaker::new(view_timeout);
        pacemaker
            .set_context(context)
            .map_err(|error| anyhow::anyhow!("cannot bind pacemaker context: {error}"))?;

        Ok(Self {
            config,
            context,
            safety: Safety::new_with_context(context),
            pacemaker,
            app: Box::new(NoOpApp),
            app_attached: false,
            reconciled_after_recovery: false,
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
            equivocation_detector: EquivocationDetector::new_with_context(context),
            equivocation_broadcast_cursor: 0,
            vote_timestamps: HashMap::new(),
        })
    }

    /// Create the committee-bound timeout collector.
    fn create_timeout_collector(config: &ConsensusConfig) -> Option<TimeoutCollector> {
        let committee = config.committee().ok()?;
        let context = config.context().ok()?;
        TimeoutCollector::with_committee_and_context(committee, context).ok()
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
        let journal_capability = persistent_store.equivocation_journal_capability();
        if !journal_capability.supports_all() {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires a persistent equivocation proof journal with load/save/delete support (load={}, save={}, delete={})",
                journal_capability.load,
                journal_capability.save,
                journal_capability.delete,
            ));
        }
        // A staged transition marker is intentionally not auto-activated on
        // restart. Until app state, safety, pacemaker, and network admission
        // can be swapped atomically, resuming old-context consensus beside a
        // durable next-committee candidate would be ambiguous. Operators or
        // a future activation path must resolve the marker explicitly.
        if persistent_store.load_epoch_transition_proof()?.is_some() {
            return Err(anyhow::anyhow!(
                "persistent store contains a staged epoch transition; runtime activation is not enabled"
            ));
        }
        Self::validate_node_identity(&config, &network)?;
        Self::validate_authenticated_transport(&network)?;
        let view_timeout = Self::view_timeout(&config)?;
        let committee = Self::validate_network_config(&config)?;
        let context = config
            .context()
            .map_err(|error| anyhow::anyhow!("invalid consensus context: {error}"))?;
        if !context.has_genesis_domain() {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires a nonzero validated genesis domain"
            ));
        }

        // Try to load prior consensus state.  A finalized chain is always
        // reconstructed from genesis and committed blocks; snapshots are not
        // sufficient because they intentionally omit orderbook state.
        let recovered_state = persistent_store.load_consensus_state()?;
        let genesis = Block::genesis(context);
        let (state, committed_blocks) = if let Some(state) = recovered_state {
            if state.context() != context {
                return Err(anyhow::anyhow!(
                    "refusing recovery with mismatched consensus context: expected epoch {} / committee {} / genesis {}, got epoch {} / committee {} / genesis {}",
                    context.epoch,
                    hex::encode(context.committee_hash),
                    hex::encode(context.genesis_hash),
                    state.epoch,
                    hex::encode(state.committee_hash),
                    hex::encode(state.genesis_hash),
                ));
            }
            let committed_blocks = validate_recovered_chain(
                persistent_store.as_ref(),
                context,
                &committee,
                &state,
                config.bls_configured(),
            )?;
            (state, committed_blocks)
        } else {
            let existing_blocks = persistent_store.blocks_from_height(0)?;
            if !existing_blocks.is_empty() || persistent_store.get_committed_head().is_some() {
                return Err(anyhow::anyhow!(
                    "persistent store contains blocks but no consensus state"
                ));
            }

            let state = ConsensusState {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                high_qc: None,
                locked_qc: None,
                voted_views: Vec::new(),
                current_view: 0,
                committed_height: 0,
                committed_hash: genesis.hash(),
                consecutive_timeouts: 0,
                vc_sent_for_view: None,
            };
            // Genesis is finalized through the same atomic path as every
            // later block, so a fresh store cannot expose a half-initialized
            // chain after a crash.
            persistent_store.commit_block(&genesis, &state)?;
            (state, vec![genesis.clone()])
        };

        let resume_view = recovery_resume_view(&state);
        info!(
            persisted_view = state.current_view,
            resume_view,
            height = state.committed_height,
            voted_views = state.voted_views.len(),
            "Recovered consensus state from storage"
        );

        let safety = Safety::with_state_for_context(
            context,
            state.high_qc.clone(),
            state.locked_qc.clone(),
            &state.voted_views,
        )
        .map_err(|error| anyhow::anyhow!("cannot restore safety in consensus context: {error}"))?;

        let mut pacemaker = Pacemaker::new(view_timeout);
        pacemaker
            .set_context(context)
            .map_err(|error| anyhow::anyhow!("cannot bind pacemaker context: {error}"))?;
        pacemaker.set_view(resume_view);
        if resume_view > state.current_view {
            // A synced high QC proves the persisted view completed.  Its
            // successor starts with fresh timeout/view-change accounting.
            pacemaker.set_timeout_state(0, None);
        } else {
            pacemaker.set_timeout_state(state.consecutive_timeouts, state.vc_sent_for_view);
        }

        let committed_height = state.committed_height;
        let committed_hash = state.committed_hash;

        // Create the committee-bound timeout collector.
        let timeout_collector = Self::create_timeout_collector(&config);

        // Create sync handler with persistent store
        use std::sync::atomic::AtomicU64;
        let height_tracker = Arc::new(AtomicU64::new(committed_height));
        let sync_handler = SyncHandler::new(persistent_store.clone(), height_tracker);
        let recovered_store = MemoryBlockStore::new();
        if let Some(committed_head) = committed_blocks.last() {
            // The persistent height index remains the source of truth for
            // historical finalized blocks. Keep only the exact live head in
            // the in-memory cache; loading the entire committed chain here
            // made every restart's speculative budget grow with chain age.
            recovered_store.save(committed_head);
        }
        for qc in [state.high_qc.as_ref(), state.locked_qc.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(block) = persistent_store.get(&qc.block_hash) {
                recovered_store.save_speculative(&block)?;
            }
        }
        recovered_store.set_committed(&committed_hash);

        Ok(Self {
            config,
            context,
            safety,
            pacemaker,
            app: Box::new(NoOpApp),
            app_attached: false,
            reconciled_after_recovery: false,
            store: Box::new(recovered_store),
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
            equivocation_detector: EquivocationDetector::new_with_context(context),
            equivocation_broadcast_cursor: 0,
            vote_timestamps: HashMap::new(),
        })
    }

    /// Validate and durably stage a staking-derived next committee.
    ///
    /// The marker is only accepted for the current finalized head and is not
    /// consumed by the live runner yet. This gives showcase tooling a real,
    /// authenticated transition artifact while keeping partial runtime swaps
    /// impossible. A future activation implementation must clear the marker
    /// in the same durable boundary as its new context and registry.
    pub fn stage_epoch_transition(&self, proof: &EpochTransitionProof) -> Result<()> {
        let store = self.persistent_store.as_ref().ok_or_else(|| {
            anyhow::anyhow!("epoch-transition staging requires persistent storage")
        })?;
        let transition_height = proof.effective_height.checked_sub(2).ok_or_else(|| {
            anyhow::anyhow!(
                "epoch-transition effective height must leave the old certified child height"
            )
        })?;
        if transition_height != self.committed_height {
            return Err(anyhow::anyhow!(
                "epoch transition must be staged from the current finalized head (proof height {}, committed {})",
                transition_height,
                self.committed_height
            ));
        }
        let finalized_block = store.get(&proof.old_qc.block_hash).ok_or_else(|| {
            anyhow::anyhow!("transition proof block is not in persistent storage")
        })?;
        if finalized_block.hash() != self.committed_hash
            || finalized_block.height != self.committed_height
        {
            return Err(anyhow::anyhow!(
                "transition proof does not target the current finalized head"
            ));
        }
        let committee = self
            .config
            .committee()
            .map_err(|error| anyhow::anyhow!("invalid active committee: {error}"))?;
        let update = self
            .app
            .validator_set_update_for_transition(&finalized_block)
            .map_err(|error| {
                anyhow::anyhow!("cannot bind epoch-transition proof to application state: {error}")
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "application has no validator-set update for the finalized transition head"
                )
            })?;
        proof
            .validate_against_validator_set_update(
                &committee,
                &finalized_block,
                self.config.bls_configured(),
                &update,
            )
            .map_err(|error| anyhow::anyhow!("invalid epoch-transition proof: {error}"))?;
        store.save_epoch_transition_proof(proof)?;
        Ok(())
    }

    fn verify_equivocation_proof(&self, proof: &EquivocationProof) -> Result<(), String> {
        let committee = self.config.committee()?;
        super::verify_equivocation_proof(
            &committee,
            proof,
            self.context,
            self.config.bls_configured(),
        )
    }

    /// Remove journal entries whose evidence is already part of the durable
    /// finalized chain, then re-enqueue the remaining durable evidence before
    /// the first live proposal round.
    fn reconcile_equivocation_journal(&mut self) -> Result<()> {
        let Some(store) = self.persistent_store.clone() else {
            return Ok(());
        };

        let committed_blocks = store
            .blocks_from_height(0)?
            .into_iter()
            .filter(|block| block.height <= self.committed_height)
            .collect::<Vec<_>>();
        for block in &committed_blocks {
            for proof in committed_equivocation_proofs(block)
                .map_err(|error| anyhow::anyhow!("invalid committed evidence: {error}"))?
            {
                if proof.context != self.context {
                    return Err(anyhow::anyhow!(
                        "committed equivocation evidence has the wrong consensus context"
                    ));
                }
                store.delete_equivocation_proof(&proof)?;
            }
        }

        let pending = store.load_equivocation_proofs()?;
        for proof in pending {
            self.verify_equivocation_proof(&proof).map_err(|error| {
                anyhow::anyhow!("durable equivocation evidence failed verification: {error}")
            })?;
            if !self.app.submit_equivocation_evidence(proof.clone()) {
                return Err(anyhow::anyhow!(
                    "application rejected durable equivocation evidence for offender {}",
                    hash_short(&proof.offender)
                ));
            }
        }
        Ok(())
    }

    /// Retry a bounded rotating batch of durable evidence.  A broadcast
    /// failure is deliberately non-destructive: the journal row remains and
    /// the next round will retry it.
    async fn rebroadcast_equivocation_batch(&mut self) -> Result<()> {
        const MAX_BATCH: usize = 16;
        if !should_rebroadcast_equivocation_batch(self.network.gossip_enabled()) {
            return Ok(());
        }
        let Some(store) = self.persistent_store.clone() else {
            return Ok(());
        };
        let proofs = store.load_equivocation_proofs()?;
        if proofs.is_empty() {
            self.equivocation_broadcast_cursor = 0;
            return Ok(());
        }

        let start = self.equivocation_broadcast_cursor % proofs.len();
        let count = proofs.len().min(MAX_BATCH);
        for offset in 0..count {
            let proof = proofs[(start + offset) % proofs.len()].clone();
            self.verify_equivocation_proof(&proof).map_err(|error| {
                anyhow::anyhow!("durable equivocation evidence failed verification: {error}")
            })?;
            if !self.app.submit_equivocation_evidence(proof.clone()) {
                return Err(anyhow::anyhow!(
                    "application rejected durable equivocation evidence for offender {}",
                    hash_short(&proof.offender)
                ));
            }
            if let Err(error) = self
                .network
                .broadcast(&Message::EquivocationEvidence(proof))
                .await
            {
                warn!(error = %error, "Failed to rebroadcast durable equivocation evidence; retaining journal row");
            }
        }
        self.equivocation_broadcast_cursor = (start + count) % proofs.len();
        Ok(())
    }

    fn initialize_live(&mut self) -> Result<()> {
        if !self.app_attached {
            return Err(anyhow::anyhow!(
                "live ConsensusRunner requires an explicitly attached application hook"
            ));
        }
        let recovered_head = self
            .store
            .get(&self.committed_hash)
            .or_else(|| self.store.get_by_height(self.committed_height))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "application recovery handshake cannot find committed head at height {}",
                    self.committed_height
                )
            })?;
        if recovered_head.height != self.committed_height
            || recovered_head.hash() != self.committed_hash
        {
            return Err(anyhow::anyhow!(
                "application recovery handshake found a mismatched consensus head"
            ));
        }
        self.app
            .validate_recovery_head(&recovered_head)
            .map_err(|error| {
                anyhow::anyhow!(
                    "application recovery handshake failed at height {}: {}",
                    self.committed_height,
                    error
                )
            })?;
        let committee = Self::validate_network_config(&self.config)?;
        self.pacemaker
            .with_view_change_committee(committee)
            .map_err(|error| anyhow::anyhow!("cannot enable committee view changes: {error}"))?;
        if !self.reconciled_after_recovery {
            self.reconcile_equivocation_journal()?;
            self.prune_speculative_stores(None)
                .map_err(|error| anyhow::anyhow!(error))?;
            self.reconciled_after_recovery = true;
        }
        Ok(())
    }

    async fn run_one_round(&mut self) -> Result<()> {
        // A crash can occur after the hash journal write and before the
        // admission round's final prune. Reconcile that bounded journal once
        // per round, before waiting on the network, so restart cannot revive
        // an unbounded speculative store.
        if self.reconciled_after_recovery {
            self.reconciled_after_recovery = false;
        } else {
            self.prune_speculative_stores(None)
                .map_err(|error| anyhow::anyhow!(error))?;
        }
        self.rebroadcast_equivocation_batch().await?;
        let view = self.pacemaker.current_view();
        if self.config.is_leader(view) {
            self.run_leader_round(view).await
        } else {
            self.run_follower_round(view).await
        }
    }

    async fn delay_after_round(&self) {
        // Configurable delay to prevent tight loop (0 = yield only)
        let delay_ms = Config::global().consensus_loop_delay_ms;
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        } else {
            tokio::task::yield_now().await;
        }
    }

    /// Run the consensus loop
    pub async fn run(&mut self) -> Result<()> {
        info!(
            node = %hash_short(&self.config.node_id),
            "Starting consensus runner"
        );

        self.initialize_live()?;

        loop {
            self.run_one_round().await?;
            self.delay_after_round().await;
        }
    }

    /// Run consensus until the requested height has been committed.
    pub async fn run_until_committed(&mut self, target_height: u64) -> Result<()> {
        self.initialize_live()?;
        if self.committed_height >= target_height {
            return Ok(());
        }

        info!(
            node = %hash_short(&self.config.node_id),
            target_height,
            "Starting consensus runner until committed height"
        );
        while self.committed_height < target_height {
            self.run_one_round().await?;
            self.delay_after_round().await;
        }

        Ok(())
    }

    /// Run one round as leader
    async fn run_leader_round(&mut self, view: View) -> Result<()> {
        info!(view, "Running as LEADER");

        let parent = self.get_proposal_parent();
        let (parent_head, parent_ancestors) = self
            .load_speculative_branch(parent.hash())
            .map_err(|error| anyhow::anyhow!("selected proposal parent unavailable: {error}"))?;
        let parent_roots = [parent.hash()];
        self.restore_application_branch_for_admission(
            &parent_head,
            &parent_ancestors,
            &parent_roots,
            0,
        )
        .map_err(|error| anyhow::anyhow!("selected proposal parent restore failed: {error}"))?;
        let payload = self.app.prepare_payload(&parent);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();

        // Create block with justify (QC that certifies our parent)
        let justify = self
            .connected_high_qc()
            .filter(|qc| qc.block_hash == parent.hash());
        let mut block = Block {
            epoch: self.context.epoch,
            committee_hash: self.context.committee_hash,
            genesis_hash: self.context.genesis_hash,
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload,
            proposer: self.config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: if parent.height == 0 {
                now.as_millis() as u64
            } else {
                (now.as_millis() as u64).max(parent.timestamp).min(
                    parent
                        .timestamp
                        .saturating_add(crate::types::MAX_BLOCK_TIMESTAMP_STEP_MS),
                )
            },
            justify: justify.clone(),
        };
        // Authenticate the complete application result on a private replay
        // before any live candidate or pending branch can be evicted.
        let (committed_head, ancestors) = self
            .preflight_application_branch(&block)
            .map_err(|error| anyhow::anyhow!("prepared block rejected: {error}"))?;
        // A leader may deliberately fall back to the exact committed head
        // when a stale/unanchored high QC is present. In that case there is
        // no attached justify and the follower vote rule is not applicable;
        // an anchored justify is still checked read-only before pruning.
        if block.justify.is_some() {
            self.safety
                .safe_to_vote(&block, block.app_hash)
                .map_err(|error| anyhow::anyhow!("prepared block is unsafe: {error}"))?;
        }

        let proposal_roots = [block.parent];
        if !self.pending_admission_available(&block, &proposal_roots) {
            return Err(anyhow::anyhow!(
                "prepared block rejected by pending resource limit"
            ));
        }
        self.check_application_branch_admission(&committed_head, &ancestors, &proposal_roots, 1)
            .map_err(|error| {
                anyhow::anyhow!("prepared block branch admission rejected: {error}")
            })?;
        // Only a fully validated and safety-eligible proposal may release
        // unprotected branches to make room for its candidate.
        if self.pending.len() >= Self::pending_admission_limit(&block) {
            self.prune_pending_unprotected_branches(Some(block.parent));
        }
        self.ensure_pending_capacity(&block).map_err(|error| {
            anyhow::anyhow!("prepared block rejected by resource limit: {error}")
        })?;
        self.restore_application_branch_for_admission(
            &committed_head,
            &ancestors,
            &proposal_roots,
            1,
        )
        .map_err(|error| anyhow::anyhow!("prepared block branch restore failed: {error}"))?;
        self.app
            .validate_block(&block)
            .map_err(|error| anyhow::anyhow!("prepared block rejected: {error}"))?;
        block.app_hash = self.app.execute(&block);
        let commitment = self
            .app
            .derive_execution_commitment(&block)
            .map_err(|error| {
                anyhow::anyhow!(
                    "prepared block commitment rejected at height {}: {error}",
                    block.height
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "prepared block has no execution commitment at height {}",
                    block.height
                )
            })?;
        block.commitment_root = commitment
            .root()
            .map_err(|error| anyhow::anyhow!("prepared block commitment root failed: {error}"))?;
        self.app
            .seal_execution_commitment(&block)
            .map_err(|error| {
                anyhow::anyhow!(
                    "prepared block commitment seal failed at height {}: {error}",
                    block.height
                )
            })?;
        let sealed_commitment = self
            .app
            .preflight_commitment(&block)
            .map_err(|error| {
                anyhow::anyhow!(
                    "sealed block commitment preflight failed at height {}: {error}",
                    block.height
                )
            })?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "sealed block has no execution commitment at height {}",
                    block.height
                )
            })?;
        let sealed_root = sealed_commitment
            .root()
            .map_err(|error| anyhow::anyhow!("sealed block commitment root failed: {error}"))?;
        if sealed_root != block.commitment_root {
            return Err(anyhow::anyhow!(
                "sealed block commitment root mismatch at height {}",
                block.height
            ));
        }
        let authenticated_root = self.app.preflight_state_root(&block).map_err(|error| {
            anyhow::anyhow!(
                "prepared block state-root generation failed at height {}: {error}",
                block.height
            )
        })?;
        if authenticated_root != Some(block.app_hash) {
            return Err(anyhow::anyhow!(
                "prepared block authenticated state-root mismatch at height {}: block {}, preflight {:?}",
                block.height,
                hex::encode(block.app_hash),
                authenticated_root.map(|root| hex::encode(root))
            ));
        }

        let block_hash = block.hash();
        info!(view, height = block.height, hash = %hash_short(&block_hash), "Proposing block");

        let store_roots = self.protected_speculative_roots(Some(block.parent));
        self.admit_speculative_stores(&block, &store_roots)
            .map_err(|error| anyhow::anyhow!("speculative store admission failed: {error}"))?;
        self.insert_pending(block.clone())
            .expect("pending admission changed after preflight");
        self.prune_speculative_stores(Some(block_hash))
            .map_err(|error| anyhow::anyhow!("speculative store prune failed: {error}"))?;

        let mut propose = Propose {
            epoch: self.context.epoch,
            committee_hash: self.context.committee_hash,
            genesis_hash: self.context.genesis_hash,
            block: block.clone(),
            justify,
            proposer_signature: vec![],
        };
        if let Some(bls_sk) = self.config.bls_secret_key() {
            propose.proposer_signature = bls_sk.sign(&propose.signing_data()).to_bytes().to_vec();
        }
        self.network.broadcast_propose(propose).await?;

        // Self-vote (with BLS signature if enabled)
        let self_vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(
                self.context,
                view,
                block_hash,
                block.app_hash,
                self.config.node_id,
                &bls_sk,
            )
        } else {
            Vote::new(
                self.context,
                view,
                block_hash,
                block.app_hash,
                self.config.node_id,
            )
        };
        // Count the leader's own vote through the same committee-bound
        // verifier as peer votes.  This prevents a malformed local key from
        // silently producing an invalid QC.
        let committee = self
            .config
            .committee()
            .map_err(|error| anyhow::anyhow!("invalid active committee: {error}"))?;
        verify_vote(
            &committee,
            &self_vote,
            self.context,
            view,
            &block_hash,
            &block.app_hash,
            self.config.bls_configured(),
        )
        .map_err(|error| anyhow::anyhow!("local vote rejected: {error}"))?;
        // A leader's own vote is a real validator vote.  Persist the next
        // safety state before exposing it to the local aggregator.  If this
        // write fails, `persist_vote_intent_with_retry` fail-stops and the
        // vote is never counted.
        let mut next_safety = self.safety.clone();
        next_safety.record_vote(view);
        self.persist_vote_intent_with_retry(view, &next_safety);
        self.safety = next_safety;
        self.votes.entry(block_hash).or_default().push(self_vote);

        // Collect votes
        let votes = self
            .collect_votes(view, block_hash, block.app_hash, Duration::from_secs(3))
            .await;

        if committee
            .has_weighted_quorum(votes.iter().map(|vote| vote.voter))
            .unwrap_or(false)
        {
            info!(view, votes = votes.len(), "Collected quorum, forming QC");
            let qc = form_certificate(
                &committee,
                self.context,
                votes,
                self.config.bls_configured(),
            )
            .map_err(|error| anyhow::anyhow!("failed to form QC: {error}"))?;
            let prepare = Prepare {
                epoch: self.context.epoch,
                committee_hash: self.context.committee_hash,
                genesis_hash: self.context.genesis_hash,
                view,
                qc: qc.clone(),
            };
            self.network.broadcast_prepare(prepare).await?;
            self.process_qc(qc);
            if let Some(ref high_qc) = self.safety.high_qc() {
                self.pacemaker.advance_view(high_qc);
            }
        } else {
            warn!(
                view,
                votes = votes.len(),
                "Failed to collect weighted quorum"
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
                    let leader = self.config.leader_of_active(view);
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
        let mut advanced_by_new_view = false;
        let mut advanced_by_timeout_certificate = false;
        debug!(view, "Handling timeout with view change");

        // Broadcast BLS-signed Timeout for TimeoutCertificate collection
        if let Some(bls_sk) = self.config.bls_secret_key() {
            let timeout = create_signed_timeout_with_context(
                self.context,
                view,
                self.safety.high_qc_view(),
                self.config.node_id,
                &bls_sk,
            );

            // Broadcast timeout
            if let Err(e) = self
                .network
                .broadcast(&Message::Timeout(timeout.clone()))
                .await
            {
                warn!(error = %e, "Failed to broadcast Timeout");
            }

            // Process our own timeout (might reach quorum)
            if let Some(tc) = self.process_timeout(timeout) {
                self.handle_timeout_certificate(tc).await?;
                advanced_by_timeout_certificate = true;
            }
        }

        // A TC already advances the pacemaker.  Do not create a ViewChange
        // for the newly advanced view from this old timeout; doing so would
        // skip the transition that the TC just certified.
        if !should_emit_view_change_after_timeout_certificate(advanced_by_timeout_certificate) {
            return Ok(());
        }

        // Create and broadcast ViewChange (BLS-signed when key available, else unsigned)
        let vc_opt = if let Some(bls_sk) = self.config.bls_secret_key() {
            self.pacemaker.create_signed_view_change(
                self.config.node_id,
                self.safety.high_qc().cloned(),
                &bls_sk,
            )
        } else {
            self.pacemaker
                .create_view_change(self.config.node_id, self.safety.high_qc().cloned())
        };

        if let Some(vc) = vc_opt {
            let committee = Self::validate_network_config(&self.config)?;
            validate_view_change_with_committee_and_context(&vc, &committee, self.context, view)
                .map_err(|error| anyhow::anyhow!("rejecting invalid local ViewChange: {error}"))?;
            if let Some(qc) = &vc.high_qc {
                self.verify_high_qc_locally(&committee, qc)
                    .map_err(|error| {
                        anyhow::anyhow!("rejecting unanchored local ViewChange high QC: {error}")
                    })?;
            }

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
                let before = self.pacemaker.current_view();
                self.handle_view_change_certificate(vcc).await?;
                advanced_by_new_view = self.pacemaker.current_view() > before;
            }
        }

        if !advanced_by_new_view {
            self.pacemaker.record_timeout();
        }
        Ok(())
    }

    /// Process a received Timeout message
    fn process_timeout(&mut self, timeout: crate::types::Timeout) -> Option<TimeoutCertificate> {
        if timeout.validate_context(self.context).is_err() {
            warn!("Rejecting Timeout with mismatched consensus context");
            return None;
        }
        let current_view = self.pacemaker.current_view();
        if !is_view_in_bounded_window(current_view, timeout.view) {
            warn!(
                timeout_view = timeout.view,
                current_view, "Rejecting Timeout outside the bounded view window"
            );
            return None;
        }
        let collector = self.timeout_collector.as_mut()?;

        match collector.add(timeout) {
            Ok(Some(tc)) => {
                info!(
                    view = tc.view,
                    signers = tc.signers.len(),
                    "Timeout quorum reached"
                );
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
        tc.validate_context(self.context)
            .map_err(|error| anyhow::anyhow!("rejecting TimeoutCertificate: {error}"))?;
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
        vcc.validate_context(self.context).map_err(|error| {
            anyhow::anyhow!("rejecting ViewChange certificate context: {error}")
        })?;
        let committee = Self::validate_network_config(&self.config)?;
        validate_view_change_certificate_with_committee_and_context(
            &vcc,
            &committee,
            self.context,
            self.pacemaker.current_view(),
        )
        .map_err(|error| anyhow::anyhow!("rejecting invalid ViewChange certificate: {error}"))?;
        self.verify_vcc_high_qcs_locally(&committee, &vcc)
            .map_err(|error| anyhow::anyhow!("rejecting unanchored ViewChange high QC: {error}"))?;

        let new_view = vcc.view;
        let new_leader = self.config.leader_of_active(new_view);

        info!(
            new_view,
            new_leader = %hash_short(&new_leader),
            "ViewChange quorum reached"
        );

        // If we're the new leader, broadcast NewView
        if new_leader == self.config.node_id {
            let high_qc = vcc.highest_qc().cloned();

            let nv = NewView {
                epoch: self.context.epoch,
                committee_hash: self.context.committee_hash,
                genesis_hash: self.context.genesis_hash,
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

    /// Authenticate a peer ViewChange before passing it to the committee
    /// collector.  The transport identity is part of the authorization.
    fn accept_view_change(
        &mut self,
        from: crate::types::NodeId,
        vc: ViewChange,
    ) -> Option<ViewChangeCertificate> {
        if from != vc.sender {
            warn!(
                from = %hash_short(&from),
                sender = %hash_short(&vc.sender),
                "Rejecting ViewChange with mismatched peer identity"
            );
            return None;
        }
        if let Err(error) = vc.validate_context(self.context) {
            warn!(
                error,
                "Rejecting ViewChange with mismatched consensus context"
            );
            return None;
        }

        let committee = match Self::validate_network_config(&self.config) {
            Ok(committee) => committee,
            Err(error) => {
                warn!(error = %error, "Rejecting ViewChange with invalid live configuration");
                return None;
            }
        };
        let current_view = self.pacemaker.current_view();
        if vc.to_view != current_view.saturating_add(1) {
            warn!(
                current_view,
                target_view = vc.to_view,
                "Rejecting ViewChange for a non-current transition"
            );
            return None;
        }
        if let Err(error) = validate_view_change_with_committee_and_context(
            &vc,
            &committee,
            self.context,
            current_view,
        ) {
            warn!(
                from = %hash_short(&from),
                error = %error,
                "Rejecting unauthenticated ViewChange"
            );
            return None;
        }
        if let Some(qc) = &vc.high_qc {
            if let Err(error) = self.verify_high_qc_locally(&committee, qc) {
                warn!(
                    from = %hash_short(&from),
                    error,
                    "Rejecting ViewChange with an unanchored high QC"
                );
                return None;
            }
        }
        self.pacemaker.on_view_change(vc)
    }

    /// Authenticate a NewView before any pacemaker or safety mutation.
    fn accept_new_view(&mut self, from: crate::types::NodeId, nv: NewView) -> bool {
        if let Err(error) = nv.validate_context(self.context) {
            warn!(error, "Rejecting NewView with mismatched consensus context");
            return false;
        }
        let current_view = self.pacemaker.current_view();
        let expected_leader = self.config.leader_of_active(nv.view);
        if from != expected_leader {
            warn!(
                from = %hash_short(&from),
                expected_leader = %hash_short(&expected_leader),
                view = nv.view,
                "Rejecting NewView from non-scheduled leader"
            );
            return false;
        }
        if nv.view != current_view.saturating_add(1)
            || nv.view > current_view.saturating_add(MAX_FUTURE_VIEWS)
        {
            warn!(
                current_view,
                new_view = nv.view,
                "Rejecting stale or unreasonable NewView transition"
            );
            return false;
        }

        let committee = match Self::validate_network_config(&self.config) {
            Ok(committee) => committee,
            Err(error) => {
                warn!(error = %error, "Rejecting NewView with invalid live configuration");
                return false;
            }
        };
        if let Err(error) = validate_view_change_certificate_with_committee_and_context(
            &nv.view_change_cert,
            &committee,
            self.context,
            current_view,
        ) {
            warn!(
                error,
                "Rejecting NewView with invalid ViewChange certificate"
            );
            return false;
        }
        if let Err(error) = self.verify_vcc_high_qcs_locally(&committee, &nv.view_change_cert) {
            warn!(error, "Rejecting NewView with an unanchored high QC");
            return false;
        }
        if nv.view_change_cert.view != nv.view {
            warn!("Rejecting NewView whose VCC view does not match its view");
            return false;
        }

        let highest_qc = nv.view_change_cert.highest_qc().cloned();
        if nv.high_qc != highest_qc {
            warn!("Rejecting NewView whose high QC is not the canonical VCC high QC");
            return false;
        }
        self.pacemaker.on_new_view(&nv);
        if let Some(qc) = nv.high_qc {
            self.safety.update_high_qc(qc);
        }
        true
    }

    /// Wait for a proposal for the given view
    async fn wait_for_proposal(&mut self, target_view: View) -> Result<Propose> {
        loop {
            let (from, msg) = self.network.recv_msg().await?;

            match msg {
                Message::Propose(propose) => {
                    if propose.validate_context(self.context).is_err()
                        || propose.block.validate_context(self.context).is_err()
                    {
                        warn!("Rejecting proposal with mismatched consensus context");
                        continue;
                    }
                    let leader = self.config.leader_of_active(target_view);
                    if from != leader || propose.block.proposer != leader {
                        warn!(
                            from = %hash_short(&from),
                            expected_leader = %hash_short(&leader),
                            "Rejecting proposal from non-scheduled leader"
                        );
                        continue;
                    }
                    if self.config.bls_configured() {
                        let committee = match self.config.committee() {
                            Ok(committee) => committee,
                            Err(error) => {
                                warn!(error = %error, "Rejecting proposal with invalid committee");
                                continue;
                            }
                        };
                        if let Err(error) = propose.verify_signature(&committee) {
                            warn!(error = %error, "Rejecting proposal with invalid proposer signature");
                            continue;
                        }
                    }
                    if propose.block.view == target_view {
                        return Ok(propose);
                    }
                    debug!(
                        got = propose.block.view,
                        expected = target_view,
                        "Wrong view proposal"
                    );
                }
                Message::Vote(vote) => {
                    if from != vote.voter {
                        continue;
                    }
                    if !self.has_locally_known_vote_subject(&vote, target_view) {
                        warn!(
                            view = vote.view,
                            target_view, "Rejecting vote for an unknown or mismatched local block"
                        );
                        continue;
                    }
                    if vote.validate_context(self.context).is_err() {
                        continue;
                    }
                    let valid = self
                        .config
                        .committee()
                        .ok()
                        .and_then(|committee| {
                            verify_vote(
                                &committee,
                                &vote,
                                self.context,
                                vote.view,
                                &vote.block_hash,
                                &vote.app_hash,
                                self.config.bls_configured(),
                            )
                            .ok()
                        })
                        .is_some();
                    if !valid {
                        continue;
                    }
                    // CRITICAL-7: Rate limit votes to prevent DoS
                    if self.is_vote_rate_limited(&vote.voter) {
                        continue;
                    }
                    if let Some(proof) = store_vote_with_context(
                        &mut self.votes,
                        vote,
                        self.context,
                        &mut self.equivocation_detector,
                    ) {
                        self.handle_equivocation(proof).await;
                    }
                }
                Message::Prepare(prepare) if prepare.view == target_view => {
                    if prepare.validate_context(self.context).is_ok()
                        && prepare.qc.validate_context(self.context).is_ok()
                        && from == self.config.leader_of_active(target_view)
                    {
                        self.process_prepare(prepare);
                    }
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
                Message::ViewChange(vc) => {
                    if let Some(vcc) = self.accept_view_change(from, vc) {
                        if let Err(error) = self.handle_view_change_certificate(vcc).await {
                            warn!(error = %error, "Failed to process ViewChange certificate");
                        }
                    }
                }
                Message::NewView(nv) => {
                    self.accept_new_view(from, nv);
                }
                Message::EquivocationEvidence(proof) => {
                    self.handle_inbound_equivocation_or_fail_stop(proof).await;
                }
                Message::UserTransaction(envelope) => {
                    self.handle_user_transaction(from, envelope).await;
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
                Message::Prepare(prepare) if prepare.view == target_view => {
                    if prepare.validate_context(self.context).is_ok()
                        && prepare.qc.validate_context(self.context).is_ok()
                        && from == self.config.leader_of_active(target_view)
                    {
                        return Ok(prepare);
                    }
                }
                Message::Vote(vote) => {
                    if from != vote.voter {
                        continue;
                    }
                    if !self.has_locally_known_vote_subject(&vote, target_view) {
                        warn!(
                            view = vote.view,
                            target_view, "Rejecting vote for an unknown or mismatched local block"
                        );
                        continue;
                    }
                    if vote.validate_context(self.context).is_err() {
                        continue;
                    }
                    let valid = self
                        .config
                        .committee()
                        .ok()
                        .and_then(|committee| {
                            verify_vote(
                                &committee,
                                &vote,
                                self.context,
                                vote.view,
                                &vote.block_hash,
                                &vote.app_hash,
                                self.config.bls_configured(),
                            )
                            .ok()
                        })
                        .is_some();
                    if !valid {
                        continue;
                    }
                    // CRITICAL-7: Rate limit votes to prevent DoS
                    if self.is_vote_rate_limited(&vote.voter) {
                        continue;
                    }
                    if let Some(proof) = store_vote_with_context(
                        &mut self.votes,
                        vote,
                        self.context,
                        &mut self.equivocation_detector,
                    ) {
                        self.handle_equivocation(proof).await;
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
                Message::ViewChange(vc) => {
                    if let Some(vcc) = self.accept_view_change(from, vc) {
                        if let Err(error) = self.handle_view_change_certificate(vcc).await {
                            warn!(error = %error, "Failed to process ViewChange certificate");
                        }
                    }
                }
                Message::NewView(nv) => {
                    self.accept_new_view(from, nv);
                }
                Message::EquivocationEvidence(proof) => {
                    self.handle_inbound_equivocation_or_fail_stop(proof).await;
                }
                Message::UserTransaction(envelope) => {
                    self.handle_user_transaction(from, envelope).await;
                }
                _ => {}
            }
        }
    }

    /// Collect votes until quorum or timeout
    async fn collect_votes(
        &mut self,
        view: View,
        block_hash: Hash,
        app_hash: Hash,
        timeout_duration: Duration,
    ) -> Vec<Vote> {
        let committee = match self.config.committee() {
            Ok(committee) => committee,
            Err(error) => {
                warn!(error, "Cannot collect votes with invalid committee");
                return vec![];
            }
        };
        let deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            let has_quorum = self
                .votes
                .get(&block_hash)
                .map(|votes| {
                    committee
                        .has_weighted_quorum(votes.iter().map(|vote| vote.voter))
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if has_quorum || tokio::time::Instant::now() >= deadline {
                return self.votes.remove(&block_hash).unwrap_or_default();
            }

            let remaining = deadline - tokio::time::Instant::now();
            match timeout(remaining, self.network.recv_msg()).await {
                Ok(Ok((from, msg))) => match msg {
                    Message::Vote(vote) => {
                        if from != vote.voter {
                            warn!(
                                from = %hash_short(&from),
                                voter = %hash_short(&vote.voter),
                                "Rejecting vote whose peer identity does not match voter"
                            );
                            continue;
                        }
                        if vote.validate_context(self.context).is_err() {
                            warn!("Rejecting vote with mismatched consensus context");
                            continue;
                        }
                        // CRITICAL-7: Rate limit votes to prevent DoS
                        if self.is_vote_rate_limited(&vote.voter) {
                            continue;
                        }
                        let vote_valid = if vote.block_hash == block_hash {
                            verify_vote(
                                &committee,
                                &vote,
                                self.context,
                                view,
                                &block_hash,
                                &app_hash,
                                self.config.bls_configured(),
                            )
                        } else if vote.view == view {
                            // Verify conflicting votes against their own
                            // signed payload before feeding them to the
                            // equivocation detector.
                            verify_vote(
                                &committee,
                                &vote,
                                self.context,
                                view,
                                &vote.block_hash,
                                &vote.app_hash,
                                self.config.bls_configured(),
                            )
                        } else {
                            Err("vote is from a different view".to_string())
                        };
                        if let Err(error) = vote_valid {
                            warn!(
                                from = %hash_short(&from),
                                voter = %hash_short(&vote.voter),
                                error,
                                "Rejecting invalid network vote"
                            );
                            continue;
                        }
                        if self
                            .votes
                            .get(&vote.block_hash)
                            .map(|votes| votes.iter().any(|existing| existing.voter == vote.voter))
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        debug!(from = %hash_short(&from), view = vote.view, "Received verified vote");
                        // Check for equivocation before storing
                        if let Some(proof) = store_vote_with_context(
                            &mut self.votes,
                            vote,
                            self.context,
                            &mut self.equivocation_detector,
                        ) {
                            self.handle_equivocation(proof).await;
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
                    Message::ViewChange(vc) => {
                        if let Some(vcc) = self.accept_view_change(from, vc) {
                            if let Err(error) = self.handle_view_change_certificate(vcc).await {
                                warn!(error = %error, "Failed to process ViewChange certificate");
                            }
                        }
                    }
                    Message::NewView(nv) => {
                        self.accept_new_view(from, nv);
                    }
                    Message::EquivocationEvidence(proof) => {
                        self.handle_inbound_equivocation_or_fail_stop(proof).await;
                    }
                    Message::UserTransaction(envelope) => {
                        self.handle_user_transaction(from, envelope).await;
                    }
                    _ => {}
                },
                Ok(Err(e)) => warn!(error = %e, "Error receiving message"),
                Err(_) => break,
            }
        }

        self.votes.remove(&block_hash).unwrap_or_default()
    }

    /// Return true only for a vote whose subject is already known locally and
    /// whose authenticated metadata matches that block exactly.  This keeps
    /// pre-proposal/future-view votes from growing the in-memory vote maps.
    fn has_locally_known_vote_subject(&self, vote: &Vote, target_view: View) -> bool {
        if vote.view != target_view {
            return false;
        }

        let block = self.known_block(&vote.block_hash);
        block
            .map(|block| {
                block.context() == self.context
                    && block.view == vote.view
                    && block.app_hash == vote.app_hash
            })
            .unwrap_or(false)
    }

    /// Verify a proposal's QC (justify certificate)
    ///
    /// Mirrors the verification logic from engine.rs.
    /// Returns Ok(()) only when the proposal is connected to the local chain
    /// and its justification is structurally and cryptographically valid.
    fn verify_proposal_qc(&self, propose: &Propose) -> Result<(), String> {
        let block = &propose.block;

        block.validate_context(self.context)?;
        block.validate()?;
        propose.validate_context(self.context)?;

        if block.height <= self.committed_height {
            return Err(format!(
                "proposal height {} is not above committed height {}",
                block.height, self.committed_height
            ));
        }
        if block.height == self.committed_height.saturating_add(1) {
            if block.parent != self.committed_hash {
                return Err("proposal does not extend the exact committed head".to_string());
            }
        } else if !self.is_chain_connected_to_committed_head(&block.parent) {
            return Err("proposal parent is not connected to the committed head".to_string());
        }

        if block.height == 0 {
            return Err("network proposals may not use height zero".to_string());
        }

        let parent_timestamp = if block.height == 1 {
            0
        } else {
            self.known_block(&block.parent)
                .ok_or_else(|| "proposal parent is not available locally".to_string())?
                .timestamp
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "local clock precedes Unix epoch".to_string())?
            .as_millis() as u64;
        block.validate_live_timestamp(parent_timestamp, now_ms)?;

        if block.height == 1 {
            if block.parent != Block::genesis(self.context).hash() {
                return Err("height-one proposal does not extend canonical genesis".to_string());
            }
            if propose.justify.is_some() || block.justify.is_some() {
                return Err("height-one proposal must not carry a QC".to_string());
            }
            return Ok(());
        }

        if propose.justify != block.justify {
            return Err("proposal and block justifications do not match".to_string());
        }

        let justify = propose
            .justify
            .as_ref()
            .ok_or_else(|| format!("Proposal at height {} missing QC", block.height))?;
        justify.validate_context(self.context)?;

        // QC must certify the parent block
        if justify.block_hash != block.parent {
            return Err(format!(
                "QC block_hash {} doesn't match parent {}",
                hash_short(&justify.block_hash),
                hash_short(&block.parent)
            ));
        }

        let parent = self
            .known_block(&block.parent)
            .ok_or_else(|| "proposal parent is not available locally".to_string())?;
        if block.height != parent.height.saturating_add(1) {
            return Err(format!(
                "proposal height {} does not follow parent height {}",
                block.height, parent.height
            ));
        }
        if justify.view != parent.view {
            return Err(format!(
                "QC view {} does not match parent view {}",
                justify.view, parent.view
            ));
        }
        let committee = self.config.committee()?;
        verify_certificate(
            &committee,
            justify,
            self.context,
            parent.view,
            &block.parent,
            Some(&parent.app_hash),
            self.config.bls_configured(),
        )?;

        Ok(())
    }

    /// Process a proposal
    fn process_proposal(&mut self, propose: Propose) -> Option<Vote> {
        if propose.validate_context(self.context).is_err()
            || propose.block.validate_context(self.context).is_err()
        {
            warn!("Rejecting proposal with mismatched consensus context");
            return None;
        }
        let view = propose.block.view;
        let height = propose.block.height;

        let leader = self.config.leader_of_active(view);
        if propose.block.proposer != leader {
            warn!(
                view,
                proposer = %hash_short(&propose.block.proposer),
                expected_leader = %hash_short(&leader),
                "Rejecting proposal from non-scheduled leader"
            );
            return None;
        }

        if self.config.bls_configured() {
            let committee = match self.config.committee() {
                Ok(committee) => committee,
                Err(error) => {
                    warn!(error = %error, "Rejecting proposal with invalid committee");
                    return None;
                }
            };
            if let Err(error) = propose.verify_signature(&committee) {
                warn!(view, error = %error, "Rejecting proposal with invalid proposer signature");
                return None;
            }
        }

        debug!(
            view,
            height,
            hash = %hash_short(&propose.block.hash()),
            "Processing proposal"
        );

        // SECURITY: Verify QC before any state mutation
        if let Err(e) = self.verify_proposal_qc(&propose) {
            warn!(view, error = %e, "Rejecting proposal: invalid QC");
            return None;
        }

        let mut block = propose.block;
        if block.justify.is_none() {
            block.justify = propose.justify.clone();
        }

        // Authenticate the complete application result on a private replay
        // before any live candidate or pending branch can be evicted.
        let (committed_head, ancestors) = match self.preflight_application_branch(&block) {
            Ok(branch) => branch,
            Err(error) => {
                warn!(
                    view,
                    error, "Rejecting proposal with invalid application payload"
                );
                return None;
            }
        };
        let mut prospective_safety = self.safety.clone();
        if let Some(justify) = propose.justify.as_ref() {
            prospective_safety.update_high_qc(justify.clone());
        }
        if let Err(error) = prospective_safety.safe_to_vote(&block, block.app_hash) {
            warn!(view, error = %error, "Unsafe to vote");
            return None;
        }

        // Only after the private validation and read-only safety check may
        // unrelated live branches be released to make room.
        let prospective_roots =
            self.protected_speculative_roots_for_safety(&prospective_safety, Some(block.parent));
        let application_roots = [block.parent];
        if !self.pending_admission_available(&block, &prospective_roots) {
            warn!(
                view,
                height, "Rejecting proposal due to pending resource limit"
            );
            return None;
        }
        if let Err(error) = self.check_application_branch_admission(
            &committed_head,
            &ancestors,
            &application_roots,
            1,
        ) {
            warn!(
                view,
                error, "Rejecting proposal due to application resource limit"
            );
            return None;
        }
        if self.pending.len() >= Self::pending_admission_limit(&block) {
            self.prune_pending_unprotected_branches_with_roots(&prospective_roots);
        }
        if let Err(error) = self.ensure_pending_capacity(&block) {
            warn!(
                view,
                height,
                error = %error,
                "Rejecting proposal due to pending resource limit"
            );
            return None;
        }
        if let Err(error) = self.restore_application_branch_for_admission(
            &committed_head,
            &ancestors,
            &application_roots,
            1,
        ) {
            warn!(
                view,
                error, "Rejecting proposal with unavailable application branch"
            );
            return None;
        }

        if let Err(error) = self.app.validate_block(&block) {
            warn!(
                view,
                error, "Rejecting proposal with invalid application payload"
            );
            return None;
        }

        // Execute block and derive the exact root that must be authenticated
        // by the proposal's existing `app_hash` header field.
        let local_state_root = self.app.execute(&block);

        // Artifact generation is part of application validity.  Reject before
        // touching safety state, durable proposal storage, or emitting a vote.
        let commitment = match self.app.preflight_commitment(&block) {
            Ok(Some(commitment)) => commitment,
            Ok(None) => {
                warn!(view, "Rejecting proposal with missing execution commitment");
                return None;
            }
            Err(error) => {
                warn!(
                    view,
                    error, "Rejecting proposal with invalid execution commitment"
                );
                return None;
            }
        };
        match commitment.root() {
            Ok(root) if root == block.commitment_root => {}
            Ok(root) => {
                warn!(
                    view,
                    expected = %hex::encode(block.commitment_root),
                    got = %hex::encode(root),
                    "Rejecting proposal with mismatched execution commitment root"
                );
                return None;
            }
            Err(error) => {
                warn!(view, error = %error, "Rejecting proposal with invalid execution commitment root");
                return None;
            }
        }

        // Full-state root generation is consensus validity.  It must both be
        // reproducible from this execution and match the root authenticated by
        // the proposed block hash, before safety state or a vote is touched.
        let authenticated_root = match self.app.preflight_state_root(&block) {
            Ok(root) => root,
            Err(error) => {
                warn!(
                    view,
                    error, "Rejecting proposal with invalid authenticated full-state root"
                );
                return None;
            }
        };
        if local_state_root != block.app_hash || authenticated_root != Some(block.app_hash) {
            warn!(
                view,
                expected = %hex::encode(block.app_hash),
                executed = %hex::encode(local_state_root),
                preflight = ?authenticated_root,
                "Rejecting proposal with mismatched authenticated full-state root"
            );
            return None;
        }

        // Check safety
        if let Err(e) = prospective_safety.safe_to_vote(&block, local_state_root) {
            warn!(view, error = %e, "Unsafe to vote");
            return None;
        }

        // Store block BEFORE recording vote.  This is a rolling, atomic
        // admission: an older unprotected sibling and all of its speculative
        // descendants may be deleted in the same transition as this new row.
        // The durable journal is acknowledged before the in-memory cache is
        // changed, so a crash cannot resurrect the evicted body on recovery.
        if let Err(error) = self.admit_speculative_stores(&block, &prospective_roots) {
            warn!(
                view,
                error, "Rejecting proposal due to speculative store limit"
            );
            return None;
        }
        // Build the complete next safety state, including the vote and the
        // proposal's high QC, without mutating live consensus state yet.
        // Persisting this candidate state before the live mutation removes the
        // crash window in which `record_vote` could be observed in memory but
        // not on disk.  A failed write fail-stops before a vote can be sent.
        let mut next_safety = prospective_safety;
        next_safety.record_vote(view);
        self.persist_vote_intent_with_retry(view, &next_safety);
        self.safety = next_safety;
        self.insert_pending(block.clone())
            .expect("pending admission changed after preflight");
        self.prune_speculative_stores(Some(block.hash()))
            .unwrap_or_else(|error| panic!("CRITICAL: speculative store prune failed: {error}"));

        // Create vote (with BLS signature if enabled)
        let vote = if let Some(bls_sk) = self.config.bls_secret_key() {
            Vote::new_bls(
                self.context,
                view,
                block.hash(),
                local_state_root,
                self.config.node_id,
                &bls_sk,
            )
        } else {
            Vote::new(
                self.context,
                view,
                block.hash(),
                local_state_root,
                self.config.node_id,
            )
        };
        Some(vote)
    }

    /// Process a prepare message
    fn process_prepare(&mut self, prepare: Prepare) {
        if prepare.validate_context(self.context).is_err()
            || prepare.qc.validate_context(self.context).is_err()
        {
            warn!("Rejecting prepare with mismatched consensus context");
            return;
        }
        debug!(view = prepare.view, "Processing prepare");

        // SECURITY: verify every prepare QC against the active committee,
        // including signer identity, configured keys, app hash, and weighted
        // quorum, before updating any safety state.
        let committee = match self.config.committee() {
            Ok(committee) => committee,
            Err(error) => {
                warn!(
                    view = prepare.view,
                    error, "Rejecting prepare: invalid committee"
                );
                return;
            }
        };
        let certified_block = match self.known_block(&prepare.qc.block_hash) {
            Some(block) => block,
            None => {
                warn!(
                    view = prepare.view,
                    hash = %hash_short(&prepare.qc.block_hash),
                    "Rejecting prepare for unknown certified block"
                );
                return;
            }
        };
        if !self.is_chain_connected_to_committed_head(&certified_block.hash()) {
            warn!(
                view = prepare.view,
                hash = %hash_short(&prepare.qc.block_hash),
                "Rejecting prepare whose certified block is not connected to the committed head"
            );
            return;
        }
        if certified_block.view != prepare.view {
            warn!(
                view = prepare.view,
                certified_view = certified_block.view,
                "Rejecting prepare whose QC view does not match certified block"
            );
            return;
        }
        let expected_app_hash = Some(certified_block.app_hash);
        if let Err(error) = verify_certificate(
            &committee,
            &prepare.qc,
            self.context,
            prepare.view,
            &prepare.qc.block_hash,
            expected_app_hash.as_ref(),
            self.config.bls_configured(),
        ) {
            warn!(view = prepare.view, error, "Rejecting prepare: invalid QC");
            return;
        }

        // Update high_qc (now verified)
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
        if qc.validate_context(self.context).is_err() {
            warn!("Rejecting QC with mismatched consensus context");
            return;
        }
        debug!(
            view = qc.view,
            hash = %hash_short(&qc.block_hash),
            "Processing QC"
        );

        // 2-chain commit rule (HotStuff-2):
        // When we have QC for block B, commit B's PARENT.
        // This ensures block N is only committed when N+1 has been certified.
        let certified_block = self.known_block(&qc.block_hash);

        if let Some(block) = certified_block {
            if !self.is_chain_connected_to_committed_head(&block.hash()) {
                warn!(
                    view = qc.view,
                    hash = %hash_short(&qc.block_hash),
                    "Rejecting QC whose certified block is not connected to the committed head"
                );
                return;
            }

            // Update high_qc only after the certified block has been anchored
            // to the exact committed head.  Otherwise a valid QC for a fork
            // could drive leader-side speculative application execution.
            self.safety.update_high_qc(qc.clone());

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
            None => self.known_block(block_hash)?,
        };

        if block.height <= self.committed_height {
            return None;
        }

        // Commit ancestors first
        if block.height > self.committed_height + 1 {
            self.try_commit(&block.parent);
        }

        if block.height == self.committed_height + 1 && block.parent != self.committed_hash {
            warn!(
                height = block.height,
                hash = %hash_short(&block.hash()),
                "Cannot commit block that does not extend the exact committed head"
            );
            let height = block.height;
            if let Err(error) = self.insert_pending(block) {
                warn!(height, error = %error, "Unable to restore pending block after failed commit");
            }
            return None;
        }

        // Application state and consensus metadata must advance together.  If
        // an ancestor was unavailable or failed to commit, do not skip a
        // height and apply this block out of order.
        if block.height != self.committed_height + 1 {
            warn!(
                height = block.height,
                committed_height = self.committed_height,
                "Cannot commit non-sequential block"
            );
            let height = block.height;
            if let Err(error) = self.insert_pending(block) {
                warn!(height, error = %error, "Unable to restore pending block after failed commit");
            }
            return None;
        }

        // Validate the application payload before crossing the durable
        // commit boundary.  `validate_block` is read-only; `commit` below is
        // the first callback allowed to mutate canonical state or publish an
        // application event.
        self.app.validate_block(&block).unwrap_or_else(|error| {
            panic!(
                "CRITICAL: application preflight failed at height {}: {}",
                block.height, error
            )
        });

        // Build the exact execution commitment before crossing the durable
        // finality boundary.  Canonical applications source this from the
        // matching speculative candidate (or a private deterministic replay);
        // the returned value is the same one persisted below.
        let commitment = self
            .app
            .preflight_commitment(&block)
            .unwrap_or_else(|error| {
                panic!(
                    "CRITICAL: execution commitment preflight failed at height {}: {}",
                    block.height, error
                )
            });
        let commitment = commitment.unwrap_or_else(|| {
            panic!(
                "CRITICAL: application returned no execution commitment at height {}",
                block.height
            )
        });
        let commitment_root = commitment.root().unwrap_or_else(|error| {
            panic!(
                "CRITICAL: execution commitment root failed at height {}: {}",
                block.height, error
            )
        });
        if commitment_root != block.commitment_root {
            panic!(
                "CRITICAL: execution commitment root mismatch at height {}: block {}, preflight {}",
                block.height,
                hex::encode(block.commitment_root),
                hex::encode(commitment_root)
            );
        }
        let state_root = self
            .app
            .preflight_state_root(&block)
            .unwrap_or_else(|error| {
                panic!(
                    "CRITICAL: full-state root preflight failed at height {}: {}",
                    block.height, error
                )
            });
        if state_root != Some(block.app_hash) {
            panic!(
                "CRITICAL: authenticated full-state root mismatch at height {}: block {}, preflight {:?}",
                block.height,
                hex::encode(block.app_hash),
                state_root.map(|root| hex::encode(root))
            );
        }

        let committed_hash = block.hash();
        let committed_state = self.consensus_state_for(block.height, committed_hash);
        // Finalized block, height index, consensus safety state, and commit
        // metadata are one durable transaction.  Do this before invoking the
        // application so a storage failure cannot expose a canonical mutation
        // or BlockCommitted event.
        self.persist_commit_with_retry(
            &block,
            &committed_state,
            Some(&commitment),
            state_root.as_ref(),
        );

        // Application commit is deliberately after the synced durable write.
        // A post-persistence application failure is fail-stop; restart will
        // replay the durable finalized chain through the application hook.
        let app_hash = self.app.commit(&block).unwrap_or_else(|error| {
            panic!(
                "CRITICAL: application commit failed at height {} after durable persistence: {}",
                block.height, error
            )
        });
        if app_hash != block.app_hash {
            panic!(
                "CRITICAL: application hash mismatch at committed height {} after durable persistence: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(app_hash)
            );
        }

        // Notifications are explicitly outside the canonical mutation and
        // durable storage transactions. They may only observe a block after
        // both boundaries above succeeded, and recovery does not replay them.
        self.app
            .on_durable_commit(&block, &commitment)
            .unwrap_or_else(|error| {
                panic!(
                    "CRITICAL: post-commit notification validation failed at height {}: {}",
                    block.height, error
                )
            });

        // Evidence is removed only after both the finalized block and the
        // application commit have succeeded.  A delete failure is fail-stop;
        // the durable row remains available for recovery if the process is
        // restarted before this point is retried.
        if let Some(store) = self.persistent_store.clone() {
            let committed_proofs = committed_equivocation_proofs(&block).unwrap_or_else(|error| {
                panic!(
                    "CRITICAL: committed evidence payload could not be parsed at height {}: {}",
                    block.height, error
                )
            });
            for proof in committed_proofs {
                if proof.context != self.context {
                    panic!(
                        "CRITICAL: committed evidence at height {} has the wrong consensus context",
                        block.height
                    );
                }
                store.delete_equivocation_proof(&proof).unwrap_or_else(|error| {
                    panic!(
                        "CRITICAL: failed to delete committed equivocation journal row at height {}: {}",
                        block.height, error
                    )
                });
            }
        }

        // Publish the new in-memory committed head only after both the
        // durable write and application canonical mutation have completed.
        self.store.set_committed(&committed_hash);
        self.committed_height = block.height;
        self.committed_hash = committed_hash;
        if self.persistent_store.is_some() {
            let roots = self.protected_speculative_roots(None);
            self.store
                .prune_production_cache(&roots)
                .unwrap_or_else(|error| {
                    panic!("CRITICAL: production block-cache prune failed: {error}")
                });
        }
        // Only advertise the new height after the durable commit and the
        // runner's in-memory committed head have both advanced.  Otherwise a
        // peer can request a height that this node cannot serve after a crash.
        if let Some(handler) = self.sync_handler.as_ref() {
            handler.update_height(self.committed_height);
        }

        info!(
            height = block.height,
            hash = %hash_short(block_hash),
            "COMMITTED block"
        );

        // Dynamic committee changes are disabled until certificates carry an
        // epoch/committee binding.  Applying an update here would let a QC
        // from the previous committee mutate state under the new one.
        if let Some(update) = self.app.take_validator_update() {
            if !update.is_empty() {
                panic!(
                    "dynamic validator updates are disabled until verified epoch-transition certificates and historical committee support are implemented"
                );
            }
        }

        // Prune only branches that are no longer reachable from the current
        // high/locked-QC or proposal-parent roots. Every retained entry has
        // its complete ancestor closure, so required branches are never
        // truncated by an age/FIFO policy.
        self.prune_pending_unprotected_branches(None);
        self.safety.prune_votes_below(block.view);
        self.equivocation_detector.prune_below(block.view);
        // CRITICAL-7: Prune old vote collections to prevent unbounded memory growth
        self.prune_old_votes(block.view);

        Some(block)
    }

    /// Get parent for new proposal
    fn get_proposal_parent(&self) -> Block {
        if let Some(qc) = self.connected_high_qc() {
            if let Some(block) = self.known_block(&qc.block_hash) {
                return block;
            }
        }
        self.known_block(&self.committed_hash)
            .or_else(|| self.store.get_by_height(self.committed_height))
            .unwrap_or_else(|| Block::genesis(self.context))
    }

    /// Get current committed height
    pub fn committed_height(&self) -> u64 {
        self.committed_height
    }

    /// Set a custom application hook
    pub fn with_app<A: AppHook + 'static>(mut self, app: A) -> Self {
        self.app = Box::new(app);
        self.app_attached = true;
        self
    }

    /// Persist consensus state to storage.
    ///
    /// CRITICAL: This must be called after each vote to prevent double-voting
    /// after crash recovery. The voted_views set must survive crashes.
    fn consensus_state_for_safety(
        &self,
        safety: &Safety,
        committed_height: u64,
        committed_hash: Hash,
    ) -> ConsensusState {
        let (consecutive_timeouts, vc_sent_for_view) = self.pacemaker.timeout_state();
        ConsensusState {
            epoch: self.context.epoch,
            committee_hash: self.context.committee_hash,
            genesis_hash: self.context.genesis_hash,
            high_qc: safety.high_qc().cloned(),
            locked_qc: safety.locked_qc().cloned(),
            voted_views: safety.voted_views(),
            current_view: self.pacemaker.current_view(),
            committed_height,
            committed_hash,
            consecutive_timeouts,
            vc_sent_for_view,
        }
    }

    fn consensus_state_for(&self, committed_height: u64, committed_hash: Hash) -> ConsensusState {
        self.consensus_state_for_safety(&self.safety, committed_height, committed_hash)
    }

    /// Persist a finalized block and matching consensus state atomically.
    fn persist_commit_with_retry(
        &self,
        block: &Block,
        state: &ConsensusState,
        commitment: Option<&crate::types::CommitmentV2>,
        state_root: Option<&Hash>,
    ) {
        let Some(store) = self.persistent_store.as_ref() else {
            return;
        };
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 10;

        for attempt in 0..MAX_RETRIES {
            match store
                .commit_block_with_commitment_and_state_root(block, state, commitment, state_root)
            {
                Ok(()) => return,
                Err(error) => {
                    let delay_ms = BASE_DELAY_MS * 10u64.pow(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        delay_ms,
                        error = %error,
                        height = block.height,
                        "Atomic finalized commit failed, retrying"
                    );
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
            }
        }

        panic!(
            "CRITICAL: Failed to atomically persist finalized block after {} retries at height {}. Halting.",
            MAX_RETRIES, block.height
        );
    }

    /// Persist the candidate safety state before a vote is exposed to the
    /// network or local vote aggregator.
    ///
    /// The caller must not mutate `self.safety` until this returns.  On a
    /// persistent-store error all retries are exhausted and this function
    /// panics deliberately: continuing would allow a post-crash double vote.
    fn persist_vote_intent_with_retry(&self, view: View, next_safety: &Safety) {
        let Some(store) = self.persistent_store.as_ref() else {
            return;
        };
        let state = self.consensus_state_for_safety(
            next_safety,
            self.committed_height,
            self.committed_hash,
        );
        const MAX_RETRIES: u32 = 3;
        const BASE_DELAY_MS: u64 = 10;

        for attempt in 0..MAX_RETRIES {
            match store.save_consensus_state(&state) {
                Ok(()) => return,
                Err(e) => {
                    let delay_ms = BASE_DELAY_MS * 10u64.pow(attempt);
                    warn!(
                        attempt = attempt + 1,
                        max_retries = MAX_RETRIES,
                        delay_ms,
                        error = %e,
                        view,
                        "Persist vote intent failed, retrying"
                    );
                    std::thread::sleep(Duration::from_millis(delay_ms));
                }
            }
        }

        // All retries exhausted — halt to prevent Byzantine failure
        panic!(
            "CRITICAL: Failed to persist vote intent after {} retries (view {}). \
             Halting to prevent potential double-voting after crash recovery.",
            MAX_RETRIES, view
        );
    }

    /// Handle detected equivocation (Byzantine fault).
    ///
    /// This is called when a validator is caught voting for two different blocks
    /// in the same view. The evidence is submitted to the staking system for slashing.
    async fn handle_equivocation(&mut self, proof: EquivocationProof) {
        let proof = proof.canonicalized().unwrap_or_else(|error| {
            panic!("CRITICAL: detected equivocation was not canonical: {error}")
        });
        self.verify_equivocation_proof(&proof)
            .unwrap_or_else(|error| {
                panic!("CRITICAL: detected equivocation failed verification: {error}")
            });

        // Log the equivocation - this is a CRITICAL security event
        warn!(
            view = proof.view,
            offender = %hash_short(&proof.offender),
            hash_a = %hash_short(&proof.hash_a),
            hash_b = %hash_short(&proof.hash_b),
            "BYZANTINE FAULT: Equivocation detected! Validator voted for conflicting blocks."
        );

        let store = self.persistent_store.clone().unwrap_or_else(|| {
            panic!("CRITICAL: equivocation journal is unavailable for a live proof")
        });
        store
            .save_equivocation_proof(&proof)
            .unwrap_or_else(|error| {
                panic!("CRITICAL: failed to durably journal equivocation evidence: {error}")
            });

        // Submit evidence to the app's local proposal input queue.  The
        // resulting system transaction performs slashing only during block
        // execution, so message arrival order cannot mutate canonical state.
        let accepted = self.app.submit_equivocation_evidence(proof.clone());
        if !accepted {
            panic!(
                "CRITICAL: application rejected verified equivocation evidence for offender {}",
                hash_short(&proof.offender)
            );
        }
        info!(
            view = proof.view,
            offender = %hash_short(&proof.offender),
            "Equivocation evidence verified and queued for proposal"
        );

        if let Err(error) = self
            .network
            .broadcast(&Message::EquivocationEvidence(proof))
            .await
        {
            warn!(error = %error, "Failed to broadcast local equivocation evidence; retaining journal row");
        }
    }

    /// Admit evidence received from a peer after transport authentication.
    /// The transport intentionally does not relay evidence: this node must
    /// durably journal it before it can become a new propagation source.
    async fn handle_inbound_equivocation(&mut self, proof: EquivocationProof) -> Result<()> {
        let proof = match proof.canonicalized() {
            Ok(proof) => proof,
            Err(error) => {
                warn!(error = %error, "Dropping non-canonical inbound equivocation evidence");
                return Ok(());
            }
        };
        if let Err(error) = self.verify_equivocation_proof(&proof) {
            warn!(error = %error, "Dropping invalid inbound equivocation evidence");
            return Ok(());
        }
        let Some(store) = self.persistent_store.clone() else {
            warn!(
                "Dropping inbound equivocation evidence because the durable journal is unavailable"
            );
            return Ok(());
        };
        store.save_equivocation_proof(&proof)?;
        if !self.app.submit_equivocation_evidence(proof.clone()) {
            warn!(
                offender = %hash_short(&proof.offender),
                "Application rejected inbound verified equivocation evidence; retaining journal row"
            );
        }

        // In gossip mode this is the first relay point after the durable
        // journal write above.  Direct-broadcast development mode already
        // sends the origin's message to every connected peer; rebroadcasting
        // there would make each peer echo the same evidence indefinitely.
        if self.network.gossip_enabled() {
            if let Err(error) = self
                .network
                .broadcast(&Message::EquivocationEvidence(proof))
                .await
            {
                warn!(
                    %error,
                    "Failed to relay inbound equivocation evidence; retaining journal row"
                );
            }
        }
        Ok(())
    }

    async fn handle_inbound_equivocation_or_fail_stop(&mut self, proof: EquivocationProof) {
        if let Err(error) = self.handle_inbound_equivocation(proof).await {
            panic!("CRITICAL: failed to persist inbound equivocation evidence: {error}");
        }
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
        while timestamps
            .front()
            .map(|t| *t < one_second_ago)
            .unwrap_or(false)
        {
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
        self.votes
            .retain(|_, votes| votes.first().map(|v| v.view >= min_view).unwrap_or(false));
    }

    // =========================================================================
    // Block Sync Protocol
    // =========================================================================

    /// Handle incoming SyncRequest from a peer
    async fn handle_sync_request(
        &self,
        from: crate::types::NodeId,
        req: crate::types::SyncRequest,
    ) {
        if let Some(ref handler) = self.sync_handler {
            let response = handler.handle_sync_request(req);
            debug!(
                from = %hash_short(&from),
                blocks = response.blocks.len(),
                "Responding to sync request"
            );
            if let Err(e) = self
                .network
                .send_to(from, &Message::SyncResponse(response))
                .await
            {
                warn!(error = %e, "Failed to send sync response");
            }
        }
    }

    /// Handle incoming SyncResponse from a peer
    async fn handle_sync_response(&mut self, response: crate::types::SyncResponse) {
        if response.blocks.is_empty() {
            self.syncing = false;
            return;
        }

        warn!(
            blocks = response.blocks.len(),
            peer_height = response.peer_height,
            "Rejecting unverified sync response; consensus proof import is disabled"
        );
        self.syncing = false;
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
                self.network
                    .send_to(*validator, &Message::SyncRequest(req))
                    .await?;
                break;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::api::{ApiSecurityPolicy, ApiState, CanonicalAppHook, SharedState};
    use crate::app::candles::Candle;
    use crate::app::{AppState, ConsensusTransaction, SignedEnvelope, Transaction};
    use crate::config::Mode;
    use crate::consensus::{AppHook, BlockStore, EquivocationProof, MemoryBlockStore, NoOpApp};
    use crate::consensus::{EpochTransitionActivation, EpochTransitionProof, StateRootReference};
    use crate::crypto::bls::BlsSecretKey;
    use crate::crypto::Signer;
    use crate::network::{GossipValidationConfig, NetworkConfig, TcpNetwork};
    use crate::storage::{
        AppSnapshot, ConsensusState, EquivocationJournalCapability, PersistentStore, RocksDbStore,
    };
    use crate::types::{
        Block, Certificate, CommitmentV2, ConsensusConfig, ConsensusContext, Hash, Propose,
        TransactionReceipt, TransactionType, Vote,
    };

    use super::{
        recovery_resume_view, should_emit_view_change_after_timeout_certificate,
        should_rebroadcast_equivocation_batch, verify_high_qc_against_block, ConsensusRunner,
        MAX_PENDING_BLOCKS, MAX_PENDING_BYTES, MAX_PENDING_DEPTH, MAX_PENDING_SOFT_BLOCKS,
        MAX_PENDING_SOFT_BYTES,
    };

    struct RecordingStore {
        inner: RocksDbStore,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_consensus_state: bool,
        fail_commit_block: Arc<std::sync::atomic::AtomicBool>,
    }

    struct CountingApp {
        execute_calls: Arc<AtomicUsize>,
        commit_calls: Arc<AtomicUsize>,
        execute_parents: Arc<Mutex<Vec<Hash>>>,
    }

    impl AppHook for CountingApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            self.execute_parents.lock().unwrap().push(block.parent);
            block.app_hash
        }

        fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some(block.app_hash))
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Ok(Some(CommitmentV2::default()))
        }

        fn commit(&mut self, block: &Block) -> Result<Hash, String> {
            self.commit_calls.fetch_add(1, Ordering::SeqCst);
            Ok(block.app_hash)
        }
    }

    impl BlockStore for RecordingStore {
        fn save(&self, block: &Block) {
            self.inner.save(block);
        }

        fn save_speculative(&self, block: &Block) -> anyhow::Result<()> {
            self.inner.save_speculative(block)
        }

        fn admit_speculative_with_rolling_victim(
            &self,
            block: &Block,
            protected_roots: &[Hash],
            max_blocks: usize,
            max_bytes: usize,
        ) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("save_block");
            self.inner.admit_speculative_with_rolling_victim(
                block,
                protected_roots,
                max_blocks,
                max_bytes,
            )
        }

        fn prune_speculative(
            &self,
            protected_roots: &[Hash],
            max_blocks: usize,
            max_bytes: usize,
        ) -> anyhow::Result<()> {
            self.inner
                .prune_speculative(protected_roots, max_blocks, max_bytes)
        }

        fn get(&self, hash: &Hash) -> Option<Block> {
            self.inner.get(hash)
        }

        fn get_by_height(&self, height: u64) -> Option<Block> {
            self.inner.get_by_height(height)
        }

        fn set_committed(&self, hash: &Hash) {
            self.inner.set_committed(hash);
        }

        fn get_committed_head(&self) -> Option<Block> {
            self.inner.get_committed_head()
        }
    }

    impl PersistentStore for RecordingStore {
        fn equivocation_journal_capability(&self) -> EquivocationJournalCapability {
            if self
                .events
                .lock()
                .expect("recording events lock")
                .contains(&"fail_equivocation_journal")
            {
                EquivocationJournalCapability::unsupported()
            } else {
                EquivocationJournalCapability::supported()
            }
        }

        fn save_block(&self, block: &Block) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("save_block");
            self.inner.save_block(block)
        }

        fn save_consensus_state(&self, state: &ConsensusState) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("save_consensus_state");
            if self.fail_consensus_state {
                anyhow::bail!("injected consensus-state write failure");
            }
            self.inner.save_consensus_state(state)
        }

        fn load_consensus_state(&self) -> anyhow::Result<Option<ConsensusState>> {
            self.inner.load_consensus_state()
        }

        fn save_equivocation_proof(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("save_equivocation");
            self.inner.save_equivocation_proof(proof)
        }

        fn load_equivocation_proofs(&self) -> anyhow::Result<Vec<EquivocationProof>> {
            if self
                .events
                .lock()
                .expect("recording events lock")
                .contains(&"fail_equivocation_journal")
            {
                anyhow::bail!("equivocation proof journal is not supported by this store");
            }
            self.inner.load_equivocation_proofs()
        }

        fn delete_equivocation_proof(&self, proof: &EquivocationProof) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("delete_equivocation");
            self.inner.delete_equivocation_proof(proof)
        }

        fn save_snapshot(&self, height: u64, snapshot: &AppSnapshot) -> anyhow::Result<()> {
            self.inner.save_snapshot(height, snapshot)
        }

        fn load_latest_snapshot(
            &self,
            before_height: u64,
        ) -> anyhow::Result<Option<(u64, AppSnapshot)>> {
            self.inner.load_latest_snapshot(before_height)
        }

        fn load_latest_snapshot_height(&self, before_height: u64) -> anyhow::Result<Option<u64>> {
            self.inner.load_latest_snapshot_height(before_height)
        }

        fn blocks_from_height(&self, from_height: u64) -> anyhow::Result<Vec<Block>> {
            self.inner.blocks_from_height(from_height)
        }

        fn commit_block(&self, block: &Block, state: &ConsensusState) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("commit_block");
            if self.fail_commit_block.load(Ordering::SeqCst) {
                anyhow::bail!("injected finalized commit failure");
            }
            self.inner.commit_block(block, state)
        }

        fn commit_block_with_artifacts(
            &self,
            block: &Block,
            state: &ConsensusState,
            artifacts: Option<&[u8]>,
        ) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("commit_block");
            if self.fail_commit_block.load(Ordering::SeqCst) {
                anyhow::bail!("injected finalized commit failure");
            }
            self.inner
                .commit_block_with_artifacts(block, state, artifacts)
        }

        fn commit_block_with_commitment_and_state_root(
            &self,
            block: &Block,
            state: &ConsensusState,
            commitment: Option<&CommitmentV2>,
            state_root: Option<&Hash>,
        ) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("commit_block");
            if self.fail_commit_block.load(Ordering::SeqCst) {
                anyhow::bail!("injected finalized commit failure");
            }
            self.inner
                .commit_block_with_commitment_and_state_root(block, state, commitment, state_root)
        }

        fn load_block_artifacts(&self, hash: &Hash) -> anyhow::Result<Option<Vec<u8>>> {
            self.inner.load_block_artifacts(hash)
        }

        fn load_state_root(&self, hash: &Hash) -> anyhow::Result<Option<Hash>> {
            self.inner.load_state_root(hash)
        }

        fn save_candles_batch(&self, entries: &[(Vec<u8>, Vec<u8>)]) -> anyhow::Result<()> {
            self.inner.save_candles_batch(entries)
        }

        fn load_candles(
            &self,
            symbol: &str,
            interval_str: &str,
            limit: usize,
        ) -> anyhow::Result<Vec<Candle>> {
            self.inner.load_candles(symbol, interval_str, limit)
        }
    }

    struct LifecycleApp {
        commit_calls: Arc<AtomicUsize>,
        order: Arc<Mutex<Vec<&'static str>>>,
    }

    struct EvidenceLifecycleApp {
        order: Arc<Mutex<Vec<&'static str>>>,
        commitment: CommitmentV2,
    }

    struct CommitmentLifecycleApp {
        order: Arc<Mutex<Vec<&'static str>>>,
        commitment: CommitmentV2,
    }

    struct StateRootLifecycleApp {
        order: Arc<Mutex<Vec<&'static str>>>,
        commitment: CommitmentV2,
        state_root: Hash,
    }

    impl AppHook for LifecycleApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some(block.app_hash))
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Ok(Some(CommitmentV2::default()))
        }

        fn commit(&mut self, block: &Block) -> Result<Hash, String> {
            self.commit_calls.fetch_add(1, Ordering::SeqCst);
            self.order.lock().unwrap().push("app_commit");
            Ok(block.app_hash)
        }
    }

    impl AppHook for EvidenceLifecycleApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Ok(Some(self.commitment.clone()))
        }

        fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some(block.app_hash))
        }

        fn submit_equivocation_evidence(&mut self, _proof: EquivocationProof) -> bool {
            self.order.lock().unwrap().push("app_enqueue");
            true
        }

        fn commit(&mut self, block: &Block) -> Result<Hash, String> {
            self.order.lock().unwrap().push("app_commit");
            Ok(block.app_hash)
        }
    }

    impl AppHook for CommitmentLifecycleApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some(block.app_hash))
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            self.order.lock().unwrap().push("app_preflight");
            Ok(Some(self.commitment.clone()))
        }

        fn commit(&mut self, block: &Block) -> Result<Hash, String> {
            self.order.lock().unwrap().push("app_commit");
            Ok(block.app_hash)
        }
    }

    impl AppHook for StateRootLifecycleApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            self.order.lock().unwrap().push("app_preflight");
            Ok(Some(self.commitment.clone()))
        }

        fn preflight_state_root(&self, _block: &Block) -> Result<Option<Hash>, String> {
            self.order.lock().unwrap().push("app_state_root");
            Ok(Some(self.state_root))
        }

        fn commit(&mut self, block: &Block) -> Result<Hash, String> {
            self.order.lock().unwrap().push("app_commit");
            Ok(block.app_hash)
        }
    }

    fn durable_test_config(label: &str) -> ConsensusConfig {
        let mut config = ConsensusConfig::single_node();
        let committee = config.committee().expect("single-node committee");
        config.genesis_hash = crate::types::genesis_domain_hash(
            label,
            config.epoch,
            config.view_timeout_ms,
            committee.hash(),
        );
        config
    }

    fn live_test_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock after Unix epoch")
            .as_millis() as u64
    }

    fn test_propose(config: &ConsensusConfig, view: u64) -> Propose {
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: live_test_timestamp(),
            justify: None,
        };
        let mut propose = Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block,
            justify: None,
            proposer_signature: Vec::new(),
        };
        let secret = config.bls_secret_key().expect("test BLS key");
        propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();
        propose
    }

    #[tokio::test]
    async fn stale_live_proposal_rejects_before_vote_or_persistence() {
        let config = durable_test_config("stale-live-timestamp");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");

        let mut propose = test_propose(&config, 1);
        propose.block.timestamp = live_test_timestamp()
            .saturating_sub(crate::types::MAX_BLOCK_PAST_DRIFT_MS)
            .saturating_sub(1);
        let secret = config.bls_secret_key().expect("test BLS key");
        propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();
        let block = propose.block.clone();
        let block_hash = block.hash();

        assert!(runner.process_proposal(propose).is_none());
        assert!(runner.pending.is_empty());
        assert!(runner.store.get(&block_hash).is_none());
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());

        let parent = block;
        runner
            .store
            .save_speculative(&parent)
            .expect("store old parent body");
        runner.pending.insert(parent.hash(), parent.clone());
        let context = config.context().expect("test context");
        let committee = config.committee().expect("test committee");
        let parent_qc = super::form_certificate(
            &committee,
            context,
            vec![Vote::new_bls(
                context,
                parent.view,
                parent.hash(),
                parent.app_hash,
                config.node_id,
                &secret,
            )],
            true,
        )
        .expect("single-validator parent QC");
        let child = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: parent.view + 1,
            height: parent.height + 1,
            parent: parent.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: parent.timestamp,
            justify: Some(parent_qc.clone()),
        };
        let mut resumed = Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block: child,
            justify: Some(parent_qc),
            proposer_signature: Vec::new(),
        };
        resumed.proposer_signature = secret.sign(&resumed.signing_data()).to_bytes().to_vec();
        assert!(runner.process_proposal(resumed).is_some());
    }

    fn test_equivocation_proof(config: &ConsensusConfig) -> EquivocationProof {
        let context = config.context().expect("test context");
        let secret = config.bls_secret_key().expect("test BLS key");
        let vote_a = Vote::new_bls(context, 4, [1u8; 32], [11u8; 32], config.node_id, &secret);
        let vote_b = Vote::new_bls(context, 4, [2u8; 32], [12u8; 32], config.node_id, &secret);
        EquivocationProof {
            context,
            offender: config.node_id,
            view: 4,
            hash_a: vote_a.block_hash,
            app_hash_a: vote_a.app_hash,
            hash_b: vote_b.block_hash,
            app_hash_b: vote_b.app_hash,
            signature_a: vote_a.signature,
            signature_b: vote_b.signature,
        }
    }

    fn single_node_network_config(config: &ConsensusConfig) -> NetworkConfig {
        let secret_key = config
            .bls_secret_key()
            .expect("single-node test config has a BLS key");
        let public_key = secret_key.public_key();
        NetworkConfig {
            node_id: config.node_id,
            listen_addr: "127.0.0.1:0".to_string(),
            peers: vec![],
            require_authenticated_peers: true,
            bls_secret_key: Some(secret_key),
            validator_pubkeys: HashMap::from([(config.node_id, public_key)]),
            gossip_validation: Some(GossipValidationConfig {
                context: config.context().expect("single-node context"),
                committee: config.committee().expect("single-node committee"),
                allow_dev_envelopes: false,
            }),
        }
    }

    async fn unused_loopback_addr() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind temporary loopback listener");
        listener
            .local_addr()
            .expect("temporary listener has an address")
            .to_string()
    }

    #[tokio::test]
    async fn non_leader_api_transaction_reaches_leader_payload_over_live_transport() {
        let source_id = [1u8; 32];
        let leader_id = [2u8; 32];
        let source_seed = [41u8; 32];
        let leader_seed = [42u8; 32];
        let source_secret = BlsSecretKey::from_seed(&source_seed);
        let leader_secret = BlsSecretKey::from_seed(&leader_seed);
        let validators = vec![source_id, leader_id];
        let voting_powers = vec![1, 1];
        let bls_pubkeys = vec![
            source_secret.public_key().to_bytes().to_vec(),
            leader_secret.public_key().to_bytes().to_vec(),
        ];
        let committee_probe = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: source_id,
            validators: validators.clone(),
            voting_powers: voting_powers.clone(),
            view_timeout_ms: 1_000,
            bls_pubkeys: bls_pubkeys.clone(),
            bls_secret_key: Some(source_seed),
        };
        let genesis_hash = crate::types::genesis_domain_hash(
            "tx-gossip-integration",
            0,
            1_000,
            committee_probe
                .committee()
                .expect("two-validator committee")
                .hash(),
        );
        let source_consensus = ConsensusConfig {
            genesis_hash,
            ..committee_probe
        };
        let leader_consensus = ConsensusConfig {
            epoch: 0,
            genesis_hash,
            node_id: leader_id,
            validators,
            voting_powers,
            view_timeout_ms: 1_000,
            bls_pubkeys,
            bls_secret_key: Some(leader_seed),
        };
        let context = source_consensus.context().expect("shared context");
        assert_eq!(leader_consensus.context().expect("leader context"), context);
        let committee = source_consensus.committee().expect("shared committee");
        assert_eq!(
            committee.leader(0),
            leader_id,
            "the receiving node must be the scheduled leader for the tested view"
        );
        let validator_pubkeys = HashMap::from([
            (source_id, source_secret.public_key()),
            (leader_id, leader_secret.public_key()),
        ]);
        let source_addr = unused_loopback_addr().await;
        let leader_addr = unused_loopback_addr().await;

        let source_network = TcpNetwork::new(NetworkConfig {
            node_id: source_id,
            listen_addr: source_addr.clone(),
            peers: vec![(leader_id, leader_addr.clone())],
            require_authenticated_peers: true,
            bls_secret_key: Some(source_secret),
            validator_pubkeys: validator_pubkeys.clone(),
            gossip_validation: Some(GossipValidationConfig {
                context,
                committee: committee.clone(),
                allow_dev_envelopes: false,
            }),
        })
        .await
        .expect("construct source transport");
        let mut leader_network = TcpNetwork::new(NetworkConfig {
            node_id: leader_id,
            listen_addr: leader_addr,
            peers: vec![(source_id, source_addr)],
            require_authenticated_peers: true,
            bls_secret_key: Some(leader_secret),
            validator_pubkeys,
            gossip_validation: Some(GossipValidationConfig {
                context,
                committee,
                allow_dev_envelopes: false,
            }),
        })
        .await
        .expect("construct leader transport");
        source_network
            .start()
            .await
            .expect("start source transport");
        leader_network
            .start()
            .await
            .expect("start leader transport");
        source_network
            .wait_for_peers(Duration::from_secs(2))
            .await
            .expect("source sees authenticated leader");
        leader_network
            .wait_for_peers(Duration::from_secs(2))
            .await
            .expect("leader sees authenticated source");

        let mut source_app = AppState::new_with_chain_domain_and_dev(context.genesis_hash, false);
        source_app.set_consensus_context(context);
        let source_shared = SharedState::new(source_app);
        source_shared
            .set_user_transaction_publisher(Arc::new(source_network.transaction_broadcaster()));
        let api = ApiState::with_policy(source_shared, ApiSecurityPolicy::new(Mode::Dev, false));
        let signer = Signer::from_bytes(&[77u8; 32]).expect("user signer");
        let trader = format!("0x{}", hex::encode(signer.address().into_array()));
        let envelope = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            u64::MAX,
            Transaction::Deposit { trader, amount: 10 },
        )
        .expect("canonical signed transaction");
        let expected_hash = envelope.hash().expect("transaction hash");
        assert_eq!(
            api.submit_user_envelope(envelope.clone(), live_test_timestamp())
                .await
                .expect("non-leader API admission"),
            expected_hash
        );

        let (from, message) =
            tokio::time::timeout(Duration::from_secs(2), leader_network.recv_msg())
                .await
                .expect("leader receives transaction before timeout")
                .expect("leader transport receive succeeds");
        let received = match message {
            crate::types::Message::UserTransaction(envelope) => envelope,
            other => panic!("expected user transaction, got {other:?}"),
        };
        assert_eq!(from, source_id);
        assert_eq!(
            received.hash().expect("received transaction hash"),
            expected_hash
        );

        let mut leader_app = AppState::new_with_chain_domain_and_dev(context.genesis_hash, false);
        leader_app.set_consensus_context(context);
        let leader_shared = SharedState::new(leader_app);
        let mut runner = ConsensusRunner::new(leader_consensus, leader_network)
            .await
            .expect("construct leader runner")
            .with_app(CanonicalAppHook::new(leader_shared));
        runner.handle_user_transaction(from, received).await;

        let payload = runner.app.prepare_payload(&Block::genesis(context));
        let entries: Vec<ConsensusTransaction> =
            bincode::deserialize(&payload).expect("leader payload is canonical transaction list");
        assert_eq!(entries.len(), 1);
        match &entries[0] {
            ConsensusTransaction::Signed(included) => assert_eq!(
                included.hash().expect("included transaction hash"),
                expected_hash
            ),
            ConsensusTransaction::System(_) => {
                panic!("user transaction must not enter the system transaction path")
            }
        }
    }

    #[tokio::test]
    async fn follower_persists_block_then_vote_intent_before_returning_vote() {
        let config = durable_test_config("follower-vote-order");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: events.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");
        runner.persistent_store = Some(store.clone());

        let propose = test_propose(&config, 1);
        let block = propose.block.clone();
        let vote = runner
            .process_proposal(propose)
            .expect("valid proposal should produce a vote");

        assert_eq!(vote.view, 1);
        assert_eq!(
            &*events.lock().unwrap(),
            &["save_block", "save_consensus_state"]
        );
        let persisted = store
            .load_consensus_state()
            .expect("load vote intent")
            .expect("vote intent must be durable");
        assert!(persisted.voted_views.contains(&1));
        assert!(matches!(
            runner.safety.safe_to_vote(&block, [0u8; 32]),
            Err(super::super::safety::SafetyError::AlreadyVoted(1))
        ));
    }

    #[tokio::test]
    async fn recovery_rejects_a_staged_epoch_transition_marker() {
        let config = durable_test_config("staged-transition-recovery");
        let context = config.context().expect("test context");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store = Arc::new(RocksDbStore::open(temp_dir.path()).expect("open test store"));
        let marker = EpochTransitionProof {
            schema_version: crate::consensus::EPOCH_TRANSITION_PROOF_SCHEMA_VERSION,
            activation: EpochTransitionActivation::StagedOnly,
            old_context: context,
            old_qc: Certificate {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: 1,
                block_hash: [1u8; 32],
                app_hash: Some([2u8; 32]),
                votes: Vec::new(),
                voters: Vec::new(),
                bls_pubkeys: Vec::new(),
                agg_signature: Vec::new(),
            },
            next_epoch: 1,
            next_committee: Vec::new(),
            next_committee_hash: [3u8; 32],
            effective_height: 2,
            state_root: StateRootReference::new(0, [4u8; 32]),
        };
        store
            .save_epoch_transition_proof(&marker)
            .expect("stage marker");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");

        let error = match ConsensusRunner::new_with_recovery(config, network, store).await {
            Ok(_) => panic!("restart must not auto-activate a staged marker"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("staged epoch transition; runtime activation is not enabled"));
    }

    #[tokio::test]
    async fn equivocation_journal_is_saved_before_enqueue_and_reloaded_after_restart() {
        let config = durable_test_config("equivocation-journal-restart");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct recoverable runner")
            .with_app(EvidenceLifecycleApp {
                order: order.clone(),
                commitment: CommitmentV2::default(),
            });
        order.lock().unwrap().clear();

        let proof = test_equivocation_proof(&config);
        runner.handle_equivocation(proof.clone()).await;
        assert_eq!(
            &*order.lock().unwrap(),
            &["save_equivocation", "app_enqueue"]
        );
        assert_eq!(
            store
                .load_equivocation_proofs()
                .expect("load journal")
                .len(),
            1
        );
        drop(runner);

        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated restart network");
        let mut recovered = ConsensusRunner::new_with_recovery(config, network, store.clone())
            .await
            .expect("recover runner after restart")
            .with_app(EvidenceLifecycleApp {
                order: order.clone(),
                commitment: CommitmentV2::default(),
            });
        order.lock().unwrap().clear();
        recovered
            .initialize_live()
            .expect("recovery must re-enqueue pending evidence");
        assert_eq!(&*order.lock().unwrap(), &["app_enqueue"]);
        assert_eq!(
            store
                .load_equivocation_proofs()
                .expect("load retained journal")
                .len(),
            1
        );
        // The application was already given the pending proof during
        // initialize_live; the durable row remains for retry until commit.
        assert!(recovered.reconciled_after_recovery);
    }

    #[tokio::test]
    async fn committed_evidence_is_deleted_after_durable_commit_and_app_commit() {
        let config = durable_test_config("equivocation-journal-gc");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let proof = test_equivocation_proof(&config);
        let context = config.context().expect("test context");
        let evidence = crate::app::staking::Evidence {
            evidence_type: crate::app::staking::EvidenceType::DoubleVote,
            offender: proof.offender,
            view: proof.view,
            timestamp: 0,
            context: proof.context,
            hash_a: proof.hash_a,
            app_hash_a: proof.app_hash_a,
            hash_b: proof.hash_b,
            app_hash_b: proof.app_hash_b,
            signature_a: proof.signature_a.clone(),
            signature_b: proof.signature_b.clone(),
        };
        let evidence_transaction = ConsensusTransaction::System(Transaction::SubmitEvidence {
            submitter: format!("system:equivocation:{}", hex::encode(proof.offender)),
            evidence,
        });
        let evidence_receipt = TransactionReceipt::success(
            0,
            evidence_transaction
                .hash()
                .expect("hash evidence transaction"),
            TransactionType::SUBMIT_EVIDENCE,
            Default::default(),
            Vec::new(),
        )
        .expect("valid evidence receipt");
        let commitment = CommitmentV2::new(vec![evidence_receipt]).expect("evidence commitment");
        let payload =
            bincode::serialize(&vec![evidence_transaction]).expect("encode evidence payload");
        let genesis = Block::genesis(context);
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload,
            proposer: config.node_id,
            commitment_root: commitment.root().expect("commitment root"),
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct recoverable runner")
            .with_app(EvidenceLifecycleApp {
                order: order.clone(),
                commitment: commitment.clone(),
            });
        store
            .save_equivocation_proof(&proof)
            .expect("save pending evidence");
        order.lock().unwrap().clear();
        runner.store.save(&block);
        assert!(runner.try_commit(&block.hash()).is_some());
        assert_eq!(
            &*order.lock().unwrap(),
            &["commit_block", "app_commit", "delete_equivocation"]
        );
        assert!(store
            .load_equivocation_proofs()
            .expect("load post-commit journal")
            .is_empty());
    }

    #[test]
    fn production_memory_cache_keeps_exact_head_without_canonical_history_growth() {
        let context = ConsensusConfig::single_node()
            .context()
            .expect("single-node context");
        let store = MemoryBlockStore::new();
        let genesis = Block::genesis(context);
        store.save(&genesis);
        store.set_committed(&genesis.hash());

        let speculative = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [1u8; 32],
            commitment_root: [1u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save_speculative(&speculative).unwrap();
        let mut larger_justify = speculative.clone();
        larger_justify.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            block_hash: genesis.hash(),
            app_hash: Some(genesis.app_hash),
            votes: Vec::new(),
            voters: vec![[9u8; 32]; 8],
            bls_pubkeys: vec![vec![7u8; 48]; 8],
            agg_signature: vec![5u8; 96],
        });
        assert_eq!(larger_justify.hash(), speculative.hash());
        store.save_speculative(&larger_justify).unwrap();
        assert!(store
            .get(&speculative.hash())
            .expect("speculative body")
            .justify
            .is_none());
        store.save(&speculative);
        store.save_speculative(&larger_justify).unwrap();
        assert!(store.get(&speculative.hash()).unwrap().justify.is_none());
        assert_eq!(store.get_by_height(1).unwrap().hash(), speculative.hash());

        let mut parent = genesis;
        let mut old_hashes = Vec::new();
        for height in 1..=8 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: height,
                height,
                parent: parent.hash(),
                payload: Vec::new(),
                proposer: [height as u8; 32],
                commitment_root: [0u8; 32],
                app_hash: [height as u8; 32],
                timestamp: height,
                justify: None,
            };
            old_hashes.push(parent.hash());
            store.save(&block);
            store.set_committed(&block.hash());
            store.prune_production_cache(&[]).unwrap();
            assert_eq!(store.get_committed_head().unwrap().hash(), block.hash());
            parent = block;
        }

        for old_hash in old_hashes {
            assert!(
                store.get(&old_hash).is_none(),
                "old canonical block remained in the production cache"
            );
        }
        assert_eq!(
            store.get_by_height(parent.height).unwrap().hash(),
            parent.hash()
        );
    }

    #[tokio::test]
    async fn delayed_qc_rehydrates_evicted_canonical_candidate_from_store_body() {
        let config = durable_test_config("delayed-qc-candidate-replay");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut app = CanonicalAppHook::new(SharedState::new(
            crate::app::AppState::new_with_chain_domain(context.genesis_hash),
        ));
        let genesis = Block::genesis(context);

        let finalize = |app: &mut CanonicalAppHook, mut block: Block| {
            block.app_hash = app.execute(&block);
            let commitment = app
                .derive_execution_commitment(&block)
                .expect("execution commitment preflight")
                .expect("execution commitment");
            block.commitment_root = commitment.root().expect("execution commitment root");
            app.seal_execution_commitment(&block)
                .expect("execution commitment seal");
            block
        };

        let first = finalize(
            &mut app,
            Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: 1,
                height: 1,
                parent: genesis.hash(),
                payload: Vec::new(),
                proposer: [1u8; 32],
                commitment_root: [0u8; 32],
                app_hash: [0u8; 32],
                timestamp: 1,
                justify: None,
            },
        );
        let child = finalize(
            &mut app,
            Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: 2,
                height: 2,
                parent: first.hash(),
                payload: Vec::new(),
                proposer: [2u8; 32],
                commitment_root: [0u8; 32],
                app_hash: [0u8; 32],
                timestamp: 2,
                justify: None,
            },
        );

        let candidate_cap = 16usize;
        let mut siblings = vec![first.clone(), child.clone()];
        for index in 0..(candidate_cap - siblings.len()) {
            siblings.push(finalize(
                &mut app,
                Block {
                    epoch: context.epoch,
                    committee_hash: context.committee_hash,
                    genesis_hash: context.genesis_hash,
                    view: 100 + index as u64,
                    height: 1,
                    parent: genesis.hash(),
                    payload: Vec::new(),
                    proposer: [(index + 3) as u8; 32],
                    commitment_root: [0u8; 32],
                    app_hash: [0u8; 32],
                    timestamp: 100 + index as u64,
                    justify: None,
                },
            ));
        }
        assert_eq!(app.candidate_count(), candidate_cap);

        // The candidate snapshots are full application clones and are
        // evicted at the fixed cap. Their corresponding block bodies remain
        // available in the runner's speculative journal below.
        app.prune_speculative_branches(&[genesis.hash()]);
        assert_eq!(app.candidate_count(), 0);

        let mut runner = ConsensusRunner::new(config, network)
            .await
            .expect("construct test runner")
            .with_app(app);
        for block in &siblings {
            runner
                .store
                .save_speculative(block)
                .expect("retain delayed-QC block body");
        }
        let stored_hashes: Vec<Hash> = siblings.iter().map(Block::hash).collect();

        // A full candidate cache released all currently unprotected snapshots,
        // but the store journal remains the replay source.
        for hash in &stored_hashes {
            assert!(runner.store.get(hash).is_some());
        }

        let malformed = Block {
            app_hash: [9u8; 32],
            view: 900,
            timestamp: 900,
            ..first.clone()
        };
        assert!(runner.preflight_application_branch(&malformed).is_err());
        assert!(runner.pending.is_empty());
        for hash in &stored_hashes {
            assert!(runner.store.get(hash).is_some());
        }

        let (committed_head, ancestors) = runner
            .preflight_application_branch(&child)
            .expect("delayed QC branch must replay from the stored body");
        assert_eq!(committed_head.hash(), genesis.hash());
        assert_eq!(ancestors.len(), 1);
        assert_eq!(ancestors[0].hash(), first.hash());
        runner
            .app
            .restore_speculative_branch(context, &committed_head, &ancestors)
            .expect("replayed branch must restore into the live app");
        runner
            .app
            .validate_block(&child)
            .expect("rehydrated parent must validate the child");
        assert_eq!(runner.app.execute(&child), child.app_hash);
        assert!(runner.app.preflight_commitment(&child).is_ok());
        assert_eq!(runner.pending.len(), 0);
    }

    #[tokio::test]
    async fn max_depth_delayed_qc_admission_replaces_unprotected_candidate() {
        let config = durable_test_config("max-depth-delayed-qc-admission");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let genesis = Block::genesis(context);
        let live_timestamp = live_test_timestamp();
        let finalize = |app: &mut CanonicalAppHook, mut block: Block| {
            block.app_hash = app.execute(&block);
            let commitment = app
                .derive_execution_commitment(&block)
                .expect("execution commitment preflight")
                .expect("execution commitment");
            block.commitment_root = commitment.root().expect("execution commitment root");
            app.seal_execution_commitment(&block)
                .expect("execution commitment seal");
            block
        };
        let make_block = |view: u64, height: u64, parent: Hash| Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent,
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: live_timestamp,
            justify: None,
        };

        // The live application retains an unrelated high/locked-QC fork with
        // two deep-cloned candidates. The delayed higher-QC branch is
        // represented only by its durable block bodies.
        let mut live = CanonicalAppHook::new(SharedState::new(
            crate::app::AppState::new_with_chain_domain(context.genesis_hash),
        ));
        let high_qc_block = finalize(&mut live, make_block(1, 1, genesis.hash()));
        let locked_qc_block = finalize(&mut live, make_block(2, 2, high_qc_block.hash()));

        let mut source = CanonicalAppHook::new(SharedState::new(
            crate::app::AppState::new_with_chain_domain(context.genesis_hash),
        ));
        let mut parent = genesis.clone();
        let mut delayed_branch = Vec::new();
        for height in 1..=15 {
            let block = finalize(&mut source, make_block(100 + height, height, parent.hash()));
            parent = block.clone();
            delayed_branch.push(block);
        }
        let delayed_child = finalize(&mut source, make_block(116, 16, parent.hash()));

        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(live);
        runner
            .store
            .save_speculative(&high_qc_block)
            .expect("store current high-QC body");
        runner
            .store
            .save_speculative(&locked_qc_block)
            .expect("store current locked-QC body");
        for block in &delayed_branch {
            runner
                .store
                .save_speculative(block)
                .expect("store delayed branch body");
        }
        runner
            .store
            .save_speculative(&delayed_child)
            .expect("store delayed child body");

        let secret = config.bls_secret_key().expect("test BLS key");
        let committee = config.committee().expect("test committee");
        let locked_qc = super::form_certificate(
            &committee,
            context,
            vec![Vote::new_bls(
                context,
                high_qc_block.view,
                high_qc_block.hash(),
                high_qc_block.app_hash,
                config.node_id,
                &secret,
            )],
            true,
        )
        .expect("current one-node locked QC");
        let high_qc = super::form_certificate(
            &committee,
            context,
            vec![Vote::new_bls(
                context,
                locked_qc_block.view,
                locked_qc_block.hash(),
                locked_qc_block.app_hash,
                config.node_id,
                &secret,
            )],
            true,
        )
        .expect("current one-node high QC");
        runner.safety.update_high_qc(high_qc);
        runner.safety.update_locked_qc(locked_qc);
        let delayed_parent = delayed_branch.last().expect("delayed parent");
        let delayed_qc = super::form_certificate(
            &committee,
            context,
            vec![Vote::new_bls(
                context,
                delayed_parent.view,
                delayed_parent.hash(),
                delayed_parent.app_hash,
                config.node_id,
                &secret,
            )],
            true,
        )
        .expect("higher delayed one-node QC");
        let mut propose = Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block: Block {
                justify: Some(delayed_qc.clone()),
                ..delayed_child
            },
            justify: Some(delayed_qc),
            proposer_signature: Vec::new(),
        };
        propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();

        let vote = runner
            .process_proposal(propose)
            .expect("max-depth delayed branch should evict only unprotected app candidate");
        assert_eq!(vote.view, 116);
        assert_eq!(runner.pending.len(), 1);
    }

    #[tokio::test]
    async fn higher_justify_qc_is_applied_before_vote_safety_check() {
        let config = durable_test_config("prospective-justify-safety");
        let context = config.context().expect("test context");
        let committee = config.committee().expect("test committee");
        let secret = config.bls_secret_key().expect("test BLS key");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(NoOpApp);
        let genesis = Block::genesis(context);
        let commitment_root = CommitmentV2::default()
            .root()
            .expect("empty commitment root");
        let live_timestamp = live_test_timestamp();
        let branch_block = |view: u64, height: u64, parent: Hash| Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent,
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root,
            app_hash: [0u8; 32],
            timestamp: live_timestamp,
            justify: None,
        };
        let old = branch_block(1, 1, genesis.hash());
        let new_parent = branch_block(2, 1, genesis.hash());
        let new_qc_parent = branch_block(3, 2, new_parent.hash());
        for block in [&old, &new_parent, &new_qc_parent] {
            runner
                .store
                .save_speculative(block)
                .expect("store QC branch body");
        }

        let make_qc = |block: &Block| {
            let vote = Vote::new_bls(
                context,
                block.view,
                block.hash(),
                block.app_hash,
                config.node_id,
                &secret,
            );
            super::form_certificate(&committee, context, vec![vote], true)
                .expect("one-node QC should form")
        };
        let old_qc = make_qc(&old);
        let new_qc = make_qc(&new_qc_parent);
        runner.safety.update_high_qc(old_qc);

        let child = branch_block(4, 3, new_qc_parent.hash());
        let mut propose = Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block: Block {
                justify: Some(new_qc.clone()),
                ..child
            },
            justify: Some(new_qc),
            proposer_signature: Vec::new(),
        };
        propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();

        let vote = runner
            .process_proposal(propose)
            .expect("higher verified justify QC should replace stale high QC for safety");
        assert_eq!(vote.view, 4);
        assert_eq!(
            runner.safety.high_qc().unwrap().block_hash,
            new_qc_parent.hash()
        );
    }

    #[tokio::test]
    async fn pending_cap_admission_rejects_before_vote_or_persistence() {
        let config = durable_test_config("pending-resource-cap");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: events.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");
        runner.persistent_store = Some(store.clone());

        let genesis = Block::genesis(context);
        let too_deep = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: MAX_PENDING_DEPTH + 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        assert!(runner.ensure_pending_capacity(&too_deep).is_err());
        for view in 1..=MAX_PENDING_BLOCKS as u64 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view,
                height: 1,
                parent: genesis.hash(),
                payload: Vec::new(),
                proposer: config.node_id,
                commitment_root: CommitmentV2::default()
                    .root()
                    .expect("empty commitment root"),
                app_hash: [0u8; 32],
                timestamp: view,
                justify: None,
            };
            runner.pending.insert(block.hash(), block);
        }
        assert_eq!(runner.pending.len(), MAX_PENDING_BLOCKS);
        let events_before_rejection = events.lock().unwrap().len();

        let rejected_block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: MAX_PENDING_BLOCKS as u64 + 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: MAX_PENDING_BLOCKS as u64 + 1,
            justify: None,
        };
        assert!(runner.ensure_pending_capacity(&rejected_block).is_err());
        assert_eq!(runner.pending.len(), MAX_PENDING_BLOCKS);
        assert_eq!(events.lock().unwrap().len(), events_before_rejection);
        assert!(store.get(&rejected_block.hash()).is_none());
        assert!(runner
            .safety
            .safe_to_vote(&rejected_block, [0u8; 32])
            .is_ok());
    }

    #[tokio::test]
    async fn verified_justify_receives_reserved_pending_slot_after_soft_cap() {
        let config = durable_test_config("pending-soft-hard-reserve");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");
        let genesis = Block::genesis(context);
        let mut protected_roots = Vec::new();
        for view in 1..=MAX_PENDING_SOFT_BLOCKS as u64 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view,
                height: 1,
                parent: genesis.hash(),
                payload: Vec::new(),
                proposer: [view as u8; 32],
                commitment_root: CommitmentV2::default()
                    .root()
                    .expect("empty commitment root"),
                app_hash: [view as u8; 32],
                timestamp: view,
                justify: None,
            };
            protected_roots.push(block.hash());
            runner
                .store
                .save_speculative(&block)
                .expect("small speculative row");
            runner.pending.insert(block.hash(), block);
        }
        assert_eq!(runner.pending.len(), MAX_PENDING_SOFT_BLOCKS);

        let ordinary = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 100,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: [100u8; 32],
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [100u8; 32],
            timestamp: 100,
            justify: None,
        };
        assert!(!runner.pending_admission_available(&ordinary, &protected_roots));
        assert!(runner
            .ensure_speculative_store_admission_capacity(&ordinary)
            .is_err());

        let mut verified = ordinary.clone();
        verified.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            block_hash: genesis.hash(),
            app_hash: Some(genesis.app_hash),
            votes: Vec::new(),
            voters: vec![[9u8; 32]],
            bls_pubkeys: vec![vec![7u8; 48]],
            agg_signature: vec![5u8; 96],
        });
        assert!(runner.pending_admission_available(&verified, &protected_roots));
        assert!(runner
            .ensure_speculative_store_admission_capacity(&verified)
            .is_ok());
    }

    #[test]
    fn memory_speculative_hash_reuse_is_first_write_wins() {
        let store = MemoryBlockStore::new();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);

        let first = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: vec![1],
            proposer: [1u8; 32],
            commitment_root: [1u8; 32],
            app_hash: [2u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save_speculative(&first).unwrap();

        let mut larger_justify = first.clone();
        larger_justify.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            block_hash: genesis.hash(),
            app_hash: Some(genesis.app_hash),
            votes: Vec::new(),
            voters: vec![[9u8; 32]; 8],
            bls_pubkeys: vec![vec![7u8; 48]; 8],
            agg_signature: vec![5u8; 96],
        });
        assert_eq!(larger_justify.hash(), first.hash());
        assert!(
            serde_json::to_vec(&larger_justify).unwrap().len()
                > serde_json::to_vec(&first).unwrap().len()
        );

        store.save_speculative(&larger_justify).unwrap();
        assert!(store.get(&first.hash()).unwrap().justify.is_none());

        store.save(&first);
        store.save_speculative(&larger_justify).unwrap();
        assert!(store.get(&first.hash()).unwrap().justify.is_none());
    }

    #[test]
    fn maximum_payload_parent_and_child_fit_reserved_byte_budget() {
        let store = MemoryBlockStore::new();
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        store.save(&genesis);
        let commitment_root = CommitmentV2::default().root().unwrap();

        let parent = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: vec![u8::MAX; crate::types::MAX_BLOCK_PAYLOAD_SIZE],
            proposer: [1u8; 32],
            commitment_root,
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        parent.validate().unwrap();
        store
            .ensure_speculative_capacity(&parent, MAX_PENDING_SOFT_BLOCKS, MAX_PENDING_SOFT_BYTES)
            .expect("one maximum-payload ordinary parent must fit the soft budget");
        store.save_speculative(&parent).unwrap();

        let child = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: parent.hash(),
            payload: vec![u8::MAX; crate::types::MAX_BLOCK_PAYLOAD_SIZE],
            proposer: [2u8; 32],
            commitment_root,
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: Some(Certificate {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: parent.view,
                block_hash: parent.hash(),
                app_hash: Some(parent.app_hash),
                votes: Vec::new(),
                voters: vec![[9u8; 32]],
                bls_pubkeys: vec![vec![7u8; 48]],
                agg_signature: vec![5u8; 96],
            }),
        };
        child.validate().unwrap();
        store
            .ensure_speculative_capacity(&child, MAX_PENDING_BLOCKS, MAX_PENDING_BYTES)
            .expect("one maximum-payload justified child must fit the reserved hard budget");
        store.save_speculative(&child).unwrap();
        assert!(store.get(&parent.hash()).is_some());
        assert!(store.get(&child.hash()).is_some());
    }

    #[tokio::test]
    async fn verified_qc_progress_uses_reserved_slot_then_commit_reopens_ordinary_admission() {
        let config = durable_test_config("verified-qc-reserved-progress");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");

        let mut height_one = Vec::new();
        for view in 1..=MAX_PENDING_SOFT_BLOCKS as u64 {
            let propose = test_propose(&config, view);
            let block = propose.block.clone();
            assert!(
                runner.process_proposal(propose).is_some(),
                "valid height-one proposal at view {view} should be voted"
            );
            height_one.push(block);
        }
        assert_eq!(runner.pending.len(), MAX_PENDING_SOFT_BLOCKS);
        let secret = config.bls_secret_key().expect("test BLS key");
        let pending_before: HashSet<Hash> = runner.pending.keys().copied().collect();
        let mut malformed = test_propose(&config, MAX_PENDING_SOFT_BLOCKS as u64 + 1);
        malformed.block.commitment_root = [0u8; 32];
        malformed.proposer_signature = secret.sign(&malformed.signing_data()).to_bytes().to_vec();
        assert!(runner.process_proposal(malformed.clone()).is_none());
        assert_eq!(
            runner.pending.keys().copied().collect::<HashSet<_>>(),
            pending_before
        );
        assert!(runner.store.get(&malformed.block.hash()).is_none());
        assert!(height_one
            .iter()
            .all(|block| runner.store.get(&block.hash()).is_some()));

        let committee = config.committee().expect("test committee");
        let qc_for = |block: &Block| {
            super::form_certificate(
                &committee,
                context,
                vec![Vote::new_bls(
                    context,
                    block.view,
                    block.hash(),
                    block.app_hash,
                    config.node_id,
                    &secret,
                )],
                true,
            )
            .expect("single-validator QC")
        };
        let parent = height_one.first().expect("height-one parent").clone();
        let parent_qc = qc_for(&parent);
        let child = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: MAX_PENDING_SOFT_BLOCKS as u64 + 1,
            height: 2,
            parent: parent.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: parent.timestamp,
            justify: Some(parent_qc.clone()),
        };
        let mut child_propose = Propose {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            block: child.clone(),
            justify: Some(parent_qc),
            proposer_signature: Vec::new(),
        };
        child_propose.proposer_signature = secret
            .sign(&child_propose.signing_data())
            .to_bytes()
            .to_vec();
        assert!(
            runner.process_proposal(child_propose).is_some(),
            "verified QC child should consume the reserved hard-cap slot"
        );
        assert_eq!(runner.pending.len(), MAX_PENDING_BLOCKS);

        let child_qc = qc_for(&child);
        runner.process_qc(child_qc);
        assert_eq!(runner.committed_height, 1);
        assert_eq!(runner.committed_hash, parent.hash());

        runner
            .prune_speculative_stores(None)
            .expect("commit-disconnected forks should be reclaimable");
        assert!(runner
            .store
            .get(&height_one.last().expect("stale fork").hash())
            .is_none());

        let ordinary = test_propose(&config, 200).block;
        assert!(runner
            .ensure_speculative_store_admission_capacity(&ordinary)
            .is_ok());
        assert!(runner.pending_admission_available(&ordinary, &[]));
    }

    #[tokio::test]
    async fn delayed_higher_view_sibling_rolls_a_body_and_reopens_after_commit() {
        let config = durable_test_config("rolling-progress-slot");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events,
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let genesis = Block::genesis(context);
        store.inner.save(&genesis);
        store.inner.set_committed(&genesis.hash());

        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");
        runner.persistent_store = Some(store.clone());

        let mut height_one = Vec::new();
        for view in 1..=MAX_PENDING_SOFT_BLOCKS as u64 {
            let propose = test_propose(&config, view);
            let block = propose.block.clone();
            assert!(
                runner.process_proposal(propose).is_some(),
                "height-one proposal at view {view} should be accepted"
            );
            height_one.push(block);
        }
        assert_eq!(height_one.len(), MAX_PENDING_SOFT_BLOCKS);

        let secret = config.bls_secret_key().expect("test BLS key");
        let committee = config.committee().expect("test committee");
        let qc_for = |block: &Block| {
            super::form_certificate(
                &committee,
                context,
                vec![Vote::new_bls(
                    context,
                    block.view,
                    block.hash(),
                    block.app_hash,
                    config.node_id,
                    &secret,
                )],
                true,
            )
            .expect("single-validator QC")
        };
        let parent = height_one.first().expect("height-one parent").clone();
        let parent_qc = qc_for(&parent);
        let make_child = |view: u64| Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height: parent.height + 1,
            parent: parent.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: parent.timestamp,
            justify: Some(parent_qc.clone()),
        };
        let process_child = |runner: &mut ConsensusRunner, block: Block| {
            let mut propose = Propose {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                block,
                justify: Some(parent_qc.clone()),
                proposer_signature: Vec::new(),
            };
            propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();
            runner.process_proposal(propose)
        };

        // A consumes the hard 64th slot but never receives a QC.  A later
        // certified continuation B must roll only A's unprotected sibling
        // branch, including any descendants, before recording its vote.
        let child_a = make_child(MAX_PENDING_SOFT_BLOCKS as u64 + 1);
        assert!(process_child(&mut runner, child_a.clone()).is_some());
        assert!(runner.store.get(&child_a.hash()).is_some());
        assert!(store.get(&child_a.hash()).is_some());

        let child_b = make_child(MAX_PENDING_SOFT_BLOCKS as u64 + 2);
        assert!(process_child(&mut runner, child_b.clone()).is_some());
        assert!(runner.store.get(&child_b.hash()).is_some());
        assert!(store.get(&child_b.hash()).is_some());
        assert!(runner.store.get(&child_a.hash()).is_none());
        assert!(store.get(&child_a.hash()).is_none());
        assert_eq!(
            runner.store.get(&parent.hash()).unwrap().hash(),
            parent.hash()
        );
        assert_eq!(store.get(&parent.hash()).unwrap().hash(), parent.hash());

        runner.process_qc(qc_for(&child_b));
        assert_eq!(runner.committed_height, parent.height);
        assert_eq!(runner.committed_hash, parent.hash());
        runner
            .prune_speculative_stores(None)
            .expect("stale disconnected siblings should be reclaimed");
        assert!(store.get(&child_a.hash()).is_none());
        assert!(runner
            .ensure_speculative_store_admission_capacity(&test_propose(&config, 200).block)
            .is_ok());
    }

    #[test]
    fn rolling_admission_preserves_protected_sibling_without_partial_writes() {
        let context = ConsensusContext::new(0, [7u8; 32]);
        let genesis = Block::genesis(context);
        let old = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 4,
            height: 1,
            parent: genesis.hash(),
            payload: vec![4],
            proposer: [4u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [4u8; 32],
            timestamp: 4,
            justify: None,
        };
        let mut next = old.clone();
        next.view = 5;
        next.timestamp = 5;
        next.payload = vec![5];
        next.app_hash = [5u8; 32];
        next.justify = Some(Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 3,
            block_hash: genesis.hash(),
            app_hash: Some(genesis.app_hash),
            votes: Vec::new(),
            voters: Vec::new(),
            bls_pubkeys: Vec::new(),
            agg_signature: Vec::new(),
        });

        let memory = MemoryBlockStore::new();
        memory.save(&genesis);
        memory.set_committed(&genesis.hash());
        memory.save_speculative(&old).unwrap();
        let error = memory
            .admit_speculative_with_rolling_victim(&next, &[old.hash()], 1, usize::MAX)
            .expect_err("protected sibling must reject at the hard cap");
        assert!(error.to_string().contains("protected speculative branches"));
        assert!(memory.get(&old.hash()).is_some());
        assert!(memory.get(&next.hash()).is_none());
        assert_eq!(memory.get_by_height(0).unwrap().hash(), genesis.hash());
        let mut ordinary = next.clone();
        ordinary.justify = None;
        assert!(memory
            .admit_speculative_with_rolling_victim(&ordinary, &[], 1, usize::MAX)
            .is_err());
        assert!(memory.get(&old.hash()).is_some());
        assert!(memory.get(&ordinary.hash()).is_none());

        let temp_dir = TempDir::new().expect("temporary storage directory");
        let rocks = RocksDbStore::open(temp_dir.path()).expect("open test store");
        rocks.save(&genesis);
        rocks.set_committed(&genesis.hash());
        rocks.save_speculative(&old).unwrap();
        let error = rocks
            .admit_speculative_with_rolling_victim(&next, &[old.hash()], 1, usize::MAX)
            .expect_err("protected sibling must reject at the hard cap");
        assert!(error.to_string().contains("protected speculative branches"));
        assert!(rocks.get(&old.hash()).is_some());
        assert!(rocks.get(&next.hash()).is_none());
        assert_eq!(rocks.get_by_height(0).unwrap().hash(), genesis.hash());
        assert!(rocks
            .admit_speculative_with_rolling_victim(&ordinary, &[], 1, usize::MAX)
            .is_err());
        assert!(rocks.get(&old.hash()).is_some());
        assert!(rocks.get(&ordinary.hash()).is_none());
    }

    #[tokio::test]
    async fn pending_pruning_keeps_protected_root_and_ancestor_closure() {
        let config = durable_test_config("pending-protected-closure");
        let context = config.context().expect("test context");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");

        let genesis = Block::genesis(context);
        let protected_parent = test_propose(&config, 1).block;
        let protected_child = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: protected_parent.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: 2,
            justify: None,
        };
        let fork = Block {
            view: 3,
            height: 1,
            parent: genesis.hash(),
            ..protected_parent.clone()
        };
        for block in [&protected_parent, &protected_child, &fork] {
            runner.store.save(block);
            runner.pending.insert(block.hash(), block.clone());
        }

        runner.prune_pending_unprotected_branches(Some(protected_child.hash()));
        assert!(runner.pending.contains_key(&protected_parent.hash()));
        assert!(runner.pending.contains_key(&protected_child.hash()));
        assert!(!runner.pending.contains_key(&fork.hash()));
    }

    #[tokio::test]
    async fn failed_vote_intent_write_stops_before_live_vote_mutation() {
        let config = durable_test_config("follower-vote-failure");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: events.clone(),
            fail_consensus_state: true,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");
        runner.persistent_store = Some(store);

        let propose = test_propose(&config, 1);
        let block = propose.block.clone();
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runner.process_proposal(propose)
        }));
        assert!(panic_result.is_err(), "storage failure must fail-stop");
        assert_eq!(events.lock().unwrap().first(), Some(&"save_block"));
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .skip(1)
            .all(|event| *event == "save_consensus_state"));
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());
    }

    #[tokio::test]
    async fn leader_self_vote_persists_intent_before_local_aggregation() {
        let config = durable_test_config("leader-vote-order");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: events.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new(config, network)
            .await
            .expect("construct test runner")
            .with_app(NoOpApp);
        runner.persistent_store = Some(store.clone());

        runner
            .run_leader_round(0)
            .await
            .expect("leader round should complete");

        let events = events.lock().unwrap();
        let block_index = events
            .iter()
            .position(|event| *event == "save_block")
            .expect("proposal block must be durable");
        let vote_index = events
            .iter()
            .position(|event| *event == "save_consensus_state")
            .expect("leader vote intent must be durable");
        assert!(block_index < vote_index);
        let persisted = store
            .load_consensus_state()
            .expect("load leader vote intent")
            .expect("leader vote intent must remain durable");
        assert!(persisted.voted_views.contains(&0));
    }

    #[tokio::test]
    async fn failed_finalized_commit_does_not_publish_application_or_heads() {
        let config = durable_test_config("runner-commit-failure-order");
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let fail_commit_block = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: fail_commit_block.clone(),
        });
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store)
            .await
            .expect("construct recovered test runner");
        order.lock().unwrap().clear();

        let commit_calls = Arc::new(AtomicUsize::new(0));
        runner = runner.with_app(LifecycleApp {
            commit_calls: commit_calls.clone(),
            order: order.clone(),
        });
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        runner.store.save(&block);
        runner.pending.insert(block.hash(), block.clone());
        let committed_hash = runner.committed_hash;
        let sync_request = crate::types::SyncRequest {
            from_height: 0,
            to_height: None,
            max_blocks: 100,
            request_id: 1,
        };
        let advertised_before = runner
            .sync_handler
            .as_ref()
            .expect("recovery creates sync handler")
            .handle_sync_request(sync_request.clone())
            .peer_height;

        fail_commit_block.store(true, Ordering::SeqCst);
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runner.try_commit(&block.hash())
        }));
        assert!(panic_result.is_err(), "storage failure must fail-stop");
        assert_eq!(commit_calls.load(Ordering::SeqCst), 0);
        assert!(order
            .lock()
            .unwrap()
            .iter()
            .all(|event| *event == "commit_block"));
        assert_eq!(runner.committed_height, 0);
        assert_eq!(runner.committed_hash, committed_hash);
        let advertised_after = runner
            .sync_handler
            .as_ref()
            .expect("recovery creates sync handler")
            .handle_sync_request(sync_request)
            .peer_height;
        assert_eq!(advertised_before, 0);
        assert_eq!(advertised_after, advertised_before);
    }

    #[tokio::test]
    async fn finalized_commit_persists_before_application_publish() {
        let config = durable_test_config("runner-commit-order");
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct recovered test runner");
        order.lock().unwrap().clear();

        let commit_calls = Arc::new(AtomicUsize::new(0));
        runner = runner.with_app(LifecycleApp {
            commit_calls: commit_calls.clone(),
            order: order.clone(),
        });
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: CommitmentV2::default()
                .root()
                .expect("empty commitment root"),
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        runner.store.save(&block);
        runner.pending.insert(block.hash(), block.clone());
        assert!(runner.try_commit(&block.hash()).is_some());

        assert_eq!(&*order.lock().unwrap(), &["commit_block", "app_commit"]);
        assert_eq!(commit_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.committed_height, 1);
        assert_eq!(runner.committed_hash, block.hash());
        assert_eq!(
            runner.store.get_committed_head().unwrap().hash(),
            block.hash()
        );
        let sync_request = crate::types::SyncRequest {
            from_height: 0,
            to_height: None,
            max_blocks: 100,
            request_id: 1,
        };
        let response = runner
            .sync_handler
            .as_ref()
            .expect("recovery creates sync handler")
            .handle_sync_request(sync_request);
        assert_eq!(response.peer_height, 1);
        assert_eq!(response.blocks.last().map(|b| b.height), Some(1));
    }

    #[tokio::test]
    async fn finalized_commit_persists_the_preflight_commitment_atomically() {
        let config = durable_test_config("runner-commitment-order");
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let commitment = CommitmentV2::new(Vec::new()).expect("empty commitment");
        let commitment_root = commitment.root().expect("empty commitment root");
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct recovered test runner")
            .with_app(CommitmentLifecycleApp {
                order: order.clone(),
                commitment: commitment.clone(),
            });
        order.lock().unwrap().clear();

        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root,
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        runner.store.save(&block);
        runner.pending.insert(block.hash(), block.clone());
        assert!(runner.try_commit(&block.hash()).is_some());

        assert_eq!(
            &*order.lock().unwrap(),
            &["app_preflight", "commit_block", "app_commit"]
        );
        assert_eq!(
            store.load_commitment(&block.hash()).unwrap(),
            Some(commitment)
        );
    }

    #[tokio::test]
    async fn finalized_commit_persists_the_exact_preflight_state_root_atomically() {
        let config = durable_test_config("runner-state-root-order");
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let order = Arc::new(Mutex::new(Vec::new()));
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: order.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let commitment = CommitmentV2::new(Vec::new()).expect("empty commitment");
        let commitment_root = commitment.root().expect("empty commitment root");
        let state_root = [0x5au8; 32];
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct recovered test runner")
            .with_app(StateRootLifecycleApp {
                order: order.clone(),
                commitment,
                state_root,
            });
        order.lock().unwrap().clear();

        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root,
            app_hash: state_root,
            timestamp: 1,
            justify: None,
        };
        runner.store.save(&block);
        runner.pending.insert(block.hash(), block.clone());
        assert!(runner.try_commit(&block.hash()).is_some());

        assert_eq!(
            &*order.lock().unwrap(),
            &[
                "app_preflight",
                "app_state_root",
                "commit_block",
                "app_commit"
            ]
        );
        assert_eq!(
            store.load_state_root(&block.hash()).unwrap(),
            Some(state_root)
        );
    }

    struct RejectingCommitmentApp;

    impl AppHook for RejectingCommitmentApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some(block.app_hash))
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Err("injected commitment failure".to_string())
        }
    }

    struct RejectingStateRootApp;

    impl AppHook for RejectingStateRootApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Ok(Some(CommitmentV2::default()))
        }

        fn preflight_state_root(&self, _block: &Block) -> Result<Option<Hash>, String> {
            Err("injected full-state root failure".to_string())
        }
    }

    struct MismatchedStateRootApp;

    impl AppHook for MismatchedStateRootApp {
        fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
            Vec::new()
        }

        fn execute(&mut self, block: &Block) -> Hash {
            block.app_hash
        }

        fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
            Ok(Some(CommitmentV2::default()))
        }

        fn preflight_state_root(&self, _block: &Block) -> Result<Option<Hash>, String> {
            Ok(Some([0xabu8; 32]))
        }
    }

    #[tokio::test]
    async fn commitment_failure_rejects_proposal_before_vote_or_persistence() {
        let config = durable_test_config("runner-commitment-reject");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(RejectingCommitmentApp);

        let propose = test_propose(&config, 1);
        let block = propose.block.clone();
        assert!(runner.process_proposal(propose).is_none());
        assert!(runner.pending.is_empty());
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());
    }

    #[tokio::test]
    async fn mismatched_commitment_root_rejects_proposal_before_vote_or_persistence() {
        let config = durable_test_config("runner-commitment-root-reject");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(NoOpApp);

        let mut propose = test_propose(&config, 1);
        propose.block.commitment_root = [9u8; 32];
        let secret = config.bls_secret_key().expect("test BLS key");
        propose.proposer_signature = secret.sign(&propose.signing_data()).to_bytes().to_vec();
        let block = propose.block.clone();

        assert!(runner.process_proposal(propose).is_none());
        assert!(runner.pending.is_empty());
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());
    }

    #[tokio::test]
    async fn structurally_malformed_proposal_rejects_before_app_or_store_mutation() {
        let config = durable_test_config("runner-structural-resource-reject");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let execute_calls = Arc::new(AtomicUsize::new(0));
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(CountingApp {
                execute_calls: execute_calls.clone(),
                commit_calls: Arc::new(AtomicUsize::new(0)),
                execute_parents: Arc::new(Mutex::new(Vec::new())),
            });

        let mut zero_root = test_propose(&config, 1);
        zero_root.block.commitment_root = [0u8; 32];
        let secret = config.bls_secret_key().expect("test BLS key");
        zero_root.proposer_signature = secret.sign(&zero_root.signing_data()).to_bytes().to_vec();
        assert!(runner.process_proposal(zero_root.clone()).is_none());
        assert_eq!(execute_calls.load(Ordering::SeqCst), 0);
        assert!(runner.pending.is_empty());
        assert!(runner.store.get(&zero_root.block.hash()).is_none());
        assert!(runner
            .safety
            .safe_to_vote(&zero_root.block, [0u8; 32])
            .is_ok());

        let mut oversized = test_propose(&config, 2);
        oversized.block.payload = vec![0u8; crate::types::MAX_BLOCK_PAYLOAD_SIZE + 1];
        oversized.proposer_signature = secret.sign(&oversized.signing_data()).to_bytes().to_vec();
        assert!(runner.process_proposal(oversized.clone()).is_none());
        assert_eq!(execute_calls.load(Ordering::SeqCst), 0);
        assert!(runner.pending.is_empty());
        assert!(runner.store.get(&oversized.block.hash()).is_none());
        assert!(runner
            .safety
            .safe_to_vote(&oversized.block, [0u8; 32])
            .is_ok());
    }

    #[tokio::test]
    async fn state_root_failure_rejects_proposal_before_vote_or_persistence() {
        let config = durable_test_config("runner-state-root-reject");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open test store"),
            events: Arc::new(Mutex::new(Vec::new())),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let mut runner = ConsensusRunner::new_with_recovery(config.clone(), network, store.clone())
            .await
            .expect("construct test runner")
            .with_app(RejectingStateRootApp);

        let propose = test_propose(&config, 1);
        let block = propose.block.clone();
        assert!(runner.process_proposal(propose).is_none());
        assert!(runner.pending.is_empty());
        assert!(store.get(&block.hash()).is_none());
        assert!(store
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|event| { *event != "save_block" && *event != "save_consensus_state" }));
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());
    }

    #[tokio::test]
    async fn mismatched_authenticated_state_root_rejects_proposal_before_vote() {
        let config = durable_test_config("runner-state-root-mismatch");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner")
            .with_app(MismatchedStateRootApp);

        let propose = test_propose(&config, 1);
        let block = propose.block.clone();
        assert!(runner.process_proposal(propose).is_none());
        assert!(runner.pending.is_empty());
        assert!(runner.safety.safe_to_vote(&block, [0u8; 32]).is_ok());
    }

    #[test]
    fn timeout_certificate_suppresses_followup_view_change() {
        assert!(!should_emit_view_change_after_timeout_certificate(true));
        assert!(should_emit_view_change_after_timeout_certificate(false));
    }

    #[test]
    fn recovery_resumes_after_the_persisted_high_qc_view() {
        let context = ConsensusContext::new(0, [7u8; 32]);
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(Certificate {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: 4,
                block_hash: [8u8; 32],
                app_hash: Some([9u8; 32]),
                votes: Vec::new(),
                voters: Vec::new(),
                bls_pubkeys: Vec::new(),
                agg_signature: Vec::new(),
            }),
            locked_qc: None,
            voted_views: vec![4],
            current_view: 4,
            committed_height: 1,
            committed_hash: [6u8; 32],
            consecutive_timeouts: 3,
            vc_sent_for_view: Some(4),
        };

        assert_eq!(recovery_resume_view(&state), 5);
    }

    #[test]
    fn timeout_view_window_rejects_stale_and_far_future_views() {
        assert!(super::is_view_in_bounded_window(10, 10));
        assert!(super::is_view_in_bounded_window(
            10,
            10 + super::MAX_FUTURE_VIEWS
        ));
        assert!(!super::is_view_in_bounded_window(10, 9));
        assert!(!super::is_view_in_bounded_window(
            10,
            11 + super::MAX_FUTURE_VIEWS,
        ));
    }

    #[test]
    fn high_qc_requires_a_matching_local_certified_block() {
        let node_id = [1u8; 32];
        let secret = BlsSecretKey::from_seed(&[7u8; 32]);
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 1000,
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            bls_secret_key: Some(secret.to_bytes()),
        };
        let committee = config.committee().expect("valid one-node committee");
        let context = committee.initial_context();
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 1,
            parent: Block::genesis(context).hash(),
            payload: vec![],
            proposer: node_id,
            commitment_root: [0u8; 32],
            app_hash: [9u8; 32],
            timestamp: 1,
            justify: None,
        };
        let vote = Vote::new_bls(
            context,
            block.view,
            block.hash(),
            block.app_hash,
            node_id,
            &secret,
        );
        let qc = super::form_certificate(&committee, context, vec![vote], true)
            .expect("one-node weighted QC should form");

        assert!(verify_high_qc_against_block(&committee, context, &qc, Some(&block), true).is_ok());
        assert!(verify_high_qc_against_block(&committee, context, &qc, None, true).is_err());

        let mut wrong_view_qc = qc.clone();
        wrong_view_qc.view += 1;
        assert!(verify_high_qc_against_block(
            &committee,
            context,
            &wrong_view_qc,
            Some(&block),
            true,
        )
        .is_err());
    }

    #[tokio::test]
    async fn valid_qc_fork_cannot_drive_leader_execution_or_commit() {
        let config = durable_test_config("runner-parent-anchor");
        let context = config.context().expect("test context");
        let committee = config.committee().expect("test committee");
        let secret = config.bls_secret_key().expect("test BLS key");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new(config.clone(), network)
            .await
            .expect("construct test runner");

        let execute_calls = Arc::new(AtomicUsize::new(0));
        let commit_calls = Arc::new(AtomicUsize::new(0));
        let execute_parents = Arc::new(Mutex::new(Vec::new()));
        runner = runner.with_app(CountingApp {
            execute_calls: execute_calls.clone(),
            commit_calls: commit_calls.clone(),
            execute_parents: execute_parents.clone(),
        });

        let genesis = Block::genesis(context);
        let fork_parent = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: [9u8; 32],
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let fork_target = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: fork_parent.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: None,
        };
        let vote = Vote::new_bls(
            context,
            fork_target.view,
            fork_target.hash(),
            fork_target.app_hash,
            config.node_id,
            &secret,
        );
        let qc = super::form_certificate(&committee, context, vec![vote], true)
            .expect("one-node QC should be cryptographically valid");
        super::verify_certificate(
            &committee,
            &qc,
            context,
            fork_target.view,
            &fork_target.hash(),
            Some(&fork_target.app_hash),
            true,
        )
        .expect("fixture QC must verify cryptographically");

        runner.store.save(&fork_parent);
        runner.store.save(&fork_target);
        runner.process_qc(qc.clone());
        assert!(runner.safety.high_qc().is_none());
        assert!(runner.try_commit(&fork_parent.hash()).is_none());
        assert_eq!(runner.committed_height, 0);
        assert_eq!(runner.committed_hash, genesis.hash());
        assert_eq!(commit_calls.load(Ordering::SeqCst), 0);

        // Even if stale recovery data has already placed the fork QC in
        // safety state, a leader must fall back to the exact committed head.
        runner.safety.update_high_qc(qc);
        assert_eq!(runner.get_proposal_parent().hash(), genesis.hash());
        runner
            .run_leader_round(0)
            .await
            .expect("leader should continue from the committed head");
        assert_eq!(execute_calls.load(Ordering::SeqCst), 1);
        assert!(execute_parents
            .lock()
            .unwrap()
            .iter()
            .all(|parent| *parent != fork_parent.hash()));
        assert_eq!(commit_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn recovery_requires_the_complete_finalized_chain() {
        let mut config = ConsensusConfig::single_node();
        let committee = config.committee().expect("valid one-node committee");
        config.genesis_hash = crate::types::genesis_domain_hash(
            "recovery-test",
            config.epoch,
            config.view_timeout_ms,
            committee.hash(),
        );
        let context = config.context().expect("valid recovery context");
        let genesis = Block::genesis(context);
        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store = RocksDbStore::open(temp_dir.path()).expect("open temporary store");
        store
            .commit_block(&genesis, &state)
            .expect("persist genesis atomically");

        let recovered = super::validate_recovered_chain(&store, context, &committee, &state, true)
            .expect("genesis-only chain should recover");
        assert_eq!(recovered.len(), 1);

        // A crash can leave a voted/proposed child durable after the last
        // finalized commit.  It must remain available by hash but must not be
        // counted as part of the committed replay chain.
        let proposal = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        store.save(&proposal);
        let recovered_with_proposal =
            super::validate_recovered_chain(&store, context, &committee, &state, true)
                .expect("unfinalized proposal must not invalidate recovery");
        assert_eq!(recovered_with_proposal.len(), 1);

        let mut missing = state.clone();
        missing.committed_height = 1;
        missing.committed_hash = [3u8; 32];
        assert!(
            super::validate_recovered_chain(&store, context, &committee, &missing, true).is_err()
        );
    }

    #[test]
    fn recovery_keeps_uncommitted_high_qc_target_available() {
        let mut config = ConsensusConfig::single_node();
        let secret = config
            .bls_secret_key()
            .expect("single-node config has a BLS key");
        let committee = config.committee().expect("valid one-node committee");
        config.genesis_hash = crate::types::genesis_domain_hash(
            "recovery-qc-test",
            config.epoch,
            config.view_timeout_ms,
            committee.hash(),
        );
        let context = config.context().expect("valid recovery context");
        let genesis = Block::genesis(context);
        let commitment = CommitmentV2::default();
        let commitment_root = commitment.root().expect("empty commitment root");
        let block1 = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root,
            app_hash: [1u8; 32],
            timestamp: 1,
            justify: None,
        };
        let vote1 = Vote::new_bls(
            context,
            block1.view,
            block1.hash(),
            block1.app_hash,
            config.node_id,
            &secret,
        );
        let qc1 = super::form_certificate(&committee, context, vec![vote1], true)
            .expect("one-node QC should form");
        let block2 = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 2,
            height: 2,
            parent: block1.hash(),
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root,
            app_hash: [2u8; 32],
            timestamp: 2,
            justify: Some(qc1.clone()),
        };
        let vote2 = Vote::new_bls(
            context,
            block2.view,
            block2.hash(),
            block2.app_hash,
            config.node_id,
            &secret,
        );
        let qc2 = super::form_certificate(&committee, context, vec![vote2], true)
            .expect("one-node QC should form");

        let state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: Some(qc2),
            locked_qc: Some(qc1),
            voted_views: vec![1, 2],
            current_view: 2,
            committed_height: 1,
            committed_hash: block1.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store = RocksDbStore::open(temp_dir.path()).expect("open temporary store");
        let genesis_state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store
            .commit_block(&genesis, &genesis_state)
            .expect("persist genesis");
        store
            .save_block(&block2)
            .expect("persist uncommitted QC target");
        store
            .commit_block_with_commitment(&block1, &state, Some(&commitment))
            .expect("persist committed block with QC state");
        assert_eq!(
            store.load_state_root(&block1.hash()).unwrap(),
            Some(block1.app_hash)
        );

        let recovered = super::validate_recovered_chain(&store, context, &committee, &state, true)
            .expect("chain and persisted QC references should recover");
        assert_eq!(recovered.len(), 2);
        assert_eq!(store.get(&block2.hash()).unwrap().hash(), block2.hash());
    }

    #[tokio::test]
    async fn new_rejects_unauthenticated_tcp_transport() {
        let network = TcpNetwork::new(NetworkConfig::local_three_nodes(0))
            .await
            .expect("dev network should be constructible");

        let error = match ConsensusRunner::new(ConsensusConfig::single_node(), network).await {
            Ok(_) => panic!("live consensus must reject unauthenticated TCP transport"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("requires authenticated TCP transport"));
    }

    #[tokio::test]
    async fn new_with_recovery_rejects_unauthenticated_tcp_transport() {
        let network = TcpNetwork::new(NetworkConfig::local_three_nodes(0))
            .await
            .expect("dev network should be constructible");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let persistent_store: Arc<dyn PersistentStore + Send + Sync> =
            Arc::new(RocksDbStore::open(temp_dir.path()).expect("open temporary store"));

        let error = match ConsensusRunner::new_with_recovery(
            ConsensusConfig::single_node(),
            network,
            persistent_store,
        )
        .await
        {
            Ok(_) => panic!("live recovery must reject unauthenticated TCP transport"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("requires authenticated TCP transport"));
    }

    #[tokio::test]
    async fn new_with_recovery_rejects_store_without_equivocation_journal() {
        let config = durable_test_config("runner-journal-capability");
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let events = Arc::new(Mutex::new(vec!["fail_equivocation_journal"]));
        let store: Arc<dyn PersistentStore + Send + Sync> = Arc::new(RecordingStore {
            inner: RocksDbStore::open(temp_dir.path()).expect("open temporary store"),
            events: events.clone(),
            fail_consensus_state: false,
            fail_commit_block: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });

        let error = match ConsensusRunner::new_with_recovery(config, network, store).await {
            Ok(_) => panic!("live recovery must require a durable evidence journal"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("requires a persistent equivocation proof journal"));
        assert_eq!(
            &*events.lock().expect("recording events lock"),
            &["fail_equivocation_journal"]
        );
    }

    #[test]
    fn direct_broadcast_does_not_schedule_equivocation_rebroadcast() {
        assert!(!should_rebroadcast_equivocation_batch(false));
        assert!(should_rebroadcast_equivocation_batch(true));
    }

    #[tokio::test]
    async fn authenticated_single_validator_runs_until_committed_height() {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = crate::types::genesis_domain_hash(
            "runner-test",
            config.epoch,
            config.view_timeout_ms,
            config.committee().expect("single-node committee").hash(),
        );
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("single-validator authenticated network");
        network.start().await.expect("single-validator listener");

        let mut runner = ConsensusRunner::new(config, network)
            .await
            .expect("single-validator runner")
            .with_app(NoOpApp);
        tokio::time::timeout(Duration::from_secs(2), runner.run_until_committed(2))
            .await
            .expect("runner should commit within the test timeout")
            .expect("runner should reach the requested height");

        assert_eq!(runner.committed_height(), 2);
        runner
            .run_until_committed(1)
            .await
            .expect("already-committed target should return immediately");
    }

    #[tokio::test]
    async fn run_until_committed_rejects_missing_application_hook() {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = crate::types::genesis_domain_hash(
            "runner-missing-app",
            config.epoch,
            config.view_timeout_ms,
            config.committee().expect("single-node committee").hash(),
        );
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("single-validator authenticated network");
        let mut runner = ConsensusRunner::new(config, network)
            .await
            .expect("single-validator runner");

        let error = runner
            .run_until_committed(1)
            .await
            .expect_err("runner must fail closed without an explicit application hook");
        assert!(error
            .to_string()
            .contains("explicitly attached application hook"));
    }

    #[tokio::test]
    async fn recovered_runner_requires_non_genesis_application_head_handshake() {
        let config = durable_test_config("runner-application-recovery-handshake");
        let context = config.context().expect("test context");
        let genesis = Block::genesis(context);
        let commitment = CommitmentV2::default();
        let block = test_propose(&config, 1).block;
        let temp_dir = TempDir::new().expect("temporary storage directory");
        let store = Arc::new(RocksDbStore::open(temp_dir.path()).expect("open test store"));
        let genesis_state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        store
            .commit_block(&genesis, &genesis_state)
            .expect("persist genesis");
        let recovered_state = ConsensusState {
            current_view: 1,
            committed_height: 1,
            committed_hash: block.hash(),
            ..genesis_state
        };
        store
            .commit_block_with_commitment_and_state_root(
                &block,
                &recovered_state,
                Some(&commitment),
                Some(&block.app_hash),
            )
            .expect("persist recovered head");

        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("authenticated test network");
        let mut runner = ConsensusRunner::new_with_recovery(config, network, store)
            .await
            .expect("recover consensus chain")
            .with_app(LifecycleApp {
                commit_calls: Arc::new(AtomicUsize::new(0)),
                order: Arc::new(Mutex::new(Vec::new())),
            });

        let error = runner
            .initialize_live()
            .expect_err("stateful app without an exact recovery handshake must be rejected");
        assert!(error
            .to_string()
            .contains("does not implement non-genesis recovery-head validation"));

        runner = runner.with_app(NoOpApp);
        runner
            .initialize_live()
            .expect("stateless app with matching roots should prove its recovered head");
    }

    #[tokio::test]
    async fn new_rejects_zero_view_timeout() {
        let mut config = ConsensusConfig::single_node();
        config.view_timeout_ms = 0;
        let network = TcpNetwork::new(single_node_network_config(&config))
            .await
            .expect("single-validator authenticated network");

        let error = match ConsensusRunner::new(config, network).await {
            Ok(_) => panic!("zero view timeout must be rejected"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("view timeout must be greater than zero"));
    }

    #[tokio::test]
    async fn new_rejects_network_node_identity_mismatch() {
        let config = ConsensusConfig::single_node();
        let network_node_id = [2u8; 32];
        let network_secret = BlsSecretKey::from_seed(&[9u8; 32]);
        let network = TcpNetwork::new(NetworkConfig {
            node_id: network_node_id,
            listen_addr: "127.0.0.1:0".to_string(),
            peers: vec![],
            require_authenticated_peers: true,
            bls_secret_key: Some(network_secret.clone()),
            validator_pubkeys: HashMap::from([(network_node_id, network_secret.public_key())]),
            gossip_validation: Some(GossipValidationConfig {
                context: config.context().expect("single-node context"),
                committee: config.committee().expect("single-node committee"),
                allow_dev_envelopes: false,
            }),
        })
        .await
        .expect("mismatched authenticated network");

        let error = match ConsensusRunner::new(config, network).await {
            Ok(_) => panic!("node identity mismatch must be rejected"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("node ID does not match network node ID"));
    }
}
