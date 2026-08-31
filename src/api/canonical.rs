//! Canonical application hook used by the consensus runner.
//!
//! Consensus performs proposal validation speculatively.  This wrapper keeps
//! those executions in per-block application snapshots while exposing one
//! committed `AppState` to the REST API and the commit path.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};

use crate::app::state::full_state_hash::ComponentTree;
use crate::app::{AppState, BlockExecutionArtifacts, ConsensusTransaction};
use crate::consensus::{AppHook, EquivocationProof};
use crate::types::{hash, Block, CommitmentV2, ConsensusContext, Hash};

use super::state::{Event, FinalizedReceiptEvent, SharedState, UserEvent};

/// Maximum number of speculative application snapshots retained at once.
///
/// A candidate owns a versioned copy-on-write `AppState` plus its block body.
/// The fixed bound remains a defense-in-depth memory limit because mutated
/// components and payloads are still candidate-local. When every retained
/// candidate may still be needed, new speculative execution fails closed
/// until a commit prunes the map; candidates are never evicted by age/FIFO.
pub(crate) const MAX_SPECULATIVE_CANDIDATES: usize = 16;

/// Maximum distance from the committed application head for a speculative
/// candidate. This is fixed in the binary so validators cannot disagree on
/// the resource policy through local environment configuration.
pub(crate) const MAX_SPECULATIVE_DEPTH: u64 = 16;

#[derive(Clone)]
struct Candidate {
    block: Block,
    state: AppState,
    /// Complete schema-v3 component-tree seal computed from this exact
    /// execution. Preflight and direct commit independently recompute it.
    full_state_tree: ComponentTree,
    /// Transactions included by this block or any speculative ancestor.
    /// The canonical mempool still contains them until commit, so this set is
    /// used to avoid proposing them again when extending the candidate.
    proposed_tx_hashes: HashSet<Hash>,
}

/// Application hook that separates speculative proposal execution from the
/// canonical state observed by API clients.
pub struct CanonicalAppHook {
    shared: SharedState,
    candidates: Mutex<HashMap<Hash, Candidate>>,
    /// Exact hash of the block represented by the canonical application
    /// state. Height alone cannot anchor a commit because a fork can reuse it.
    committed_hash: Option<Hash>,
}

impl CanonicalAppHook {
    /// Deterministic result used when a speculative application state fails
    /// its primary or derived invariant check. The candidate must not be
    /// exposed to preflight or commit after this point.
    fn invalid_consensus_state_hash() -> Hash {
        hash(b"invalid-canonical-consensus-state")
    }

    fn validate_consensus_state(state: &AppState, boundary: &str) -> Result<(), String> {
        state
            .validate_consensus_state()
            .map(|_| ())
            .map_err(|error| format!("{boundary}: {error}"))
    }

    /// Create a canonical hook over the `SharedState` application handle.
    pub fn new(shared: SharedState) -> Self {
        Self {
            shared,
            candidates: Mutex::new(HashMap::new()),
            committed_hash: None,
        }
    }

    /// Return the state handle used by this hook and the API.
    pub fn shared_state(&self) -> SharedState {
        self.shared.clone()
    }

    /// Number of speculative candidates retained by the hook.
    pub fn candidate_count(&self) -> usize {
        self.candidates
            .lock()
            .map(|candidates| candidates.len())
            .unwrap_or_default()
    }

    /// Return the exact block hash represented by the canonical application
    /// state, when this hook has been anchored to a persisted head.
    ///
    /// This is crate-visible because state sync must reject a store/app pair
    /// whose heights happen to agree while their histories do not.
    pub(crate) fn exact_committed_hash(&self) -> Option<Hash> {
        self.committed_hash
    }

    /// Construct a private hook over an already verified application anchor.
    ///
    /// `CanonicalAppHook::new` intentionally starts unanchored for ordinary
    /// genesis callers. Verified replay needs the same exact-hash boundary as
    /// a recovered live hook, but its candidates must remain completely
    /// isolated from the live hook.
    pub(crate) fn for_verified_anchor(shared: SharedState, committed_hash: Hash) -> Self {
        Self {
            shared,
            candidates: Mutex::new(HashMap::new()),
            committed_hash: Some(committed_hash),
        }
    }

