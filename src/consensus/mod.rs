//! HotStuff-2 Consensus Implementation
//!
//! This module implements the HotStuff-2 BFT consensus protocol.
//!
//! ## Components
//!
//! - `engine`: Legacy in-memory consensus loop (opt-in `legacy-engine` feature)
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
pub mod committee;
#[cfg(feature = "legacy-engine")]
mod engine;
pub mod equivocation;
mod message_handler;
pub mod metrics;
mod pacemaker;
pub mod runner;
mod safety;
pub mod timeout;
pub mod transition;
pub mod view_change;

pub use aggregator::{RateLimitError, VoteAggregator, VoteRateLimiter};
pub use committee::{form_certificate, verify_certificate, verify_equivocation_proof, verify_vote};
#[cfg(feature = "legacy-engine")]
pub use engine::Engine;
pub use equivocation::{
    EquivocationDetector, EquivocationProof, EquivocationStats, VoteCheckResult,
};
pub use metrics::{ConsensusMetrics, MetricsSummary};
pub use pacemaker::Pacemaker;
pub use runner::ConsensusRunner;
pub use safety::Safety;
pub use timeout::{
    create_signed_timeout, verify_timeout_certificate, TimeoutCollector, TimeoutError,
};
pub use transition::{
    committee_members_from_update, EpochTransitionActivation, EpochTransitionProof,
    StateRootReference, EPOCH_TRANSITION_PROOF_SCHEMA_VERSION, MAX_EPOCH_TRANSITION_PROOF_BYTES,
};
pub use view_change::{
    create_signed_view_change, validate_view_change, validate_view_change_with_sig,
    ViewChangeCollector, ViewChangeError,
};

use crate::types::{Block, CommitmentV2, Hash};

use std::collections::{HashMap, HashSet};

// === Rate Limiting Constants (CRITICAL-7) ===

/// Maximum votes per validator per second (prevents vote spam DoS)
pub const MAX_VOTES_PER_VALIDATOR_PER_SECOND: usize = 10;

/// How many views of votes to retain (older votes are pruned after commit)
pub const VOTE_RETENTION_VIEWS: u64 = 10;

/// Shared fixed speculative storage budget used by both the in-memory cache
/// and persistent hash journal. Keeping these protocol/runtime constants in
/// one module prevents local environment drift between validators.
pub(crate) const MAX_SPECULATIVE_STORE_BLOCKS: usize = 64;
/// Leave one journal entry available for a verified delayed-QC continuation.
pub(crate) const MAX_SPECULATIVE_STORE_SOFT_BLOCKS: usize = MAX_SPECULATIVE_STORE_BLOCKS - 1;
/// A maximum protocol payload serializes as a JSON byte array, so the durable
/// journal needs more than the old 64 MiB ceiling to admit one such block.
pub(crate) const MAX_SPECULATIVE_STORE_BYTES: usize = 96 * 1024 * 1024;
/// Ordinary proposals use this lower watermark; a verified QC may consume the
/// reserved final entry/byte headroom.  A single row is bounded separately.
pub(crate) const MAX_SPECULATIVE_STORE_SOFT_BYTES: usize = 48 * 1024 * 1024;
pub(crate) const MAX_SPECULATIVE_BLOCK_BYTES: usize = 48 * 1024 * 1024;

// =============================================================================
// Traits (Module Boundaries)
// =============================================================================

/// Application hook - consensus calls this to execute blocks
pub trait AppHook: Send + Sync {
    /// Prepare payload for next block (called by leader)
    fn prepare_payload(&self, parent: &Block) -> Vec<u8>;

    /// Validate the complete application payload before a node votes.  The
    /// default keeps lightweight/no-op applications compatible; canonical
    /// runtimes must reject malformed payloads, invalid envelopes, and
    /// non-sequential signer nonces here.
    fn validate_block(&self, _block: &Block) -> Result<(), String> {
        Ok(())
    }