    /// Restore a verified speculative branch after a process restart.
    ///
    /// The canonical application is replayed separately from consensus
    /// recovery.  A persisted high/locked QC can nevertheless point at a
    /// block above the committed head, and the next proposal must execute on
    /// that block's application state. Replay each such block through a
    /// temporary hook and publish its candidates only after the entire branch
    /// validates; the canonical state is never mutated by this method.
    pub fn restore_speculative_chain(
        &mut self,
        context: ConsensusContext,
        committed_head: &Block,
        blocks: &[Block],
    ) -> Result<(), String> {
        committed_head.validate_context(context)?;
        committed_head.validate()?;
        let canonical_height = self.canonical_read().committed_height();
        if committed_head.height != canonical_height {
            return Err(format!(
                "speculative replay anchor height {} does not match canonical height {}",
                committed_head.height, canonical_height
            ));
        }
        let trusted_head_hash = if canonical_height == 0 {
            self.committed_hash
                .or_else(|| Some(Block::genesis(context).hash()))
        } else {
            Some(self.committed_hash.ok_or_else(|| {
                "speculative replay requires a trusted committed head hash for nonzero canonical height"
                    .to_string()
            })?)
        };
        if trusted_head_hash != Some(committed_head.hash()) {
            return Err(
                "speculative replay anchor does not match the exact canonical head".to_string(),
            );
        }

        // Stage the entire branch against the trusted anchor. The temporary
        // hook shares only the read-only canonical state handle; its candidate
        // map and committed hash are private, so a later replay failure cannot
        // partially publish recovery state.
        let mut staged = Self::new(self.shared.clone());
        staged.committed_hash = Some(committed_head.hash());

        let mut parent_hash = committed_head.hash();
        let mut expected_height = committed_head
            .height
            .checked_add(1)
            .ok_or_else(|| "speculative replay height overflows".to_string())?;

        for block in blocks {
            block.validate_context(context)?;
            block.validate()?;
            if block.app_hash == [0u8; 32] {
                return Err(format!(
                    "speculative replay block at height {} has an empty app hash",
                    block.height
                ));
            }
            if block.height != expected_height {
                return Err(format!(
                    "speculative replay block height {} is not the expected {}",
                    block.height, expected_height
                ));
            }
            if block.parent != parent_hash {
                return Err(format!(
                    "speculative replay block at height {} has a broken parent",
                    block.height
                ));
            }

            // `execute` validates and replays the block against either its
            // restored parent candidate or the canonical state.  Comparing
            // the resulting hash to the persisted app hash makes corruption
            // or nondeterministic replay fail closed before consensus starts.
            let app_hash = staged.execute(block);
            if app_hash != block.app_hash {
                return Err(format!(
                    "speculative replay app hash mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.app_hash),
                    hex::encode(app_hash)
                ));
            }
            let commitment = staged
                .preflight_commitment(block)?
                .ok_or_else(|| "speculative replay produced no execution commitment".to_string())?;
            let commitment_root = commitment
                .root()
                .map_err(|error| format!("speculative replay commitment root failed: {error}"))?;
            if commitment_root != block.commitment_root {
                return Err(format!(
                    "speculative replay commitment-root mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.commitment_root),
                    hex::encode(commitment_root)
                ));
            }

            parent_hash = block.hash();
            expected_height = expected_height
                .checked_add(1)
                .ok_or_else(|| "speculative replay height overflows".to_string())?;
        }

        let staged_candidates = staged.candidates.into_inner().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned during speculative replay".to_string()
        })?;
        let mut candidates = self.candidates.lock().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned".to_string()
        })?;
        let additional_candidates = staged_candidates
            .keys()
            .filter(|candidate_hash| !candidates.contains_key(*candidate_hash))
            .count();
        if candidates.len().saturating_add(additional_candidates) > MAX_SPECULATIVE_CANDIDATES {
            return Err(format!(
                "speculative replay would exceed candidate limit {}",
                MAX_SPECULATIVE_CANDIDATES
            ));
        }
        candidates.extend(staged_candidates);
        self.committed_hash = Some(committed_head.hash());
        Ok(())
    }

    fn candidates_read(&self) -> MutexGuard<'_, HashMap<Hash, Candidate>> {
        self.candidates.lock().unwrap_or_else(|_| {
            self.shared.set_state_corrupted();
            panic!("canonical application candidate lock poisoned")
        })
    }

    fn canonical_read(&self) -> RwLockReadGuard<'_, AppState> {
        self.shared.app.read().unwrap_or_else(|_| {
            self.shared.set_state_corrupted();
            panic!("canonical application read lock poisoned")
        })
    }

    fn canonical_write(&self) -> Result<RwLockWriteGuard<'_, AppState>, String> {
        self.shared.app.write().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application write lock poisoned".to_string()
        })
    }

    fn payload_transactions(payload: &[u8]) -> Result<Vec<ConsensusTransaction>, String> {
        AppState::decode_consensus_payload(payload)
    }

    fn payload_hashes(payload: &[u8]) -> Result<HashSet<Hash>, String> {
        Self::payload_transactions(payload)?
            .into_iter()
            .map(|entry| entry.hash().map_err(|error| error.to_string()))
            .collect()
    }

    fn ensure_candidate_capacity(
        candidates: &HashMap<Hash, Candidate>,
        block: &Block,
        canonical_height: u64,
    ) -> Result<(), String> {
        if block.height > canonical_height.saturating_add(MAX_SPECULATIVE_DEPTH) {
            return Err(format!(
                "speculative candidate height {} exceeds committed height {} by more than {}",
                block.height, canonical_height, MAX_SPECULATIVE_DEPTH
            ));
        }

        // Re-validating an already retained candidate must not consume another
        // slot. This is needed while a proposer seals its commitment root and
        // followers repeat preflight on the exact block.
        if candidates.contains_key(&block.hash()) {
            return Ok(());
        }

        if candidates.len() >= MAX_SPECULATIVE_CANDIDATES {
            return Err(format!(
                "speculative candidate limit {} reached; refusing unprotected branch",
                MAX_SPECULATIVE_CANDIDATES
            ));
        }
        Ok(())
    }

    fn committed_head_hash(&self, context: ConsensusContext, height: u64) -> Option<Hash> {
        self.committed_hash
            .or_else(|| (height == 0).then(|| Block::genesis(context).hash()))
    }

    fn candidate_chain_connected(
        candidates: &HashMap<Hash, Candidate>,
        parent_hash: Hash,
        committed_height: u64,
        committed_hash: Option<Hash>,
    ) -> bool {
        let Some(committed_hash) = committed_hash else {
            return false;
        };

        let mut current_hash = parent_hash;
        let mut visited = HashSet::new();
        loop {
            if current_hash == committed_hash {
                return true;
            }
            if !visited.insert(current_hash) {
                return false;
            }

            let Some(candidate) = candidates.get(&current_hash) else {
                return false;
            };
            if candidate.block.hash() != current_hash || candidate.block.height <= committed_height
            {
                return false;
            }
            if candidate.block.parent == committed_hash {
                return candidate.block.height == committed_height.saturating_add(1);
            }

            let Some(parent) = candidates.get(&candidate.block.parent) else {
                return false;
            };
            if candidate.block.height != parent.block.height.saturating_add(1) {
                return false;
            }
            current_hash = candidate.block.parent;
        }
    }

    fn retain_protected_candidates(
        candidates: &mut HashMap<Hash, Candidate>,
        canonical_height: u64,
        protected_roots: &[Hash],
    ) {
        let mut protected = HashSet::new();
        for root in protected_roots {
            let mut current = *root;
            let mut visited = HashSet::new();
            loop {
                if !visited.insert(current) {
                    break;
                }
                let Some(candidate) = candidates.get(&current) else {
                    break;
                };
                if candidate.block.height <= canonical_height {
                    break;
                }
                protected.insert(current);
                current = candidate.block.parent;
            }
        }
        candidates.retain(|candidate_hash, _| protected.contains(candidate_hash));
    }

    /// Ensure that an artifact came from the exact payload/result being
    /// finalized.  The Commitment v2 schema intentionally remains block-local
    /// (the enclosing block supplies height/hash), so this binding is checked
    /// at the application boundary before the bytes cross into storage.
    fn commitment_for_block(
        block: &Block,
        artifacts: &BlockExecutionArtifacts,
    ) -> Result<CommitmentV2, String> {
        if artifacts.height != block.height {
            return Err(format!(
                "execution artifact height {} does not match block height {}",
                artifacts.height, block.height
            ));
        }
        if artifacts.timestamp != block.timestamp {
            return Err(format!(
                "execution artifact timestamp {} does not match block timestamp {}",
                artifacts.timestamp, block.timestamp
            ));
        }

        // A leader executes before filling app_hash and commitment_root,
        // while followers execute the fully formed block. Both hashes are
        // valid observations of the same execution input; neither is part of
        // Commitment v2 itself, so accept only those exact execution phases.
        let mut zero_app_hash_block = block.clone();
        zero_app_hash_block.app_hash = [0u8; 32];
        let mut zero_commitment_block = block.clone();
        zero_commitment_block.commitment_root = [0u8; 32];
        let mut zero_roots_block = zero_commitment_block.clone();
        zero_roots_block.app_hash = [0u8; 32];
        if ![
            block.hash(),
            zero_app_hash_block.hash(),
            zero_commitment_block.hash(),
            zero_roots_block.hash(),
        ]
        .contains(&artifacts.block_hash)
        {
            return Err("execution artifact is bound to a different block result".to_string());
        }

        let entries = Self::payload_transactions(&block.payload)?;
        if artifacts.transactions.len() != entries.len() {
            return Err(format!(
                "execution artifact transaction count {} does not match payload count {}",
                artifacts.transactions.len(),
                entries.len()
            ));
        }

        for (index, (entry, artifact)) in entries
            .iter()
            .zip(artifacts.transactions.iter())
            .enumerate()
        {
            let expected_payload_entry_bytes = bincode::serialize(entry)
                .map_err(|error| format!("failed to encode payload entry: {error}"))?;
            if artifact.payload_entry_bytes != expected_payload_entry_bytes {
                return Err(format!(
                    "execution artifact payload entry mismatch at transaction {}",
                    index
                ));
            }

            let expected_canonical_bytes = match entry {
                ConsensusTransaction::Signed(envelope) => envelope
                    .encoded_bytes()
                    .map_err(|error| format!("failed to encode signed envelope: {error}"))?,
                ConsensusTransaction::System(transaction) => bincode::serialize(transaction)
                    .map_err(|error| format!("failed to encode system transaction: {error}"))?,
            };
            if artifact.canonical_bytes != expected_canonical_bytes {
                return Err(format!(
                    "execution artifact canonical transaction mismatch at transaction {}",
                    index
                ));
            }
            if artifact.receipt.tx_index != index as u32 {
                return Err(format!(
                    "execution artifact receipt index {} does not match transaction {}",
                    artifact.receipt.tx_index, index
                ));
            }
            if artifact.receipt.tx_id != hash(&artifact.canonical_bytes) {
                return Err(format!(
                    "execution artifact transaction ID mismatch at transaction {}",
                    index
                ));
            }
            if artifact.signer != entry.trader_address() {
                return Err(format!(
                    "execution artifact signer mismatch at transaction {}",
                    index
                ));
            }
        }

        artifacts
            .commitment_with_block_events()
            .map_err(|error| format!("invalid execution commitment: {error}"))
    }

    /// Extract a validated commitment from a private execution state.  Taking
    /// the transient artifact from a clone keeps both the canonical state and
    /// the speculative candidate untouched.
    fn commitment_from_state(block: &Block, state: &mut AppState) -> Result<CommitmentV2, String> {
        let artifacts = state
            .take_execution_artifacts()
            .ok_or_else(|| "execution produced no commitment artifact".to_string())?;
        Self::commitment_for_block(block, &artifacts)
    }

    fn candidate_matches_block(candidate: &Candidate, block: &Block) -> bool {
        candidate.block.hash() == block.hash()
            && candidate.block.height == block.height
            && candidate.block.parent == block.parent
            && candidate.block.payload == block.payload
            && candidate.block.proposer == block.proposer
            && candidate.block.app_hash == block.app_hash
            && candidate.block.timestamp == block.timestamp
    }

    fn candidate_matches_precommitment(candidate: &Candidate, block: &Block) -> bool {
        let mut candidate_block = candidate.block.clone();
        let mut precommitment_block = block.clone();
        candidate_block.commitment_root = [0u8; 32];
        precommitment_block.commitment_root = [0u8; 32];
        candidate_block.hash() == precommitment_block.hash()
            && candidate_block.height == block.height
            && candidate_block.parent == block.parent
            && candidate_block.payload == block.payload
            && candidate_block.proposer == block.proposer
            && candidate_block.app_hash == block.app_hash
            && candidate_block.timestamp == block.timestamp
    }

    fn ensure_sealed_commitment_root(
        block: &Block,
        commitment: &CommitmentV2,
    ) -> Result<(), String> {
        let root = commitment
            .root()
            .map_err(|error| format!("invalid execution commitment root: {error}"))?;
        if block.commitment_root != root {
            return Err(format!(
                "execution commitment root mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.commitment_root),
                hex::encode(root)
            ));
        }
        Ok(())
    }

    fn execution_commitment(&self, block: &Block) -> Result<CommitmentV2, String> {
        let candidates = self.candidates_read();
        let canonical = self.canonical_read();
        let canonical_height = canonical.committed_height();
        let committed_hash = self.committed_head_hash(block.context(), canonical_height);

        // Prefer the exact speculative result. In particular, do not execute
        // the block again: the candidate is the result validated for this
        // block input, with or without the final commitment header root.
        if let Some(candidate) = candidates.get(&block.hash()) {
            if !Self::candidate_matches_block(candidate, block) {
                return Err("speculative candidate does not match finalized block".to_string());
            }
            if !Self::candidate_chain_connected(
                &candidates,
                block.hash(),
                canonical_height,
                committed_hash,
            ) {
                return Err("speculative candidate is not connected to canonical head".to_string());
            }
            let artifacts = candidate
                .state
                .execution_artifacts()
                .ok_or_else(|| "execution produced no commitment artifact".to_string())?;
            return Self::commitment_for_block(block, artifacts);
        }

        // Recovery can finalize a block for which no speculative candidate is
        // retained. Execute on a private copy only; this path never mutates
        // canonical state, publishes an event, or inserts a candidate.
        let mut state = if let Some(parent) = candidates.get(&block.parent) {
            if !Self::candidate_chain_connected(
                &candidates,
                block.parent,
                canonical_height,
                committed_hash,
            ) || block.height != parent.block.height.saturating_add(1)
            {
                return Err("block parent is not connected to canonical head".to_string());
            }
            parent.state.clone_for_verified_component_child()
        } else {
            let expected_height = canonical_height.saturating_add(1);
            if block.height != expected_height || Some(block.parent) != committed_hash {
                return Err(format!(
                    "block does not extend canonical head at height {}",
                    expected_height
                ));
            }
            canonical.clone_for_verified_component_child()
        };

        <AppState as AppHook>::validate_block(&state, block)?;
        let app_hash = <AppState as AppHook>::execute(&mut state, block);
        Self::validate_consensus_state(
            &state,
            "consensus-state validation after private commitment replay",
        )?;
        if app_hash != block.app_hash {
            return Err(format!(
                "application hash mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(app_hash)
            ));
        }
        Self::commitment_from_state(block, &mut state)
    }

    fn prune_speculative_branches_for_admission(&mut self, protected_roots: &[Hash]) {
        let canonical_height = self.canonical_read().committed_height();
        let mut candidates = self.candidates.lock().unwrap_or_else(|_| {
            self.shared.set_state_corrupted();
            panic!("canonical application candidate lock poisoned")
        });
        Self::retain_protected_candidates(&mut candidates, canonical_height, protected_roots);
    }

    fn stage_speculative_branch_for_admission(
        &self,
        context: ConsensusContext,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        reserve_slots: usize,
    ) -> Result<(HashMap<Hash, Candidate>, Option<Hash>), String> {
        let original = self
            .candidates
            .lock()
            .map_err(|_| {
                self.shared.set_state_corrupted();
                "canonical application candidate lock poisoned".to_string()
            })?
            .clone();
        let mut staged = Self::new(self.shared.clone());
        staged.committed_hash = self.committed_hash;
        *staged.candidates.lock().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned".to_string()
        })? = original;

        staged.prune_speculative_branches_for_admission(protected_roots);
        if staged.candidate_count().saturating_add(reserve_slots) > MAX_SPECULATIVE_CANDIDATES {
            return Err(format!(
                "speculative candidate limit {} leaves no admission slot",
                MAX_SPECULATIVE_CANDIDATES
            ));
        }
        staged.restore_speculative_chain(context, committed_head, ancestors)?;
        if staged.candidate_count().saturating_add(reserve_slots) > MAX_SPECULATIVE_CANDIDATES {
            return Err(format!(
                "speculative branch restore leaves no admission slot below candidate limit {}",
                MAX_SPECULATIVE_CANDIDATES
            ));
        }

        let committed_hash = staged.committed_hash;
        let staged_candidates = staged.candidates.into_inner().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned during admission".to_string()
        })?;
        Ok((staged_candidates, committed_hash))
    }
}

impl AppHook for CanonicalAppHook {
    fn submit_user_transaction(
        &mut self,
        envelope: crate::app::SignedEnvelope,
        timestamp: u64,
    ) -> Result<Hash, String> {
        self.canonical_write()?
            .submit_envelope_at(envelope, timestamp)
            .map_err(|error| error.to_string())
    }

    fn prepare_payload(&self, parent: &Block) -> Vec<u8> {
        // Always read pending transactions from the canonical state so API
        // submissions made after a proposal are visible to the next leader.
        // A parent candidate contributes the transaction set already proposed
        // on that branch, which is filtered from the canonical mempool.
        let candidates = self.candidates_read();
        let canonical = self.canonical_read();
        let canonical_height = canonical.committed_height();
        let committed_hash = self.committed_head_hash(parent.context(), canonical_height);
        let parent_hash = parent.hash();
        let connected = if Some(parent_hash) == committed_hash {
            parent.height == canonical_height
        } else {
            Self::candidate_chain_connected(
                &candidates,
                parent_hash,
                canonical_height,
                committed_hash,
            )
        };
        if !connected {
            return Vec::new();
        }
        let proposed = candidates
            .get(&parent_hash)
            .map(|candidate| &candidate.proposed_tx_hashes);
        // A child of a speculative candidate must schedule against that
        // branch's account nonces, while still reading the live canonical
        // mempool so API submissions made after the candidate remain visible.
        // Replacing only the transient mempool keeps the branch state and the
        // API-visible queue from being mixed.
        let payload = if let Some(candidate) = candidates.get(&parent_hash) {
            let mut branch = candidate.state.clone();
            branch.mempool = canonical.mempool.clone();
            <AppState as AppHook>::prepare_payload(&branch, parent)
        } else {
            <AppState as AppHook>::prepare_payload(&canonical, parent)
        };
        let txs = Self::payload_transactions(&payload)
            .expect("canonical AppState produced an invalid consensus payload");

        let mut seen = HashSet::new();
        let filtered: Vec<_> = txs
            .into_iter()
            .filter(|entry| {
                let tx_hash = entry
                    .hash()
                    .expect("canonical mempool entry must have bounded encoding");
                proposed
                    .map(|hashes| !hashes.contains(&tx_hash))
                    .unwrap_or(true)
                    && seen.insert(tx_hash)
            })
            .collect();

        bincode::serialize(&filtered).unwrap_or_default()
    }

    fn validate_block(&self, block: &Block) -> Result<(), String> {
        let candidates = self.candidates_read();
        let canonical = self.canonical_read();
        let canonical_height = canonical.committed_height();
        Self::ensure_candidate_capacity(&candidates, block, canonical_height)?;
        let committed_hash = self.committed_head_hash(block.context(), canonical_height);
        let state = if let Some(candidate) = candidates.get(&block.parent) {
            if !Self::candidate_chain_connected(
                &candidates,
                block.parent,
                canonical_height,
                committed_hash,
            ) || block.height != candidate.block.height.saturating_add(1)
            {
                return Err("block parent is not connected to the exact canonical head".to_string());
            }
            candidate.state.clone()
        } else {
            let expected_height = canonical_height.saturating_add(1);
            if block.height != expected_height || Some(block.parent) != committed_hash {
                return Err(format!(
                    "block does not extend the exact canonical head at height {}",
                    expected_height
                ));
            }
            canonical.clone()
        };
        <AppState as AppHook>::validate_block(&state, block)
    }

    fn preflight_block_with_speculative_branch(
        &self,
        context: ConsensusContext,
        block: &Block,
        committed_head: &Block,
        ancestors: &[Block],
    ) -> Result<(), String> {
        // Always use a private hook for this phase.  The live candidate map
        // may already be full, and a malformed proposal must not force an
        // eviction merely to discover that it cannot execute.
        let mut staged = Self::new(self.shared.clone());
        staged.committed_hash = Some(committed_head.hash());
        staged.restore_speculative_chain(context, committed_head, ancestors)?;
        staged.validate_block(block)?;

        let app_hash = staged.execute(block);
        if block.app_hash != [0u8; 32] && app_hash != block.app_hash {
            return Err(format!(
                "application hash mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(app_hash)
            ));
        }

        // Leader proposals enter this private preflight as drafts with zero
        // application and commitment roots.  Execute first, then address the
        // staged candidate by the root it actually produced.  A follower's
        // non-zero authenticated roots are still compared exactly below.
        let mut executed_block = block.clone();
        executed_block.app_hash = app_hash;
        let commitment = staged
            .derive_execution_commitment(&executed_block)?
            .ok_or_else(|| "execution produced no commitment artifact".to_string())?;
        let commitment_root = commitment
            .root()
            .map_err(|error| format!("execution commitment root failed: {error}"))?;
        if block.commitment_root != [0u8; 32] && commitment_root != block.commitment_root {
            return Err(format!(
                "execution commitment root mismatch at height {}",
                block.height
            ));
        }
        let state_root = staged.preflight_state_root(&executed_block)?;
        if state_root != Some(app_hash) {
            return Err(format!(
                "authenticated state-root mismatch at height {}",
                block.height
            ));
        }
        Ok(())
    }

    fn restore_speculative_branch(
        &mut self,
        context: ConsensusContext,
        committed_head: &Block,
        ancestors: &[Block],
    ) -> Result<(), String> {
        if ancestors.is_empty() {
            return Ok(());
        }
        self.restore_speculative_chain(context, committed_head, ancestors)
    }

    fn check_speculative_branch_admission(
        &self,
        context: ConsensusContext,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        reserve_slots: usize,
    ) -> Result<(), String> {
        self.stage_speculative_branch_for_admission(
            context,
            committed_head,
            ancestors,
            protected_roots,
            reserve_slots,
        )
        .map(|_| ())
    }