    /// Admit one canonical signed user transaction into the application's
    /// mempool.  Consensus runners call this after transport admission; the
    /// application remains the authority for nonce and mempool policy.
    fn submit_user_transaction(
        &mut self,
        _envelope: crate::app::SignedEnvelope,
        _timestamp: u64,
    ) -> Result<Hash, String> {
        Err("application does not implement signed user transaction admission".to_string())
    }

    /// Execute block and return state hash (called after commit)
    fn execute(&mut self, block: &Block) -> Hash;

    /// Build the deterministic execution commitment for a block that is
    /// about to cross the finalized storage boundary.
    ///
    /// This callback is deliberately read-only.  Implementations that do not
    /// expose execution artifacts may return `Ok(None)`; canonical runtimes
    /// should return the exact `CommitmentV2` produced by the matching
    /// speculative execution (or by a private deterministic pre-execution
    /// when no candidate is available).  The runner persists these same bytes
    /// atomically with the finalized block before calling [`Self::commit`].
    fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
        Ok(None)
    }

    /// Derive an execution commitment before the proposer seals its root into
    /// the block header. This is a proposal-only phase; followers and commit
    /// boundaries must use [`Self::preflight_commitment`] instead.
    fn derive_execution_commitment(&self, block: &Block) -> Result<Option<CommitmentV2>, String> {
        self.preflight_commitment(block)
    }

    /// Re-key a speculative execution candidate after the proposer has filled
    /// the execution commitment into the block header.  The commitment is
    /// deliberately derived after `execute`, so implementations that retain
    /// candidates must update their hash index before the block is signed or
    /// voted on.  Stateless hooks can keep the default no-op implementation.
    fn seal_execution_commitment(&mut self, _block: &Block) -> Result<(), String> {
        Ok(())
    }

    /// Build the deterministic schema-v3 full-state root for a finalized
    /// block.  The returned root is authenticated as `Block::app_hash`, and
    /// therefore is also bound by the block hash, votes, and certificates.
    ///
    /// Every active application hook must implement this check explicitly.
    /// Returning `None` is a fail-closed signal to consensus and is useful for
    /// legacy/test hooks that have not opted into the authenticated root.
    fn preflight_state_root(&self, _block: &Block) -> Result<Option<Hash>, String> {
        Ok(None)
    }

    /// Apply a block to the canonical application state.
    ///
    /// Historically `execute` was also used as the commit operation.  Keep a
    /// no-op default here so existing application hooks remain compatible;
    /// canonical runtimes can override this callback to separate speculative
    /// execution from committed state mutation.
    fn commit(&mut self, block: &Block) -> Result<Hash, String> {
        Ok(block.app_hash)
    }

    /// Publish best-effort application notifications for a block only after
    /// consensus has durably persisted the finalized block and the canonical
    /// application commit has succeeded.
    ///
    /// Recovery and verified-import paths intentionally do not call this
    /// hook, so restarting a node cannot replay historical WebSocket events.
    fn on_durable_commit(
        &mut self,
        _block: &Block,
        _commitment: &CommitmentV2,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Prove that the attached application has recovered the exact durable
    /// consensus head before a recovered runner starts participating.
    ///
    /// Stateful applications must override this and bind their canonical
    /// height, block hash, and freshly computed state root to `block`.  The
    /// default is deliberately fail-closed for non-genesis recovery so a new
    /// binary cannot attach a fresh application state with `with_app()` and
    /// accidentally resume an older consensus database.
    fn validate_recovery_head(&self, block: &Block) -> Result<(), String> {
        if block.height == 0 {
            Ok(())
        } else {
            Err("application does not implement non-genesis recovery-head validation".to_string())
        }
    }

    /// Drop only speculative branches that are not in the supplied protected
    /// root/ancestor set. Stateful hooks may use this before executing a new
    /// proposal when their snapshot budget is full; the default is a no-op
    /// for stateless and legacy hooks.
    fn prune_speculative_branches(&mut self, _protected_roots: &[Hash]) {}

    /// Validate a proposal against a bounded, read-only replay of its
    /// persisted parent branch.  Stateful hooks use this to authenticate a
    /// delayed QC branch before evicting any live candidate; lightweight hooks
    /// can use their ordinary validation path.
    fn preflight_block_with_speculative_branch(
        &self,
        _context: crate::types::ConsensusContext,
        block: &Block,
        _committed_head: &Block,
        _ancestors: &[Block],
    ) -> Result<(), String> {
        self.validate_block(block)
    }

    /// Rebuild a bounded speculative branch after the caller has completed
    /// read-only application and safety checks.  The default is a no-op for
    /// stateless hooks.
    fn restore_speculative_branch(
        &mut self,
        _context: crate::types::ConsensusContext,
        _committed_head: &Block,
        _ancestors: &[Block],
    ) -> Result<(), String> {
        Ok(())
    }

    /// Read-only admission check for a restored branch. The runner performs
    /// this before pruning pending/application state or touching the store.
    fn check_speculative_branch_admission(
        &self,
        _context: crate::types::ConsensusContext,
        _committed_head: &Block,
        _ancestors: &[Block],
        _protected_roots: &[Hash],
        _reserve_slots: usize,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Atomically make room for a validated speculative branch and restore its
    /// ancestor closure. Stateful hooks may stage candidate replacement so a
    /// capacity or replay failure leaves the live application unchanged.
    fn restore_speculative_branch_for_admission(
        &mut self,
        context: crate::types::ConsensusContext,
        committed_head: &Block,
        ancestors: &[Block],
        protected_roots: &[Hash],
        _reserve_slots: usize,
    ) -> Result<(), String> {
        self.prune_speculative_branches(protected_roots);
        self.restore_speculative_branch(context, committed_head, ancestors)
    }

    /// Take pending validator set update from epoch transition.
    ///
    /// Called after commit to check if the validator set should be updated.
    /// Returns None if no epoch transition occurred or if staking is disabled.
    fn take_validator_update(&mut self) -> Option<crate::app::staking::ValidatorSetUpdate> {
        None // Default implementation - no staking
    }

    /// Return the validator-set update authenticated by the canonical
    /// application state at `finalized_block`.
    ///
    /// This is deliberately read-only: staging a transition must not consume
    /// application state or accept committee material supplied only by the
    /// caller.  Implementations that cannot bind an update to the exact
    /// finalized head fail closed rather than returning an untrusted set.
    fn validator_set_update_for_transition(
        &self,
        _finalized_block: &Block,
    ) -> Result<Option<crate::app::staking::ValidatorSetUpdate>, String> {
        Err(
            "application does not expose a canonical validator-set update for transition staging"
                .to_string(),
        )
    }

    /// Submit equivocation evidence for slashing.
    ///
    /// Called by consensus when double-voting is detected. The evidence includes
    /// the two conflicting votes (same view, different block hashes) and their
    /// BLS signatures, proving the validator misbehaved.
    ///
    /// Returns true if the proof was verified and queued as a local proposal
    /// input, false if rejected (e.g., invalid signatures or validator not
    /// found). Slashing occurs only when the resulting system transaction is
    /// executed from a committed block.
    fn submit_equivocation_evidence(&mut self, _proof: EquivocationProof) -> bool {
        false // Default implementation - no slashing
    }
}

/// Block storage abstraction
pub trait BlockStore: Send + Sync {
    /// Save a block
    fn save(&self, block: &Block);

    /// Save an unfinalized block by hash without changing the canonical
    /// height index.  This is the storage boundary for speculative proposals.
    fn save_speculative(&self, block: &Block) -> anyhow::Result<()> {
        self.save(block);
        Ok(())
    }

    /// Check the fixed speculative journal budget before writing a new row.
    /// Callers must run branch pruning first; a protected full set fails
    /// closed rather than allowing a transient over-cap write.
    fn ensure_speculative_capacity(
        &self,
        _block: &Block,
        _max_blocks: usize,
        _max_bytes: usize,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Admit a speculative block, rolling one older unprotected sibling
    /// branch when the bounded journal is full.  Implementations that own a
    /// persistent journal should plan the victim and write the deletes plus
    /// the new row as one transition.  The default preserves the historical
    /// first-write/save semantics for lightweight test stores.
    fn admit_speculative_with_rolling_victim(
        &self,
        block: &Block,
        _protected_roots: &[Hash],
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        self.ensure_speculative_capacity(block, max_blocks, max_bytes)?;
        self.save_speculative(block)
    }

    /// Prune only unfinalized speculative branches. Implementations must keep
    /// every canonical height-index entry and committed head intact. The
    /// fixed bound is deliberately passed by the protocol, not by an env var.
    fn prune_speculative(
        &self,
        _protected_roots: &[Hash],
        _max_blocks: usize,
        _max_bytes: usize,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    /// Drop obsolete rows from a short-lived in-memory production cache.
    /// Persistent stores keep their canonical history, so the default is a
    /// no-op and standalone stores retain their normal history semantics.
    fn prune_production_cache(&self, _protected_roots: &[Hash]) -> anyhow::Result<()> {
        Ok(())
    }

    /// Get block by hash
    fn get(&self, hash: &Hash) -> Option<Block>;

    /// Get block by height
    fn get_by_height(&self, height: u64) -> Option<Block>;

    /// Mark block as committed
    fn set_committed(&self, hash: &Hash);

    /// Get the highest committed block
    fn get_committed_head(&self) -> Option<Block>;
}

fn reaches_ancestor(blocks: &HashMap<Hash, Block>, descendant: Hash, ancestor: Hash) -> bool {
    let mut current = descendant;
    let mut visited = HashSet::new();
    loop {
        if current == ancestor {
            return true;
        }
        if !visited.insert(current) {
            return false;
        }
        let Some(block) = blocks.get(&current) else {
            return false;
        };
        current = block.parent;
    }
}

fn speculative_ancestor_closure(blocks: &HashMap<Hash, Block>, roots: &[Hash]) -> HashSet<Hash> {
    let mut protected = HashSet::new();
    for root in roots {
        let mut current = *root;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                break;
            }
            let Some(block) = blocks.get(&current) else {
                break;
            };
            protected.insert(current);
            current = block.parent;
        }
    }
    protected
}

/// Simple in-memory block store for testing
#[derive(Default)]
pub struct MemoryBlockStore {
    blocks: std::sync::RwLock<HashMap<Hash, Block>>,
    by_height: std::sync::RwLock<HashMap<u64, Hash>>,
    committed: std::sync::RwLock<Option<Hash>>,
    speculative: std::sync::RwLock<HashSet<Hash>>,
    /// Serialize speculative admission, pruning, and canonical promotion so
    /// the check/account/insert transition cannot be bypassed by concurrent
    /// writers.
    speculative_journal_lock: std::sync::Mutex<()>,
}

impl MemoryBlockStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BlockStore for MemoryBlockStore {
    fn save(&self, block: &Block) {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .expect("speculative journal lock must not be poisoned");
        let hash = block.hash();
        self.blocks.write().unwrap().insert(hash, block.clone());
        self.by_height.write().unwrap().insert(block.height, hash);
        self.speculative.write().unwrap().remove(&hash);
    }

    fn save_speculative(&self, block: &Block) -> anyhow::Result<()> {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("speculative journal lock is poisoned"))?;
        let hash = block.hash();
        {
            let blocks = self.blocks.read().unwrap();
            if blocks.contains_key(&hash) {
                let speculative = self.speculative.read().unwrap();
                if speculative.contains(&hash)
                    || self
                        .by_height
                        .read()
                        .unwrap()
                        .values()
                        .any(|canonical_hash| *canonical_hash == hash)
                {
                    // Block::hash intentionally excludes justify.  Keep the
                    // first body for a hash so a larger certificate cannot
                    // overwrite the bounded row or change its classification.
                    return Ok(());
                }
                anyhow::bail!(
                    "speculative block {} exists without a canonical index",
                    hex::encode(hash)
                );
            }
        }
        <Self as BlockStore>::ensure_speculative_capacity(
            self,
            block,
            MAX_SPECULATIVE_STORE_BLOCKS,
            MAX_SPECULATIVE_STORE_BYTES,
        )?;
        self.blocks.write().unwrap().insert(hash, block.clone());
        self.speculative.write().unwrap().insert(hash);
        Ok(())
    }

    fn ensure_speculative_capacity(
        &self,
        block: &Block,
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        let blocks = self.blocks.read().unwrap();
        let speculative = self.speculative.read().unwrap();
        let hash = block.hash();
        if speculative.contains(&hash) {
            return Ok(());
        }
        if blocks.contains_key(&hash) {
            if self
                .by_height
                .read()
                .unwrap()
                .values()
                .any(|canonical_hash| *canonical_hash == hash)
            {
                return Ok(());
            }
            anyhow::bail!(
                "speculative block {} exists without a canonical index",
                hex::encode(hash)
            );
        }
        let block_bytes = serde_json::to_vec(block)?;
        if block_bytes.len() > MAX_SPECULATIVE_BLOCK_BYTES {
            anyhow::bail!(
                "speculative block serialization {} bytes exceeds per-row limit {}",
                block_bytes.len(),
                MAX_SPECULATIVE_BLOCK_BYTES
            );
        }
        let bytes: usize = speculative
            .iter()
            .filter_map(|hash| blocks.get(hash))
            .filter_map(|block| serde_json::to_vec(block).ok())
            .map(|bytes| bytes.len())
            .sum();
        if speculative.len().saturating_add(1) > max_blocks
            || bytes.saturating_add(block_bytes.len()) > max_bytes
        {
            anyhow::bail!(
                "speculative store capacity {} blocks/{} bytes would be exceeded",
                max_blocks,
                max_bytes
            );
        }
        Ok(())
    }

    fn admit_speculative_with_rolling_victim(
        &self,
        block: &Block,
        protected_roots: &[Hash],
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("speculative journal lock is poisoned"))?;
        let target_hash = block.hash();
        let canonical: HashSet<Hash> = self
            .by_height
            .read()
            .unwrap()
            .values()
            .copied()
            .chain(self.committed.read().unwrap().iter().copied())
            .collect();
        {
            let blocks = self.blocks.read().unwrap();
            if blocks.contains_key(&target_hash) {
                if self.speculative.read().unwrap().contains(&target_hash)
                    || canonical.contains(&target_hash)
                {
                    // Block::hash intentionally excludes `justify`: retain
                    // the first body and its original classification.
                    return Ok(());
                }
                anyhow::bail!(
                    "speculative block {} exists without a canonical index",
                    hex::encode(target_hash)
                );
            }
        }

        let target_bytes = serde_json::to_vec(block)?;
        if target_bytes.len() > MAX_SPECULATIVE_BLOCK_BYTES {
            anyhow::bail!(
                "speculative block serialization {} bytes exceeds per-row limit {}",
                target_bytes.len(),
                MAX_SPECULATIVE_BLOCK_BYTES
            );
        }

        let blocks_snapshot = self.blocks.read().unwrap().clone();
        let speculative_snapshot = self.speculative.read().unwrap().clone();
        let block_bytes: HashMap<Hash, usize> = speculative_snapshot
            .iter()
            .filter_map(|hash| {
                blocks_snapshot
                    .get(hash)
                    .and_then(|candidate| serde_json::to_vec(candidate).ok())
                    .map(|bytes| (*hash, bytes.len()))
            })
            .collect();
        let protected = speculative_ancestor_closure(&blocks_snapshot, protected_roots);
        let total_bytes: usize = block_bytes.values().sum();
        let fits = |removed: &HashSet<Hash>| {
            speculative_snapshot
                .len()
                .saturating_sub(removed.len())
                .saturating_add(1)
                <= max_blocks
                && total_bytes
                    .saturating_sub(
                        removed
                            .iter()
                            .map(|hash| block_bytes.get(hash).copied().unwrap_or_default())
                            .sum(),
                    )
                    .saturating_add(target_bytes.len())
                    <= max_bytes
        };

        let mut victims = HashSet::new();
        if !fits(&victims) {
            // Prefer the newest lower-view sibling: it is the branch most
            // likely to be the delayed-QC body that just timed out.  A branch
            // is eligible only when its complete speculative descendant
            // closure is outside every protected root.
            let mut candidates: Vec<Hash> = speculative_snapshot
                .iter()
                .copied()
                .filter(|hash| {
                    !canonical.contains(hash)
                        && !protected.contains(hash)
                        && block.justify.is_some()
                        && blocks_snapshot.get(hash).is_some_and(|candidate| {
                            candidate.parent == block.parent && candidate.view < block.view
                        })
                })
                .collect();
            candidates.sort_by(|left, right| {
                let left_view = blocks_snapshot[left].view;
                let right_view = blocks_snapshot[right].view;
                right_view.cmp(&left_view).then_with(|| left.cmp(right))
            });
            for root in candidates {
                let branch: HashSet<Hash> = speculative_snapshot
                    .iter()
                    .copied()
                    .filter(|hash| {
                        !canonical.contains(hash) && reaches_ancestor(&blocks_snapshot, *hash, root)
                    })
                    .collect();
                if branch.is_empty() || branch.iter().any(|hash| protected.contains(hash)) {
                    continue;
                }
                victims = branch;
                if fits(&victims) {
                    break;
                }
                victims.clear();
            }
        }
        if !fits(&victims) {
            anyhow::bail!(
                "protected speculative branches exceed {} blocks/{} bytes",
                max_blocks,
                max_bytes
            );
        }

        let mut blocks = self.blocks.write().unwrap();
        let mut speculative = self.speculative.write().unwrap();
        for hash in victims {
            if canonical.contains(&hash) {
                continue;
            }
            blocks.remove(&hash);
            speculative.remove(&hash);
        }
        blocks.insert(target_hash, block.clone());
        speculative.insert(target_hash);
        Ok(())
    }

    fn prune_speculative(
        &self,
        protected_roots: &[Hash],
        max_blocks: usize,
        max_bytes: usize,
    ) -> anyhow::Result<()> {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("speculative journal lock is poisoned"))?;
        let blocks = self.blocks.read().unwrap().clone();
        let speculative = self.speculative.read().unwrap().clone();
        let block_bytes: HashMap<Hash, usize> = speculative
            .iter()
            .filter_map(|hash| {
                blocks
                    .get(hash)
                    .and_then(|block| serde_json::to_vec(block).ok())
                    .map(|bytes| (*hash, bytes.len()))
            })
            .collect();
        let total_bytes: usize = block_bytes.values().sum();
        let under_budget = speculative.len() <= max_blocks && total_bytes <= max_bytes;

        let canonical: HashSet<Hash> = self
            .by_height
            .read()
            .unwrap()
            .values()
            .copied()
            .chain(self.committed.read().unwrap().iter().copied())
            .collect();
        let committed_hash = *self.committed.read().unwrap();
        let protected = speculative_ancestor_closure(&blocks, protected_roots);
        let mut eligible: Vec<Hash> = speculative
            .iter()
            .copied()
            .filter(|hash| {
                !canonical.contains(hash)
                    && !protected.contains(hash)
                    && committed_hash
                        .map(|committed| !reaches_ancestor(&blocks, *hash, committed))
                        .unwrap_or(false)
            })
            .collect();
        eligible.sort_by_key(|hash| {
            (
                blocks
                    .get(hash)
                    .map(|block| block.height)
                    .unwrap_or(u64::MAX),
                *hash,
            )
        });

        let mut removed = HashSet::new();
        let mut remaining = speculative.len();
        let mut remaining_bytes = total_bytes;
        for root in eligible {
            if !under_budget && remaining <= max_blocks && remaining_bytes <= max_bytes {
                break;
            }
            let branch: HashSet<Hash> = speculative
                .iter()
                .copied()
                .filter(|hash| {
                    !canonical.contains(hash)
                        && !protected.contains(hash)
                        && committed_hash
                            .map(|committed| !reaches_ancestor(&blocks, *hash, committed))
                            .unwrap_or(false)
                        && reaches_ancestor(&blocks, *hash, root)
                })
                .collect();
            if branch.is_empty() {
                continue;
            }
            remaining = remaining.saturating_sub(branch.len());
            remaining_bytes = remaining_bytes.saturating_sub(
                branch
                    .iter()
                    .map(|hash| block_bytes.get(hash).copied().unwrap_or_default())
                    .sum(),
            );
            removed.extend(branch);
        }

        if remaining > max_blocks || remaining_bytes > max_bytes {
            anyhow::bail!(
                "protected speculative branches exceed {} blocks/{} bytes",
                max_blocks,
                max_bytes
            );
        }
        if removed.is_empty() {
            return Ok(());
        }
        let mut blocks = self.blocks.write().unwrap();
        let mut speculative = self.speculative.write().unwrap();
        for hash in removed {
            if canonical.contains(&hash) {
                continue;
            }
            blocks.remove(&hash);
            speculative.remove(&hash);
        }
        Ok(())
    }

    fn prune_production_cache(&self, protected_roots: &[Hash]) -> anyhow::Result<()> {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("speculative journal lock is poisoned"))?;
        let committed_hash = *self.committed.read().unwrap();
        let Some(committed_hash) = committed_hash else {
            return Ok(());
        };
        let blocks_snapshot = self.blocks.read().unwrap().clone();
        let committed_height = blocks_snapshot
            .get(&committed_hash)
            .map(|block| block.height);
        let protected = speculative_ancestor_closure(&blocks_snapshot, protected_roots);
        let keep: HashSet<Hash> = blocks_snapshot
            .keys()
            .copied()
            .filter(|hash| {
                *hash == committed_hash
                    || (protected.contains(hash)
                        && committed_height
                            .map(|height| blocks_snapshot[hash].height > height)
                            .unwrap_or(true))
            })
            .collect();

        let mut blocks = self.blocks.write().unwrap();
        let mut speculative = self.speculative.write().unwrap();
        blocks.retain(|hash, _| keep.contains(hash));
        speculative.retain(|hash| keep.contains(hash));
        let mut by_height = self.by_height.write().unwrap();
        by_height.retain(|height, hash| {
            *hash == committed_hash
                && committed_height
                    .map(|committed_height| *height == committed_height)
                    .unwrap_or(false)
        });
        Ok(())
    }

    fn get(&self, hash: &Hash) -> Option<Block> {
        self.blocks.read().unwrap().get(hash).cloned()
    }

    fn get_by_height(&self, height: u64) -> Option<Block> {
        let hash = self.by_height.read().unwrap().get(&height).copied()?;
        self.get(&hash)
    }

    fn set_committed(&self, hash: &Hash) {
        let _journal_guard = self
            .speculative_journal_lock
            .lock()
            .expect("speculative journal lock must not be poisoned");
        *self.committed.write().unwrap() = Some(*hash);
        self.speculative.write().unwrap().remove(hash);
        if let Some(block) = self.get(hash) {
            self.by_height.write().unwrap().insert(block.height, *hash);
        }
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

    fn preflight_commitment(&self, _block: &Block) -> Result<Option<CommitmentV2>, String> {
        Ok(Some(CommitmentV2::default()))
    }

    fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
        let root = [0u8; 32];
        if block.app_hash != root {
            return Err("no-op application state root must be zero".to_string());
        }
        Ok(Some(root))
    }

    fn validate_recovery_head(&self, block: &Block) -> Result<(), String> {
        if block.app_hash != [0u8; 32] {
            return Err("no-op recovery head must have a zero application root".to_string());
        }
        let commitment = CommitmentV2::default();
        let commitment_root = commitment
            .root()
            .map_err(|error| format!("no-op recovery commitment root failed: {error}"))?;
        if block.height > 0 && block.commitment_root != commitment_root {
            return Err("no-op recovery head has a mismatched commitment root".to_string());
        }
        Ok(())
    }
}