    fn restore_speculative_branch_for_admission(
        &mut self,
        context: ConsensusContext,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        reserve_slots: usize,
    ) -> Result<(), String> {
        let (staged_candidates, committed_hash) = self.stage_speculative_branch_for_admission(
            context,
            committed_head,
            ancestors,
            protected_roots,
            reserve_slots,
        )?;
        let mut candidates = self.candidates.lock().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned".to_string()
        })?;
        *candidates = staged_candidates;
        self.committed_hash = committed_hash;
        Ok(())
    }

    fn execute(&mut self, block: &Block) -> Hash {
        if let Err(error) = self.validate_block(block) {
            tracing::warn!(height = block.height, error = %error, "Rejected canonical application block");
            return hash(b"invalid-canonical-application-payload");
        }

        let mut candidates = self.candidates_read();

        // A duplicate validation of a fully formed block can reuse its
        // speculative result.  Leader-side execution has a zero app hash and
        // therefore cannot hit this branch until the runner fills it in.
        if block.app_hash != [0u8; 32] && candidates.contains_key(&block.hash()) {
            return block.app_hash;
        }

        let canonical_height = self.canonical_read().committed_height();
        if let Err(error) = Self::ensure_candidate_capacity(&candidates, block, canonical_height) {
            tracing::warn!(
                height = block.height,
                error = %error,
                "Rejected canonical application candidate due to resource limit"
            );
            return Self::invalid_consensus_state_hash();
        }

        // Clone only the parent material needed after execution while the
        // candidate lock is held. In particular, do not clone the whole
        // Candidate and then clone its AppState again for the child branch.
        let (mut state, parent_tree, parent_proposed_tx_hashes) =
            if let Some(parent_candidate) = candidates.get(&block.parent) {
                (
                    parent_candidate.state.clone_for_verified_component_child(),
                    Some(parent_candidate.full_state_tree.clone()),
                    Some(parent_candidate.proposed_tx_hashes.clone()),
                )
            } else {
                let canonical = self.canonical_read();
                let expected_height = canonical.committed_height().saturating_add(1);
                if block.height != expected_height {
                    // A proposal that skips a candidate is invalid from this
                    // node's perspective, but it is not evidence of local state
                    // corruption: a Byzantine proposer can send such a block.
                    // Return a non-matching hash so consensus rejects the vote.
                    return hash(b"canonical-missing-parent-candidate");
                }
                (canonical.clone_for_verified_component_child(), None, None)
            };

        let app_hash = <AppState as AppHook>::execute(&mut state, block);
        if let Err(error) =
            Self::validate_consensus_state(&state, "consensus-state validation after execution")
        {
            tracing::warn!(
                height = block.height,
                error = %error,
                "Rejected canonical application candidate with invalid consensus state"
            );
            return Self::invalid_consensus_state_hash();
        }
        let full_state_tree = state.derive_full_state_tree(parent_tree.as_ref());
        if full_state_tree.root != app_hash {
            tracing::warn!(
                height = block.height,
                expected = %hex::encode(app_hash),
                got = %hex::encode(full_state_tree.root),
                "Rejected canonical application candidate with inconsistent state root"
            );
            return Self::invalid_consensus_state_hash();
        }
        let mut candidate_block = block.clone();
        candidate_block.app_hash = app_hash;
        let candidate_hash = candidate_block.hash();
        let mut proposed_tx_hashes = parent_proposed_tx_hashes.unwrap_or_default();
        proposed_tx_hashes.extend(
            Self::payload_hashes(&block.payload)
                .expect("canonical block payload validated before execution"),
        );

        candidates.insert(
            candidate_hash,
            Candidate {
                block: candidate_block,
                state,
                full_state_tree,
                proposed_tx_hashes,
            },
        );
        app_hash
    }

    fn derive_execution_commitment(&self, block: &Block) -> Result<Option<CommitmentV2>, String> {
        self.execution_commitment(block).map(Some)
    }

    fn preflight_commitment(&self, block: &Block) -> Result<Option<CommitmentV2>, String> {
        let commitment = self.execution_commitment(block)?;
        Self::ensure_sealed_commitment_root(block, &commitment)?;
        Ok(Some(commitment))
    }

    fn seal_execution_commitment(&mut self, block: &Block) -> Result<(), String> {
        if block.height > 0 && block.commitment_root == [0u8; 32] {
            return Err("cannot seal an empty execution commitment root".to_string());
        }

        let mut precommitment_block = block.clone();
        precommitment_block.commitment_root = [0u8; 32];
        let precommitment_hash = precommitment_block.hash();
        let final_hash = block.hash();
        let mut candidates = self.candidates.lock().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned".to_string()
        })?;

        if candidates.contains_key(&final_hash) {
            return Err("execution commitment candidate hash already exists".to_string());
        }
        let mut candidate = candidates.remove(&precommitment_hash).ok_or_else(|| {
            "speculative candidate for pre-commitment block was not found".to_string()
        })?;
        if !Self::candidate_matches_precommitment(&candidate, block) {
            self.shared.set_state_corrupted();
            candidates.insert(precommitment_hash, candidate);
            return Err("speculative candidate does not match commitment seal".to_string());
        }
        candidate.block = block.clone();
        candidates.insert(final_hash, candidate);
        Ok(())
    }

    fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
        let candidates = self.candidates_read();
        let canonical = self.canonical_read();
        let canonical_height = canonical.committed_height();
        let committed_hash = self.committed_head_hash(block.context(), canonical_height);

        // Prefer the exact speculative candidate. This is the same state that
        // was executed and used for the app hash; do not replay it a second
        // time merely to produce the authenticated full-state root.
        if let Some(candidate) = candidates.get(&block.hash()) {
            if !Self::candidate_matches_block(candidate, block) {
                return Err("speculative candidate does not match finalized block".to_string());
            }
            if !Self::candidate_chain_connected(
                &candidates,
                block.hash(),
                canonical_height,
                committed_hash,
            ) {
                return Err("speculative candidate is not connected to canonical head".to_string());
            }
            Self::validate_consensus_state(
                &candidate.state,
                "consensus-state validation before state-root preflight",
            )?;
            let fresh_tree = candidate.state.compute_full_state_tree_fresh();
            if fresh_tree != candidate.full_state_tree {
                return Err(
                    "speculative candidate full-state tree changed after execution".to_string(),
                );
            }
            if fresh_tree.root != block.app_hash {
                return Err(format!(
                    "speculative candidate state-root mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.app_hash),
                    hex::encode(fresh_tree.root)
                ));
            }
            return Ok(Some(fresh_tree.root));
        }

        // Recovery/direct callers may not retain a candidate. Rebuild on a
        // private state only; canonical state and speculative candidates stay
        // untouched until the later commit callback.
        let mut state = if let Some(parent) = candidates.get(&block.parent) {
            if !Self::candidate_chain_connected(
                &candidates,
                block.parent,
                canonical_height,
                committed_hash,
            ) || block.height != parent.block.height.saturating_add(1)
            {
                return Err("block parent is not connected to canonical head".to_string());
            }
            parent.state.clone_for_verified_component_child()
        } else {
            let expected_height = canonical_height.saturating_add(1);
            if block.height != expected_height || Some(block.parent) != committed_hash {
                return Err(format!(
                    "block does not extend canonical head at height {}",
                    expected_height
                ));
            }
            canonical.clone_for_verified_component_child()
        };

        <AppState as AppHook>::validate_block(&state, block)?;
        let app_hash = <AppState as AppHook>::execute(&mut state, block);
        Self::validate_consensus_state(
            &state,
            "consensus-state validation after private state-root replay",
        )?;
        if app_hash != block.app_hash {
            return Err(format!(
                "application hash mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(app_hash)
            ));
        }
        let fresh_tree = state.compute_full_state_tree_fresh();
        if fresh_tree.root != block.app_hash {
            return Err(format!(
                "state-root mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(fresh_tree.root)
            ));
        }
        Ok(Some(fresh_tree.root))
    }

    fn commit(&mut self, block: &Block) -> Result<Hash, String> {
        // Keep the candidate lock first, matching execute/prepare lock order;
        // this prevents a speculative execution and commit from deadlocking.
        let mut candidates = self.candidates.lock().map_err(|_| {
            self.shared.set_state_corrupted();
            "canonical application candidate lock poisoned".to_string()
        })?;
        let mut canonical = self.canonical_write()?;
        let expected_height = canonical.committed_height().saturating_add(1);
        if block.height != expected_height {
            return Err(format!(
                "non-sequential application commit: expected height {}, got {}",
                expected_height, block.height
            ));
        }
        let expected_parent =
            self.committed_head_hash(block.context(), canonical.committed_height());
        if expected_parent != Some(block.parent) {
            return Err("application commit does not extend the exact canonical head".to_string());
        }

        // Reuse the exact speculative result that preflight validated.  This
        // keeps the persisted commitment and canonical state tied to one
        // execution, avoiding a second ambiguous replay of a finalized
        // candidate.  Recovery/direct callers without a candidate retain the
        // deterministic private replay fallback.
        let mut next = if let Some(candidate) = candidates.get(&block.hash()) {
            if !Self::candidate_matches_block(candidate, block) {
                self.shared.set_state_corrupted();
                return Err("speculative candidate does not match finalized block".to_string());
            }
            if let Err(error) = Self::validate_consensus_state(
                &candidate.state,
                "consensus-state validation before commit",
            ) {
                self.shared.set_state_corrupted();
                return Err(error);
            }
            let fresh_tree = candidate.state.compute_full_state_tree_fresh();
            if fresh_tree != candidate.full_state_tree {
                self.shared.set_state_corrupted();
                return Err(
                    "speculative candidate full-state tree changed before commit".to_string(),
                );
            }
            if fresh_tree.root != block.app_hash {
                self.shared.set_state_corrupted();
                return Err(format!(
                    "speculative candidate state-root mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.app_hash),
                    hex::encode(fresh_tree.root)
                ));
            }
            let artifacts = candidate.state.execution_artifacts().ok_or_else(|| {
                self.shared.set_state_corrupted();
                "execution produced no commitment artifact".to_string()
            })?;
            let commitment = Self::commitment_for_block(block, artifacts).map_err(|error| {
                self.shared.set_state_corrupted();
                error
            })?;
            Self::ensure_sealed_commitment_root(block, &commitment).map_err(|error| {
                self.shared.set_state_corrupted();
                error
            })?;
            candidate.state.clone()
        } else {
            let mut next = canonical.clone_for_verified_component_child();
            <AppState as AppHook>::validate_block(&next, block)?;
            let app_hash = <AppState as AppHook>::execute(&mut next, block);
            if let Err(error) = Self::validate_consensus_state(
                &next,
                "consensus-state validation after private commit replay",
            ) {
                self.shared.set_state_corrupted();
                return Err(error);
            }
            if app_hash != block.app_hash {
                self.shared.set_state_corrupted();
                return Err(format!(
                    "application hash mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.app_hash),
                    hex::encode(app_hash)
                ));
            }
            let fresh_tree = next.compute_full_state_tree_fresh();
            if fresh_tree.root != block.app_hash {
                self.shared.set_state_corrupted();
                return Err(format!(
                    "state-root mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(block.app_hash),
                    hex::encode(fresh_tree.root)
                ));
            }
            let commitment = Self::commitment_from_state(block, &mut next).map_err(|error| {
                self.shared.set_state_corrupted();
                error
            })?;
            Self::ensure_sealed_commitment_root(block, &commitment).map_err(|error| {
                self.shared.set_state_corrupted();
                error
            })?;
            next
        };
        next.reconcile_canonical_mempool(&canonical, block)
            .map_err(|error| {
                self.shared.set_state_corrupted();
                format!("canonical mempool reconciliation failed: {error}")
            })?;

        // Execution artifacts are transient and have already been validated
        // by preflight before the runner reaches commit.  Do not retain them
        // in canonical state, otherwise every future speculative state clone
        // needlessly copies the full artifact bundle.  Apply this to the
        // private replay fallback as well as the matching candidate path.
        next.clear_execution_artifacts();
        let app_hash = block.app_hash;

        *canonical = next;
        drop(canonical);
        self.committed_hash = Some(block.hash());

        // Keep only descendants that extend the newly committed block.  This
        // removes conflicting branches as well as stale ancestors; merely
        // filtering by height would leave conflicting descendants available
        // for a later proposal.
        let committed_hash = block.hash();
        let mut descendants = HashSet::new();
        loop {
            let newly_found: Vec<Hash> = candidates
                .iter()
                .filter(|(candidate_hash, candidate)| {
                    !descendants.contains(*candidate_hash)
                        && (candidate.block.parent == committed_hash
                            || descendants.contains(&candidate.block.parent))
                })
                .map(|(candidate_hash, _)| *candidate_hash)
                .collect();
            if newly_found.is_empty() {
                break;
            }
            descendants.extend(newly_found);
        }
        candidates.retain(|candidate_hash, _| descendants.contains(candidate_hash));
        Ok(app_hash)
    }

    fn on_durable_commit(
        &mut self,
        block: &Block,
        commitment: &CommitmentV2,
    ) -> Result<(), String> {
        commitment
            .validate()
            .map_err(|error| format!("invalid committed execution artifact: {error}"))?;
        let commitment_root = commitment
            .root()
            .map_err(|error| format!("invalid committed execution root: {error}"))?;
        if commitment_root != block.commitment_root {
            return Err("committed execution artifact does not match block root".to_string());
        }
        let entries = Self::payload_transactions(&block.payload)?;
        if entries.len() != commitment.receipts.len() {
            return Err(format!(
                "committed payload count {} does not match receipt count {}",
                entries.len(),
                commitment.receipts.len()
            ));
        }

        for (entry, receipt) in entries.iter().zip(&commitment.receipts) {
            let tx_id = entry.hash().map_err(|error| error.to_string())?;
            if receipt.tx_id != tx_id {
                return Err(format!(
                    "committed receipt transaction ID mismatch at index {}",
                    receipt.tx_index
                ));
            }

            // Protocol-owned system entries have no globally unique signed
            // envelope identity and therefore no private transaction
            // lifecycle subscription.
            if !matches!(entry, ConsensusTransaction::Signed(_)) {
                continue;
            }

            let events = receipt
                .events
                .iter()
                .map(|event| FinalizedReceiptEvent {
                    event_index: event.event_index,
                    event_type: event.event_type.0,
                    payload_hex: hex::encode(&event.payload),
                })
                .collect();
            self.shared.broadcast_committed_user_event(
                &entry.trader_address(),
                UserEvent::TransactionFinalized {
                    tx_hash: hex::encode(receipt.tx_id),
                    block_height: block.height,
                    block_hash: hex::encode(block.hash()),
                    tx_index: receipt.tx_index,
                    tx_type: receipt.tx_type.0,
                    status: receipt.status.0,
                    error_code: receipt.error_code.0,
                    compute_units: receipt.resource_usage.compute_units,
                    storage_read_bytes: receipt.resource_usage.storage_read_bytes,
                    storage_write_bytes: receipt.resource_usage.storage_write_bytes,
                    events,
                },
            );
        }

        self.shared.broadcast(Event::BlockCommitted {
            height: block.height,
            hash: hex::encode(block.hash()),
            tx_count: entries.len(),
        });
        Ok(())
    }

    fn prune_speculative_branches(&mut self, protected_roots: &[Hash]) {
        let canonical_height = self.canonical_read().committed_height();
        let mut candidates = self.candidates.lock().unwrap_or_else(|_| {
            self.shared.set_state_corrupted();
            panic!("canonical application candidate lock poisoned")
        });
        // Preserve still-possibly-certified proposals while there is room.
        // Admission uses the explicit atomic boundary below when it needs to
        // reserve space for a restored ancestor closure.
        if candidates.len() < MAX_SPECULATIVE_CANDIDATES {
            return;
        }
        Self::retain_protected_candidates(&mut candidates, canonical_height, protected_roots);
    }

    fn validate_recovery_head(&self, block: &Block) -> Result<(), String> {
        block.validate()?;

        let canonical = self.canonical_read();
        if canonical.chain_domain() != block.genesis_hash {
            return Err(
                "recovered application chain domain does not match consensus head".to_string(),
            );
        }
        if canonical.current_epoch() != block.epoch {
            return Err(format!(
                "recovered application epoch {} does not match consensus head epoch {}",
                canonical.current_epoch(),
                block.epoch
            ));
        }
        if canonical.pending_validator_update().is_some() {
            return Err(
                "recovered static application contains a pending validator update".to_string(),
            );
        }
        let canonical_height = canonical.committed_height();
        if canonical_height != block.height {
            return Err(format!(
                "recovered application height {} does not match consensus head {}",
                canonical_height, block.height
            ));
        }
        if block.height == 0 {
            return Ok(());
        }
        if self.committed_hash != Some(block.hash()) {
            return Err(
                "recovered application block hash does not match consensus head".to_string(),
            );
        }
        Self::validate_consensus_state(
            &canonical,
            "consensus-state validation at recovered application head",
        )?;
        let fresh_tree = canonical.compute_full_state_tree_fresh();
        if fresh_tree.root != block.app_hash {
            return Err(format!(
                "recovered application state root mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(fresh_tree.root)
            ));
        }
        Ok(())
    }

    fn take_validator_update(&mut self) -> Option<crate::app::staking::ValidatorSetUpdate> {
        let mut canonical = self.canonical_write().ok()?;
        canonical.take_pending_validator_update()
    }

    fn validator_set_update_for_transition(
        &self,
        finalized_block: &Block,
    ) -> Result<Option<crate::app::staking::ValidatorSetUpdate>, String> {
        finalized_block.validate()?;
        let canonical = self.canonical_read();
        if finalized_block.genesis_hash != canonical.chain_domain() {
            return Err(
                "transition block chain domain does not match canonical application".to_string(),
            );
        }
        if finalized_block.height != canonical.committed_height() {
            return Err(format!(
                "transition block height {} does not match canonical head {}",
                finalized_block.height,
                canonical.committed_height()
            ));
        }
        let expected_next_epoch = finalized_block
            .epoch
            .checked_add(1)
            .ok_or_else(|| "transition next epoch overflows u64".to_string())?;
        if expected_next_epoch != canonical.current_epoch() {
            return Err(format!(
                "canonical epoch {} is not finalized transition epoch {} + 1",
                canonical.current_epoch(),
                finalized_block.epoch,
            ));
        }
        if self.committed_hash != Some(finalized_block.hash()) {
            return Err("transition block is not the exact canonical application head".to_string());
        }
        Self::validate_consensus_state(&canonical, "canonical validator-set transition binding")?;
        if canonical.compute_full_state_root() != finalized_block.app_hash {
            return Err(
                "transition block app hash does not match canonical application".to_string(),
            );
        }

        // The update is intentionally transient.  Refuse to stage when the
        // exact result of this finalized transition is no longer available;
        // deriving a fresh set could bind a proof to later application state.
        Ok(canonical.pending_validator_update().cloned())
    }

    fn submit_equivocation_evidence(&mut self, proof: EquivocationProof) -> bool {
        // Network arrival is nondeterministic.  Only enqueue the verified
        // proof in the node-local mempool; staking state changes happen when
        // the resulting system transaction is executed from a block.
        let Ok(mut canonical) = self.canonical_write() else {
            return false;
        };
        canonical.enqueue_equivocation_evidence(proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ConsensusTransaction, OrderType, Side, SignedEnvelope, Transaction};
    use crate::consensus::{AppHook, EquivocationProof};
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::{Certificate, ConsensusConfig, ConsensusContext, NodeId};

    fn block(
        context: ConsensusContext,
        parent: &Block,
        height: u64,
        view: u64,
        payload: Vec<u8>,
    ) -> Block {
        Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            height,
            parent: parent.hash(),
            payload,
            proposer: [7u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: view + 1,
            justify: None,
        }
    }

    fn finalized(hook: &mut CanonicalAppHook, mut block: Block) -> Block {
        block.app_hash = hook.execute(&block);
        let precommitment_hash = block.hash();
        let commitment = hook
            .derive_execution_commitment(&block)
            .expect("execution commitment preflight")
            .expect("execution commitment");
        block.commitment_root = commitment.root().expect("execution commitment root");
        hook.seal_execution_commitment(&block)
            .expect("execution commitment seal");
        assert_ne!(precommitment_hash, block.hash());
        block
    }

    fn payload(tx: Transaction) -> Vec<u8> {
        bincode::serialize(&vec![ConsensusTransaction::System(tx)]).unwrap()
    }

    fn context() -> ConsensusContext {
        ConsensusConfig::single_node().context().unwrap()
    }

    fn evidence_context() -> ConsensusContext {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [9u8; 32];
        config.context().unwrap()
    }

    fn equivocation_fixture() -> (AppState, EquivocationProof, NodeId) {
        let context = evidence_context();
        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        state.set_consensus_context(context);

        let node_id = [9u8; 32];
        let mut seed = [0u8; 32];
        seed[0] = 9;
        let secret = BlsSecretKey::from_seed(&seed);
        state
            .accounts_mut()
            .withdraw_hyck(
                crate::app::staking::HYCK_TREASURY_ADDRESS,
                crate::app::staking::MIN_SELF_STAKE,
            )
            .unwrap();
        state
            .staking
            .register_validator(
                "validator".to_string(),
                node_id,
                secret.public_key().to_bytes().to_vec(),
                secret
                    .create_proof_of_possession(&context.genesis_hash, &node_id)
                    .to_bytes()
                    .to_vec(),
                context.genesis_hash,
                crate::app::staking::MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        let view = 17;
        let hash_a = [1u8; 32];
        let app_hash_a = [0x11u8; 32];
        let hash_b = [2u8; 32];
        let app_hash_b = [0x22u8; 32];
        let signature_a = secret
            .sign(&Certificate::build_signing_message(
                context,
                view,
                &hash_a,
                &app_hash_a,
            ))
            .to_bytes()
            .to_vec();
        let signature_b = secret
            .sign(&Certificate::build_signing_message(
                context,
                view,
                &hash_b,
                &app_hash_b,
            ))
            .to_bytes()
            .to_vec();

        let proof = EquivocationProof {
            context,
            offender: node_id,
            view,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a,
            signature_b,
        };
        (state, proof, node_id)
    }

    #[test]
    fn equivocation_ingress_accepts_valid_proof_without_mutating_canonical_state() {
        let (state, proof, node_id) = equivocation_fixture();
        let shared = SharedState::new(state);
        let before_root = shared.app.read().unwrap().compute_full_state_root();
        let before_status = shared
            .app
            .read()
            .unwrap()
            .staking
            .get_validator_by_node(&node_id)
            .unwrap()
            .status;
        let mut hook = CanonicalAppHook::new(shared.clone());

        assert!(hook.submit_equivocation_evidence(proof));
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.compute_full_state_root(), before_root);
        assert_eq!(
            canonical
                .staking
                .get_validator_by_node(&node_id)
                .unwrap()
                .status,
            before_status
        );
        assert_eq!(canonical.mempool_stats(), (1, 0, 0));
    }

    #[test]
    fn equivocation_ingress_rejects_forged_proof() {
        let (state, mut proof, node_id) = equivocation_fixture();
        proof.signature_a[0] ^= 1;
        let mut state = state;
        let forged_evidence = state.equivocation_evidence_from_proof(&proof);
        assert!(state
            .submit_tx(Transaction::SubmitEvidence {
                submitter: "attacker".to_string(),
                evidence: forged_evidence,
            })
            .is_err());
        let shared = SharedState::new(state);
        let before_root = shared.app.read().unwrap().compute_full_state_root();
        let mut hook = CanonicalAppHook::new(shared.clone());

        assert!(!hook.submit_equivocation_evidence(proof));
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.compute_full_state_root(), before_root);
        assert_eq!(canonical.mempool_stats(), (0, 0, 0));
        assert!(
            canonical
                .staking
                .get_validator_by_node(&node_id)
                .unwrap()
                .status
                != crate::app::staking::ValidatorStatus::Tombstoned
        );
    }

    #[test]
    fn duplicate_equivocation_ingress_is_harmless_and_slashes_once_on_commit() {
        let (state, mut proof, node_id) = equivocation_fixture();
        let context = proof.context;
        let shared = SharedState::new(state);
        let mut hook = CanonicalAppHook::new(shared.clone());
        assert!(hook.submit_equivocation_evidence(proof.clone()));
        let first_hash = shared
            .app
            .read()
            .unwrap()
            .mempool
            .peek_consensus_block_txs(1)[0]
            .hash()
            .unwrap();
        std::mem::swap(&mut proof.hash_a, &mut proof.hash_b);
        std::mem::swap(&mut proof.app_hash_a, &mut proof.app_hash_b);
        std::mem::swap(&mut proof.signature_a, &mut proof.signature_b);
        assert!(hook.submit_equivocation_evidence(proof));
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.mempool_stats(), (1, 0, 0));
        assert_eq!(
            canonical.mempool.peek_consensus_block_txs(1)[0]
                .hash()
                .unwrap(),
            first_hash
        );
        drop(canonical);

        let genesis = Block::genesis(context);
        let txs = shared
            .app
            .read()
            .unwrap()
            .mempool
            .peek_consensus_block_txs(1);
        let payload = bincode::serialize(&txs).unwrap();
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, payload));
        hook.commit(&committed).unwrap();

        let canonical = shared.app.read().unwrap();
        assert_eq!(
            canonical
                .staking
                .get_validator_by_node(&node_id)
                .unwrap()
                .status,
            crate::app::staking::ValidatorStatus::Tombstoned
        );
        assert_eq!(canonical.mempool_stats(), (0, 0, 0));
    }

    #[test]
    fn valid_evidence_is_validated_by_proposer_and_follower_before_commit() {
        let (state, proof, node_id) = equivocation_fixture();
        let mut follower_state = state.clone();
        let context = proof.context;

        // The follower observed a different valid vote pair for the same
        // offender/context.  Its local proof hash therefore differs from the
        // proposer payload, but the canonical key is the same.
        let mut alternate_proof = proof.clone();
        alternate_proof.hash_b = [3u8; 32];
        alternate_proof.app_hash_b = [0x33u8; 32];
        let mut seed = [0u8; 32];
        seed[0] = 9;
        let secret = BlsSecretKey::from_seed(&seed);
        alternate_proof.signature_b = secret
            .sign(&Certificate::build_signing_message(
                context,
                alternate_proof.view,
                &alternate_proof.hash_b,
                &alternate_proof.app_hash_b,
            ))
            .to_bytes()
            .to_vec();
        assert!(follower_state.enqueue_equivocation_evidence(alternate_proof));

        let proposer_shared = SharedState::new(state);
        let mut proposer = CanonicalAppHook::new(proposer_shared.clone());
        assert!(proposer.submit_equivocation_evidence(proof));

        let payload = bincode::serialize(
            &proposer_shared
                .app
                .read()
                .unwrap()
                .mempool
                .peek_consensus_block_txs(1),
        )
        .unwrap();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut proposer, block(context, &genesis, 1, 1, payload));

        let follower_shared = SharedState::new(follower_state);
        let mut follower = CanonicalAppHook::new(follower_shared.clone());
        assert!(follower.validate_block(&committed).is_ok());
        follower.commit(&committed).unwrap();
        assert_eq!(
            follower_shared
                .app
                .read()
                .unwrap()
                .staking
                .get_validator_by_node(&node_id)
                .unwrap()
                .status,
            crate::app::staking::ValidatorStatus::Tombstoned
        );
        assert_eq!(
            follower_shared.app.read().unwrap().mempool_stats(),
            (0, 0, 0)
        );
    }

    #[test]
    fn malformed_system_evidence_block_is_rejected_without_state_mutation() {
        enum Malformation {
            Submitter,
            Timestamp,
            ReversedTuple,
            ForgedSignature,
        }

        for malformation in [
            Malformation::Submitter,
            Malformation::Timestamp,
            Malformation::ReversedTuple,
            Malformation::ForgedSignature,
        ] {
            let (state, proof, _node_id) = equivocation_fixture();
            let context = proof.context;
            let mut evidence = state.equivocation_evidence_from_proof(&proof);
            match malformation {
                Malformation::Submitter => {
                    let submitter = "attacker".to_string();
                    let tx = Transaction::SubmitEvidence {
                        submitter,
                        evidence,
                    };
                    assert_rejected_system_evidence_block(state, context, tx);
                }
                Malformation::Timestamp => {
                    evidence.timestamp = 1;
                    let tx = Transaction::SubmitEvidence {
                        submitter: format!(
                            "system:equivocation:{}",
                            hex::encode(evidence.offender)
                        ),
                        evidence,
                    };
                    assert_rejected_system_evidence_block(state, context, tx);
                }
                Malformation::ReversedTuple => {
                    std::mem::swap(&mut evidence.hash_a, &mut evidence.hash_b);
                    std::mem::swap(&mut evidence.app_hash_a, &mut evidence.app_hash_b);
                    std::mem::swap(&mut evidence.signature_a, &mut evidence.signature_b);
                    let tx = Transaction::SubmitEvidence {
                        submitter: format!(
                            "system:equivocation:{}",
                            hex::encode(evidence.offender)
                        ),
                        evidence,
                    };
                    assert_rejected_system_evidence_block(state, context, tx);
                }
                Malformation::ForgedSignature => {
                    evidence.signature_a[0] ^= 1;
                    let tx = Transaction::SubmitEvidence {
                        submitter: format!(
                            "system:equivocation:{}",
                            hex::encode(evidence.offender)
                        ),
                        evidence,
                    };
                    assert_rejected_system_evidence_block(state, context, tx);
                }
            }
        }
    }

    #[test]
    fn signed_evidence_is_rejected_at_admission_and_block_validation() {
        let (mut state, proof, _node_id) = equivocation_fixture();
        let context = proof.context;
        let signer = crate::crypto::Signer::generate();
        let submitter = format!("{:?}", signer.address());
        let evidence = state.equivocation_evidence_from_proof(&proof);
        let envelope = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::SubmitEvidence {
                submitter,
                evidence,
            },
        )
        .unwrap();
        assert!(state.submit_envelope_at(envelope.clone(), 2).is_err());

        let shared = SharedState::new(state);
        let hook = CanonicalAppHook::new(shared);
        let genesis = Block::genesis(context);
        let payload = bincode::serialize(&vec![ConsensusTransaction::Signed(envelope)]).unwrap();
        assert!(hook
            .validate_block(&block(context, &genesis, 1, 1, payload))
            .is_err());
    }

    #[test]
    fn valid_proof_with_bad_reserved_fields_is_rejected_from_mempool() {
        let (mut state, proof, _node_id) = equivocation_fixture();
        let valid = state.equivocation_evidence_from_proof(&proof);
        let mut bad_timestamp = valid.clone();
        bad_timestamp.timestamp = 1;

        let mut reversed = valid.clone();
        std::mem::swap(&mut reversed.hash_a, &mut reversed.hash_b);
        std::mem::swap(&mut reversed.app_hash_a, &mut reversed.app_hash_b);
        std::mem::swap(&mut reversed.signature_a, &mut reversed.signature_b);

        for (submitter, evidence) in [
            ("attacker".to_string(), valid.clone()),
            (
                format!("system:equivocation:{}", hex::encode(valid.offender)),
                bad_timestamp,
            ),
            (
                format!("system:equivocation:{}", hex::encode(reversed.offender)),
                reversed,
            ),
        ] {
            assert!(state
                .submit_tx(Transaction::SubmitEvidence {
                    submitter,
                    evidence,
                })
                .is_err());
        }
        assert_eq!(state.mempool_stats(), (0, 0, 0));
    }

    #[test]
    fn valid_equivocation_evidence_survives_stale_pruning() {
        let (mut state, proof, _node_id) = equivocation_fixture();
        state.timestamp = 0;
        assert!(state.enqueue_equivocation_evidence(proof.clone()));
        state
            .mempool
            .add(
                Transaction::Deposit {
                    trader: "ordinary".to_string(),
                    amount: 1,
                },
                0,
            )
            .unwrap();

        assert_eq!(state.mempool.prune_stale(u64::MAX), 1);
        assert_eq!(state.mempool.len(), 1);
        let evidence = state.equivocation_evidence_from_proof(&proof);
        assert!(state
            .mempool
            .find_equivocation_evidence_hash(&evidence)
            .is_some());
    }

    fn assert_rejected_system_evidence_block(
        state: AppState,
        context: ConsensusContext,
        transaction: Transaction,
    ) {
        let shared = SharedState::new(state);
        let before_root = shared.app.read().unwrap().compute_full_state_root();
        let hook = CanonicalAppHook::new(shared.clone());
        let genesis = Block::genesis(context);
        let proposed = block(context, &genesis, 1, 1, payload(transaction));
        assert!(hook.validate_block(&proposed).is_err());
        assert_eq!(
            shared.app.read().unwrap().compute_full_state_root(),
            before_root
        );
        assert_eq!(hook.candidate_count(), 0);
    }

    fn signed_payload(
        domain: [u8; 32],
        signer: &crate::crypto::Signer,
        nonce: u64,
        valid_until: u64,
        action: Transaction,
    ) -> Vec<u8> {
        let envelope = SignedEnvelope::sign(domain, signer, nonce, 0, valid_until, action).unwrap();
        bincode::serialize(&vec![ConsensusTransaction::Signed(envelope)]).unwrap()
    }

    #[test]
    fn proposal_validation_rejects_legacy_malformed_bad_signature_expiry_and_nonce() {
        let context = context();
        let genesis = Block::genesis(context);
        let legacy = bincode::serialize(&vec![Transaction::Deposit {
            trader: "alice".to_string(),
            amount: 1,
        }])
        .unwrap();
        let shared = SharedState::new(AppState::new_with_chain_domain_and_dev(
            context.genesis_hash,
            false,
        ));
        let hook = CanonicalAppHook::new(shared);
        assert!(hook
            .validate_block(&block(context, &genesis, 1, 1, legacy))
            .is_err());
        assert!(hook
            .validate_block(&block(context, &genesis, 1, 2, vec![0xff, 0x01, 0x02],))
            .is_err());

        let signer = crate::crypto::Signer::generate();
        let action = Transaction::PlaceOrder {
            trader: format!("{:?}", signer.address()),
            symbol: "BTC-USDT".to_string(),
            side: Side::Bid,
            price: 5_000_000,
            size: 1,
            order_type: OrderType::Gtc,
            reduce_only: false,
        };
        let mut bad =
            SignedEnvelope::sign(context.genesis_hash, &signer, 0, 0, 100, action.clone()).unwrap();
        bad.signature[0] ^= 1;
        let bad_payload = bincode::serialize(&vec![ConsensusTransaction::Signed(bad)]).unwrap();
        assert!(hook
            .validate_block(&block(context, &genesis, 1, 3, bad_payload))
            .is_err());

        assert!(hook
            .validate_block(&block(
                context,
                &genesis,
                1,
                4,
                signed_payload(context.genesis_hash, &signer, 0, 1, action.clone()),
            ))
            .is_err());
        assert!(hook
            .validate_block(&block(
                context,
                &genesis,
                1,
                5,
                signed_payload(context.genesis_hash, &signer, 1, 100, action),
            ))
            .is_err());

        // Two envelopes from the same signer cannot consume the same nonce
        // in one block.  Validation must reject the second entry using the
        // trial state advanced by the first entry.
        let first = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: format!("{:?}", signer.address()),
                amount: 1,
            },
        )
        .unwrap();
        let second = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: format!("{:?}", signer.address()),
                amount: 2,
            },
        )
        .unwrap();
        let duplicate_payload = bincode::serialize(&vec![
            ConsensusTransaction::Signed(first),
            ConsensusTransaction::Signed(second),
        ])
        .unwrap();
        assert!(hook
            .validate_block(&block(context, &genesis, 1, 6, duplicate_payload))
            .is_err());
    }

    #[test]
    fn proposal_validation_accepts_failed_action_and_consumes_nonce_on_execution_copy() {
        let context = context();
        let genesis = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let action = Transaction::Withdraw {
            trader: format!("{:?}", signer.address()),
            amount: 1,
        };
        let shared = SharedState::new(AppState::new_with_chain_domain_and_dev(
            context.genesis_hash,
            false,
        ));
        let mut hook = CanonicalAppHook::new(shared.clone());
        let proposed = block(
            context,
            &genesis,
            1,
            50,
            signed_payload(context.genesis_hash, &signer, 0, 100, action),
        );
        assert!(hook.validate_block(&proposed).is_ok());
        let finalized = finalized(&mut hook, proposed);
        assert!(hook.commit(&finalized).is_ok());
        let canonical = shared.app.read().unwrap();
        assert_eq!(
            canonical
                .accounts()
                .get_nonce(&format!("{:?}", signer.address())),
            1
        );
    }

    #[test]
    fn speculative_execution_is_not_visible_until_commit() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let transaction = Transaction::Deposit {
            trader: "alice".to_string(),
            amount: 10,
        };
        shared
            .app
            .write()
            .unwrap()
            .submit_tx(transaction.clone())
            .unwrap();

        let proposed = block(context, &genesis, 1, 1, payload(transaction));
        let committed = finalized(&mut hook, proposed);

        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), 0);
        assert!(canonical.account("alice").is_none());
        assert_eq!(canonical.mempool_stats(), (1, 0, 0));
        drop(canonical);

        hook.commit(&committed).unwrap();
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), 1);
        assert_eq!(canonical.account("alice").unwrap().balance, 10);
        assert_eq!(canonical.mempool_stats(), (0, 0, 0));
        assert!(canonical.execution_artifacts().is_none());
    }

    #[test]
    fn commit_preserves_mempool_submissions_received_after_speculative_execution() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let committed_tx = Transaction::Deposit {
            trader: "alice".to_string(),
            amount: 10,
        };
        let later_tx = Transaction::Deposit {
            trader: "bob".to_string(),
            amount: 20,
        };

        shared
            .app
            .write()
            .unwrap()
            .submit_tx(committed_tx.clone())
            .unwrap();
        let proposed = block(context, &genesis, 1, 1, payload(committed_tx));
        let finalized = finalized(&mut hook, proposed);

        shared
            .app
            .write()
            .unwrap()
            .submit_tx(later_tx.clone())
            .unwrap();
        hook.commit(&finalized).unwrap();

        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.mempool_stats(), (1, 0, 0));
        assert!(canonical.account("bob").is_none());
        drop(canonical);

        let next_payload = hook.prepare_payload(&finalized);
        let next_transactions = CanonicalAppHook::payload_transactions(&next_payload).unwrap();
        assert_eq!(next_transactions.len(), 1);
        assert!(matches!(
            &next_transactions[0],
            ConsensusTransaction::System(Transaction::Deposit { trader, amount })
                if trader == "bob" && *amount == 20
        ));
    }

    #[test]
    fn preflight_reads_finalized_candidate_without_publishing_or_reexecuting() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        let proposed = block(
            context,
            &genesis,
            1,
            1,
            payload(Transaction::Deposit {
                trader: "alice".to_string(),
                amount: 10,
            }),
        );
        let finalized = finalized(&mut hook, proposed);
        let candidate_count = hook.candidate_count();

        let first = hook
            .preflight_commitment(&finalized)
            .expect("candidate preflight")
            .expect("canonical app must produce a commitment");
        let second = hook
            .preflight_commitment(&finalized)
            .expect("candidate preflight")
            .expect("canonical app must produce a commitment");

        assert_eq!(first, second);
        assert_eq!(first.receipts.len(), 1);
        assert_eq!(hook.candidate_count(), candidate_count);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn state_root_preflight_uses_exact_candidate_without_publishing() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        let proposed = block(
            context,
            &genesis,
            1,
            1,
            payload(Transaction::Deposit {
                trader: "alice".to_string(),
                amount: 10,
            }),
        );
        let finalized = finalized(&mut hook, proposed);
        let candidates = hook.candidates.lock().unwrap();
        let candidate = candidates.get(&finalized.hash()).unwrap();
        assert_eq!(
            candidate.state.full_state_dirty(),
            crate::app::state::full_state_hash::COMPONENT_DIRTY_NONE
        );
        let expected = candidate.full_state_tree.root;
        drop(candidates);

        assert_eq!(hook.preflight_state_root(&finalized), Ok(Some(expected)));
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn state_root_preflight_rejects_changed_candidate_state() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared);
        let context = context();
        let genesis = Block::genesis(context);
        let finalized = finalized(
            &mut hook,
            block(
                context,
                &genesis,
                1,
                1,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );

        let before = hook.preflight_state_root(&finalized).unwrap().unwrap();
        hook.candidates
            .lock()
            .unwrap()
            .get_mut(&finalized.hash())
            .unwrap()
            .state
            .timestamp += 1;
        // This test-only direct mutation changes the state after sealing. The
        // candidate boundary must reject it rather than persist the old root.
        assert!(hook.preflight_state_root(&finalized).is_err());
        assert!(hook.commit(&finalized).is_err());
        assert!(hook.shared_state().is_state_corrupted());
        assert_ne!(before, [0u8; 32]);
    }

    #[test]
    fn direct_commit_rejects_changed_candidate_root_without_mutating_canonical() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let finalized = finalized(
            &mut hook,
            block(
                context,
                &genesis,
                1,
                1,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );
        let (before_height, before_root) = {
            let canonical = shared.app.read().unwrap();
            (
                canonical.committed_height(),
                canonical.compute_full_state_root(),
            )
        };

        hook.candidates
            .lock()
            .unwrap()
            .get_mut(&finalized.hash())
            .unwrap()
            .state
            .timestamp += 1;

        assert!(hook.commit(&finalized).is_err());
        assert!(shared.is_state_corrupted());
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), before_height);
        assert_eq!(canonical.compute_full_state_root(), before_root);
        assert!(canonical.account("alice").is_none());
    }

    #[test]
    fn direct_commit_rejects_candidate_derived_index_corruption() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let finalized = finalized(
            &mut hook,
            block(
                context,
                &genesis,
                1,
                1,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );

        hook.candidates
            .lock()
            .unwrap()
            .get_mut(&finalized.hash())
            .unwrap()
            .state
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .order_index
            .insert(
                "corrupt-direct-commit".into(),
                (crate::app::orderbook::Side::Bid, 1),
            );

        assert!(hook.commit(&finalized).is_err());
        assert!(shared.is_state_corrupted());
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), 0);
        assert!(canonical.account("alice").is_none());
    }

    #[test]
    fn preflight_rejects_mismatched_candidate_artifact() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared);
        let context = context();
        let genesis = Block::genesis(context);
        let finalized = finalized(
            &mut hook,
            block(
                context,
                &genesis,
                1,
                1,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );

        let candidate = hook.candidates.get_mut().expect("candidate lock");
        std::sync::Arc::make_mut(
            candidate
                .get_mut(&finalized.hash())
                .expect("matching candidate")
                .state
                .last_execution_artifacts
                .as_mut()
                .expect("candidate artifact"),
        )
        .height += 1;

        assert!(hook.preflight_commitment(&finalized).is_err());
        assert_eq!(
            hook.shared_state().app.read().unwrap().committed_height(),
            0
        );
    }

    #[test]
    fn corrupted_canonical_indexes_reject_execute_without_candidate_or_publish() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        shared
            .app
            .write()
            .unwrap()
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .order_index
            .insert("corrupt".to_string(), (Side::Bid, 1));

        let proposed = block(context, &genesis, 1, 1, Vec::new());
        assert_eq!(
            hook.execute(&proposed),
            CanonicalAppHook::invalid_consensus_state_hash()
        );
        assert_eq!(hook.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn corrupted_canonical_primary_state_rejects_execute_without_candidate_or_publish() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        shared
            .app
            .write()
            .unwrap()
            .mark_prices
            .insert("BTC-USDT".to_string(), 0);

        let proposed = block(context, &genesis, 1, 1, Vec::new());
        assert_eq!(
            hook.execute(&proposed),
            CanonicalAppHook::invalid_consensus_state_hash()
        );
        assert_eq!(hook.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn corrupted_canonical_indexes_fail_private_replay_without_publish() {
        let context = context();
        let genesis = Block::genesis(context);
        let source_shared = SharedState::new(AppState::new());
        let mut source = CanonicalAppHook::new(source_shared);
        let finalized = finalized(&mut source, block(context, &genesis, 1, 1, Vec::new()));

        let shared = SharedState::new(AppState::new());
        shared
            .app
            .write()
            .unwrap()
            .orderbooks
            .get_mut("BTC-USDT")
            .unwrap()
            .order_index
            .insert("corrupt".to_string(), (Side::Bid, 1));
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();

        assert!(hook.preflight_commitment(&finalized).is_err());
        assert!(hook.preflight_state_root(&finalized).is_err());
        assert!(hook.commit(&finalized).is_err());
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn preflight_private_replay_does_not_create_candidate_or_publish() {
        let shared = SharedState::new(AppState::new());
        let mut source = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let finalized = finalized(
            &mut source,
            block(
                context,
                &genesis,
                1,
                1,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );
        // Simulate recovery of a finalized block without retaining its
        // speculative candidate.
        source.candidates.get_mut().unwrap().clear();
        let recovered = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let commitment = recovered
            .preflight_commitment(&finalized)
            .expect("private preflight")
            .expect("canonical app must produce a commitment");

        assert_eq!(commitment.receipts.len(), 1);
        assert_eq!(recovered.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn leader_draft_private_preflight_uses_executed_roots_without_publishing() {
        let shared = SharedState::new(AppState::new());
        let hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let draft = block(context, &genesis, 1, 1, Vec::new());
        let mut events = shared.subscribe();

        hook.preflight_block_with_speculative_branch(context, &draft, &genesis, &[])
            .expect("leader draft should validate against its privately executed roots");

        assert_eq!(hook.candidate_count(), 0);
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn chained_candidate_does_not_repropose_parent_transactions() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let first = Transaction::Deposit {
            trader: "alice".to_string(),
            amount: 10,
        };
        let first_block = finalized(
            &mut hook,
            block(context, &genesis, 1, 1, payload(first.clone())),
        );

        let child_payload = hook.prepare_payload(&first_block);
        let child_txs: Vec<ConsensusTransaction> = bincode::deserialize(&child_payload).unwrap();
        assert!(child_txs.is_empty());

        let second = Transaction::Deposit {
            trader: "bob".to_string(),
            amount: 20,
        };
        shared
            .app
            .write()
            .unwrap()
            .submit_tx(second.clone())
            .unwrap();
        let child_payload = hook.prepare_payload(&first_block);
        let child_txs: Vec<ConsensusTransaction> = bincode::deserialize(&child_payload).unwrap();
        assert_eq!(child_txs.len(), 1);
        assert!(
            matches!(child_txs[0].action(), Transaction::Deposit { trader, .. } if trader == "bob")
        );
    }

    #[test]
    fn signed_child_candidate_derives_from_clean_parent_tree() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared);
        let context = context();
        let genesis = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let trader = format!("{:?}", signer.address());

        let first = finalized(
            &mut hook,
            block(
                context,
                &genesis,
                1,
                1,
                signed_payload(
                    context.genesis_hash,
                    &signer,
                    0,
                    100,
                    Transaction::Deposit {
                        trader: trader.clone(),
                        amount: 10,
                    },
                ),
            ),
        );
        let second = finalized(
            &mut hook,
            block(
                context,
                &first,
                2,
                2,
                signed_payload(
                    context.genesis_hash,
                    &signer,
                    1,
                    100,
                    Transaction::Deposit { trader, amount: 20 },
                ),
            ),
        );

        let candidates = hook.candidates.lock().unwrap();
        let parent = candidates.get(&first.hash()).unwrap();
        let child = candidates.get(&second.hash()).unwrap();
        assert_eq!(
            child.state.full_state_dirty(),
            crate::app::state::full_state_hash::COMPONENT_DIRTY_NONE
        );
        assert_ne!(
            child.full_state_tree.components[0],
            parent.full_state_tree.components[0]
        );
        assert_ne!(
            child.full_state_tree.components[1],
            parent.full_state_tree.components[1]
        );
        assert_ne!(
            child.full_state_tree.components[5],
            parent.full_state_tree.components[5]
        );
        for index in [2, 3, 4, 6, 7, 8] {
            assert_eq!(
                child.full_state_tree.components[index], parent.full_state_tree.components[index],
                "unrelated component {index} changed"
            );
        }
    }

    #[test]
    fn restored_speculative_candidate_extends_after_restart() {
        let shared = SharedState::new(AppState::new());
        let mut before_restart = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);

        let committed = finalized(
            &mut before_restart,
            block(context, &genesis, 1, 1, Vec::new()),
        );
        before_restart.commit(&committed).unwrap();

        let speculative = finalized(
            &mut before_restart,
            block(
                context,
                &committed,
                2,
                2,
                payload(Transaction::Deposit {
                    trader: "alice".to_string(),
                    amount: 10,
                }),
            ),
        );

        // A fresh hook represents the process after restart.  Only the
        // committed state was replayed; restore the persisted high-QC branch
        // into its candidate map.
        let mut after_restart = CanonicalAppHook::new(shared.clone());
        // Production restart seeds this trusted value by replaying the
        // finalized chain through `commit` before speculative recovery.
        after_restart.committed_hash = Some(committed.hash());
        after_restart
            .restore_speculative_chain(context, &committed, &[speculative.clone()])
            .unwrap();
        assert_eq!(after_restart.candidate_count(), 1);
        assert_eq!(shared.app.read().unwrap().committed_height(), 1);

        let child = block(context, &speculative, 3, 3, Vec::new());
        let expected_child_hash = before_restart.execute(&child);
        let restored_child_hash = after_restart.execute(&child);
        assert_eq!(restored_child_hash, expected_child_hash);
        assert_eq!(shared.app.read().unwrap().committed_height(), 1);
    }

    #[test]
    fn speculative_replay_rejects_untrusted_same_height_anchor_without_mutation() {
        let shared = SharedState::new(AppState::new());
        let mut source = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut source, block(context, &genesis, 1, 1, Vec::new()));
        source.commit(&committed).unwrap();

        let mut restarted = CanonicalAppHook::new(shared.clone());
        let before_height = shared.app.read().unwrap().committed_height();
        let before_root = shared.app.read().unwrap().compute_full_state_root();
        let before_candidates = restarted.candidate_count();
        let before_committed_hash = restarted.committed_hash;

        let error = restarted
            .restore_speculative_chain(context, &committed, &[])
            .expect_err("nonzero recovery requires a trusted committed hash");
        assert!(error.contains("trusted committed head hash"));
        assert_eq!(restarted.candidate_count(), before_candidates);
        assert_eq!(restarted.committed_hash, before_committed_hash);

        // Even with a trusted hash seeded by committed replay, a same-height
        // anchor with a different exact block hash must not mutate recovery
        // state or publish candidates.
        restarted.committed_hash = Some(committed.hash());
        let mut wrong_anchor = committed.clone();
        wrong_anchor.timestamp = wrong_anchor.timestamp.saturating_add(1);
        let error = restarted
            .restore_speculative_chain(context, &wrong_anchor, &[])
            .expect_err("same-height anchor must match the trusted block hash");
        assert!(error.contains("exact canonical head"));
        assert_eq!(restarted.candidate_count(), before_candidates);
        assert_eq!(restarted.committed_hash, Some(committed.hash()));
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), before_height);
        assert_eq!(canonical.compute_full_state_root(), before_root);
    }

    #[test]
    fn speculative_replay_publishes_no_partial_candidates_after_late_failure() {
        let shared = SharedState::new(AppState::new());
        let mut source = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut source, block(context, &genesis, 1, 1, Vec::new()));
        source.commit(&committed).unwrap();
        let speculative = finalized(
            &mut source,
            block(
                context,
                &committed,
                2,
                2,
                payload(Transaction::Deposit {
                    trader: "staged-only".to_string(),
                    amount: 10,
                }),
            ),
        );
        let mut invalid_grandchild = block(context, &speculative, 3, 3, Vec::new());
        invalid_grandchild.commitment_root = CommitmentV2::default().root().unwrap();
        invalid_grandchild.app_hash = [9u8; 32];

        let mut restarted = CanonicalAppHook::new(shared.clone());
        // This mirrors the trusted hash published by committed replay.
        restarted.committed_hash = Some(committed.hash());
        let before_root = shared.app.read().unwrap().compute_full_state_root();

        let error = restarted
            .restore_speculative_chain(context, &committed, &[speculative, invalid_grandchild])
            .expect_err("invalid grandchild must discard the staged branch");
        assert!(error.contains("app hash mismatch"));
        assert_eq!(restarted.candidate_count(), 0);
        assert_eq!(restarted.committed_hash, Some(committed.hash()));
        let canonical = shared.app.read().unwrap();
        assert_eq!(canonical.committed_height(), committed.height);
        assert_eq!(canonical.compute_full_state_root(), before_root);
        assert!(canonical.accounts.get("staged-only").is_none());
    }

    #[test]
    fn speculative_replay_rejects_context_parent_and_app_hash_mismatch() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        hook.commit(&committed).unwrap();
        let valid = finalized(&mut hook, block(context, &committed, 2, 2, Vec::new()));

        let mut restarted = CanonicalAppHook::new(shared.clone());
        let mut wrong_context = valid.clone();
        wrong_context.genesis_hash[0] ^= 1;
        assert!(restarted
            .restore_speculative_chain(context, &committed, &[wrong_context])
            .is_err());

        let mut wrong_parent = valid.clone();
        wrong_parent.parent = genesis.hash();
        assert!(restarted
            .restore_speculative_chain(context, &committed, &[wrong_parent])
            .is_err());

        let mut wrong_app_hash = valid;
        wrong_app_hash.app_hash[0] ^= 1;
        assert!(restarted
            .restore_speculative_chain(context, &committed, &[wrong_app_hash])
            .is_err());
        assert_eq!(shared.app.read().unwrap().committed_height(), 1);
    }

    #[test]
    fn durable_commit_callback_publishes_block_event_only_after_application_commit() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        let commitment = hook
            .preflight_commitment(&committed)
            .unwrap()
            .expect("commitment");

        hook.commit(&committed).unwrap();
        assert!(events.try_recv().is_err());
        hook.on_durable_commit(&committed, &commitment).unwrap();
        match events.try_recv().unwrap() {
            Event::BlockCommitted {
                height,
                hash,
                tx_count,
            } => {
                assert_eq!(height, 1);
                assert_eq!(hash, hex::encode(committed.hash()));
                assert_eq!(tx_count, 0);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn durable_commit_callback_targets_signed_transaction_receipt_to_signer() {
        let context = context();
        let state = AppState::new_with_chain_domain_and_dev(context.genesis_hash, false);
        let shared = SharedState::new(state);
        let mut user_events = shared.subscribe_committed_user_events();
        let mut hook = CanonicalAppHook::new(shared.clone());
        let genesis = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let action = Transaction::Deposit {
            trader: format!("{:?}", signer.address()),
            amount: 10,
        };
        let envelope =
            SignedEnvelope::sign(context.genesis_hash, &signer, 0, 0, 100, action).unwrap();
        let tx_hash = envelope.hash().unwrap();
        let payload = bincode::serialize(&vec![ConsensusTransaction::Signed(envelope)]).unwrap();
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, payload));
        let commitment = hook
            .preflight_commitment(&committed)
            .unwrap()
            .expect("commitment");

        hook.commit(&committed).unwrap();
        assert!(user_events.try_recv().is_err());
        hook.on_durable_commit(&committed, &commitment).unwrap();

        let (address, event) = user_events.try_recv().expect("finalized user event");
        assert_eq!(address, format!("{:?}", signer.address()).to_lowercase());
        match event {
            UserEvent::TransactionFinalized {
                tx_hash: actual,
                block_height,
                status,
                events,
                ..
            } => {
                assert_eq!(actual, hex::encode(tx_hash));
                assert_eq!(block_height, 1);
                assert_eq!(status, crate::types::ReceiptStatus::SUCCESS.0);
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].event_type, crate::types::EventType::DEPOSIT.0);
            }
            other => panic!("unexpected user event: {other:?}"),
        }
    }

    #[test]
    fn recovery_head_validation_binds_height_hash_and_fresh_state_root() {
        let context = context();
        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        state.set_consensus_context(context);
        state.staking_mut().epoch_snapshot = Some(crate::app::staking::EpochSnapshot::new(0, 0, 0));
        let shared = SharedState::new(state);
        let mut hook = CanonicalAppHook::new(shared);
        let genesis = Block::genesis(context);
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        hook.commit(&committed).expect("commit recovered head");

        hook.validate_recovery_head(&committed)
            .expect("exact recovered head should validate");

        hook.canonical_write().unwrap().staking_mut().current_epoch = 1;
        assert!(hook
            .validate_recovery_head(&committed)
            .unwrap_err()
            .contains("application epoch"));
        hook.canonical_write().unwrap().staking_mut().current_epoch = 0;

        let pending_update = hook
            .canonical_read()
            .staking()
            .active_validator_set_for_consensus();
        hook.canonical_write().unwrap().pending_validator_update = Some(pending_update);
        assert!(hook
            .validate_recovery_head(&committed)
            .unwrap_err()
            .contains("pending validator update"));
        hook.canonical_write().unwrap().pending_validator_update = None;

        let mut wrong_hash = committed.clone();
        wrong_hash.timestamp += 1;
        assert!(hook
            .validate_recovery_head(&wrong_hash)
            .unwrap_err()
            .contains("block hash"));

        let mut wrong_root = committed;
        wrong_root.app_hash[0] ^= 1;
        hook.committed_hash = Some(wrong_root.hash());
        assert!(hook
            .validate_recovery_head(&wrong_root)
            .unwrap_err()
            .contains("state root mismatch"));
    }

    #[test]
    fn hash_mismatch_marks_state_corrupted_without_publishing() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let mut events = shared.subscribe();
        let context = context();
        let genesis = Block::genesis(context);
        let mut committed = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        committed.app_hash[0] ^= 1;

        assert!(hook.commit(&committed).is_err());
        assert!(shared.is_state_corrupted());
        assert_eq!(shared.app.read().unwrap().committed_height(), 0);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn commit_prunes_conflicting_descendants() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let branch_a = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        let branch_b = finalized(&mut hook, block(context, &genesis, 1, 2, Vec::new()));
        let _child_a = finalized(&mut hook, block(context, &branch_a, 2, 3, Vec::new()));
        let _child_b = finalized(&mut hook, block(context, &branch_b, 2, 4, Vec::new()));
        assert_eq!(hook.candidate_count(), 4);

        hook.commit(&branch_a).unwrap();
        assert_eq!(hook.candidate_count(), 1);
    }

    #[test]
    fn speculative_candidates_are_bounded_without_eviction_of_protected_ancestors() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared);
        let context = context();
        let genesis = Block::genesis(context);

        let too_deep = block(context, &genesis, MAX_SPECULATIVE_DEPTH + 1, 1, Vec::new());
        assert!(hook.validate_block(&too_deep).is_err());
        assert_eq!(hook.candidate_count(), 0);

        let root = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        let child = finalized(&mut hook, block(context, &root, 2, 2, Vec::new()));
        let delayed_qc_candidate = finalized(&mut hook, block(context, &genesis, 1, 3, Vec::new()));

        // Below the resource limit, an apparently unprotected proposal must
        // remain available because its vote can still form a delayed QC.
        hook.prune_speculative_branches(&[child.hash()]);
        assert!(hook
            .candidates
            .lock()
            .unwrap()
            .contains_key(&delayed_qc_candidate.hash()));

        // Fill the remaining slots with same-height forks. None of these may
        // evict the root/child ancestor closure needed to extend `child`.
        for view in 4..=(MAX_SPECULATIVE_CANDIDATES as u64) + 1 {
            if hook.candidate_count() == MAX_SPECULATIVE_CANDIDATES {
                break;
            }
            let fork = block(context, &genesis, 1, view, Vec::new());
            let _ = finalized(&mut hook, fork);
        }
        assert_eq!(hook.candidate_count(), MAX_SPECULATIVE_CANDIDATES);
        {
            let candidates = hook.candidates.lock().unwrap();
            assert!(candidates.contains_key(&root.hash()));
            assert!(candidates.contains_key(&child.hash()));
        }

        // A consensus root can safely release the unrelated fork branches and
        // continue extending the protected child even while no commit has
        // arrived yet.
        hook.prune_speculative_branches(&[child.hash()]);
        assert_eq!(hook.candidate_count(), 2);
        let extension = block(context, &child, 3, 100, Vec::new());
        assert!(hook.validate_block(&extension).is_ok());
        let _extension = finalized(&mut hook, extension);
        for view in 101..=200 {
            if hook.candidate_count() == MAX_SPECULATIVE_CANDIDATES {
                break;
            }
            let fork = block(context, &genesis, 1, view, Vec::new());
            let _ = finalized(&mut hook, fork);
        }
        assert_eq!(hook.candidate_count(), MAX_SPECULATIVE_CANDIDATES);

        let overflow = block(
            context,
            &genesis,
            1,
            MAX_SPECULATIVE_CANDIDATES as u64 + 10,
            Vec::new(),
        );
        assert!(hook.validate_block(&overflow).is_err());
        let _invalid = hook.execute(&overflow);
        assert_eq!(hook.candidate_count(), MAX_SPECULATIVE_CANDIDATES);
    }

    #[test]
    fn commit_rejects_same_height_fork_without_mutating_canonical_state() {
        let shared = SharedState::new(AppState::new());
        let mut hook = CanonicalAppHook::new(shared.clone());
        let context = context();
        let genesis = Block::genesis(context);
        let committed = finalized(&mut hook, block(context, &genesis, 1, 1, Vec::new()));
        hook.commit(&committed).unwrap();

        // This block reuses the next height but points at genesis instead of
        // the exact committed head. Its payload/app hash is irrelevant: the
        // parent anchor must fail before application execution.
        let fork = block(context, &genesis, 2, 2, Vec::new());
        let candidate_count = hook.candidate_count();
        assert!(hook.commit(&fork).is_err());
        assert_eq!(shared.app.read().unwrap().committed_height(), 1);
        assert_eq!(hook.candidate_count(), candidate_count);
    }
}
