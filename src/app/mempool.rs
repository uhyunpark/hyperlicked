//! 3-Bucket Mempool with Two-Phase Commit
//!
//! Orders transactions by priority:
//! 1. Non-order txs (deposits, withdrawals) - bucket 0
//! 2. Cancels - bucket 1
//! 3. Orders (GTC, IOC, ALO) - bucket 2
//!
//! Within each bucket, FIFO order is maintained.
//!
//! Two-phase commit protocol:
//! 1. `peek_block()` - marks txs as "in proposal", returns refs
//! 2. `commit_proposal()` - removes committed txs by hash
//! 3. `rollback_proposal()` - clears pending set on view change

use std::collections::{HashMap, HashSet, VecDeque};

use super::staking::Evidence;
use super::{ConsensusTransaction, EnvelopeError, SignedEnvelope, Transaction};
use crate::types::{Hash, View};

/// Fixed local reserve for verified equivocation proofs.  This matches the
/// bounded journal headroom so an evidence burst cannot be used to grow the
/// process without bound, while ordinary bucket pressure cannot suppress
/// safety-critical evidence.
pub const MAX_PENDING_EVIDENCE: usize = 256;

/// Transaction with metadata
#[derive(Debug, Clone)]
pub struct PendingTx {
    pub entry: ConsensusTransaction,
    pub hash: Hash,
    pub timestamp: u64,
}

/// 3-bucket mempool for transaction ordering
#[derive(Clone)]
pub struct Mempool {
    /// Verified protocol evidence.  This queue has an independent bound and
    /// is proposed before ordinary bucket-0 transactions.
    evidence: VecDeque<PendingTx>,
    /// Bucket 0: Non-order transactions (deposits, withdrawals)
    bucket0: VecDeque<PendingTx>,
    /// Bucket 1: Cancel orders
    bucket1: VecDeque<PendingTx>,
    /// Bucket 2: Place orders (GTC, IOC, ALO)
    bucket2: VecDeque<PendingTx>,
    /// Maximum transactions per bucket
    max_per_bucket: usize,
    /// Hashes of transactions currently in an uncommitted proposal
    pending_proposal: HashSet<Hash>,
    /// View number of the current proposal (for validation)
    proposal_view: View,
    /// Per-address pending transaction count (anti-spam)
    per_address_count: HashMap<String, usize>,
    /// Maximum pending transactions per address
    max_per_address: usize,
    /// Maximum transaction age in milliseconds before eviction
    max_age_ms: u64,
}

impl Mempool {
    pub fn new(max_per_bucket: usize) -> Self {
        Self::with_config(max_per_bucket, 100, 3_600_000)
    }

    /// Create mempool with full configuration
    pub fn with_config(max_per_bucket: usize, max_per_address: usize, max_age_ms: u64) -> Self {
        Self {
            evidence: VecDeque::with_capacity(MAX_PENDING_EVIDENCE),
            bucket0: VecDeque::new(),
            bucket1: VecDeque::new(),
            bucket2: VecDeque::new(),
            max_per_bucket,
            pending_proposal: HashSet::new(),
            proposal_view: 0,
            per_address_count: HashMap::new(),
            max_per_address,
            max_age_ms,
        }
    }

    /// Create mempool from global config
    pub fn from_config() -> Self {
        let config = crate::config::Config::global();
        Self::with_config(
            config.mempool_max_per_bucket,
            config.mempool_max_per_address,
            config.mempool_max_age_ms,
        )
    }

    /// Add a transaction to the appropriate bucket
    pub fn add(&mut self, tx: Transaction, timestamp: u64) -> Result<Hash, MempoolError> {
        if matches!(&tx, Transaction::SubmitEvidence { .. }) {
            return Err(MempoolError::EvidenceRequiresVerifiedAdmission);
        }
        self.add_entry(ConsensusTransaction::System(tx), timestamp)
    }

    /// Add an authenticated user envelope to the mempool.
    pub fn add_envelope(
        &mut self,
        envelope: SignedEnvelope,
        timestamp: u64,
    ) -> Result<Hash, MempoolError> {
        if matches!(&envelope.action, Transaction::SubmitEvidence { .. }) {
            return Err(MempoolError::EvidenceRequiresVerifiedAdmission);
        }
        envelope
            .validate_structure()
            .map_err(MempoolError::Envelope)?;
        self.add_entry(ConsensusTransaction::Signed(envelope), timestamp)
    }

    /// Add a proof that has already passed the application validator/context
    /// checks.  The dedicated queue intentionally bypasses ordinary bucket
    /// and per-address limits, but remains strictly count-bounded and never
    /// evicts an older proof when full.
    pub(crate) fn add_verified_evidence(
        &mut self,
        tx: Transaction,
        timestamp: u64,
    ) -> Result<Hash, MempoolError> {
        let Transaction::SubmitEvidence {
            submitter,
            evidence,
        } = tx
        else {
            return Err(MempoolError::EvidenceRequiresVerifiedAdmission);
        };
        validate_evidence_shape(&submitter, &evidence)?;

        if let Some(hash) = self.find_equivocation_evidence_hash(&evidence) {
            return Ok(hash);
        }
        if self.evidence.len() >= MAX_PENDING_EVIDENCE {
            return Err(MempoolError::EvidenceQueueFull);
        }

        let entry = ConsensusTransaction::System(Transaction::SubmitEvidence {
            submitter,
            evidence,
        });
        let hash = entry.hash().map_err(MempoolError::Envelope)?;
        if self.contains_hash(&hash) {
            return Err(MempoolError::Duplicate);
        }
        self.evidence.push_back(PendingTx {
            entry,
            hash,
            timestamp,
        });
        Ok(hash)
    }

    fn add_entry(
        &mut self,
        entry: ConsensusTransaction,
        timestamp: u64,
    ) -> Result<Hash, MempoolError> {
        let hash = entry.hash().map_err(MempoolError::Envelope)?;
        let bucket = entry.bucket();
        let trader = entry.trader_address();

        if self.contains_hash(&hash) {
            return Err(MempoolError::Duplicate);
        }
        if let ConsensusTransaction::Signed(envelope) = &entry {
            if self.contains_signer_nonce(&envelope.signer_address(), envelope.nonce) {
                return Err(MempoolError::DuplicateSignerNonce);
            }
        }

        // Check per-address limit
        let count = self.per_address_count.get(&trader).copied().unwrap_or(0);
        if count >= self.max_per_address {
            return Err(MempoolError::AddressLimitReached);
        }

        let queue = match bucket {
            0 => &mut self.bucket0,
            1 => &mut self.bucket1,
            _ => &mut self.bucket2,
        };

        if queue.len() >= self.max_per_bucket {
            return Err(MempoolError::BucketFull);
        }

        queue.push_back(PendingTx {
            entry,
            hash,
            timestamp,
        });

        // Increment per-address count
        *self.per_address_count.entry(trader).or_insert(0) += 1;

        Ok(hash)
    }

    /// Return all pending entries in deterministic bucket/FIFO order without
    /// removing them.  The returned hash is the consensus/mempool identity.
    pub fn peek_consensus_block(
        &mut self,
        max_txs: usize,
        view: View,
    ) -> Vec<(ConsensusTransaction, Hash)> {
        if self.proposal_view != view {
            self.pending_proposal.clear();
            self.proposal_view = view;
        }

        let mut result = Vec::with_capacity(max_txs);
        // Verified protocol evidence is always proposed before ordinary
        // bucket-0 transactions.  It does not participate in ordinary
        // bucket/address quotas, so a full user bucket cannot delay safety
        // processing.
        for pending in &self.evidence {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                result.push((pending.entry.clone(), pending.hash));
                self.pending_proposal.insert(pending.hash);
            }
        }
        for bucket in [&self.bucket0, &self.bucket1, &self.bucket2] {
            for pending in bucket {
                if result.len() >= max_txs {
                    break;
                }
                if !self.pending_proposal.contains(&pending.hash) {
                    result.push((pending.entry.clone(), pending.hash));
                    self.pending_proposal.insert(pending.hash);
                }
            }
        }
        result
    }

    /// Read-only consensus payload view used by proposal construction.  It
    /// intentionally does not mutate proposal bookkeeping because the
    /// `AppHook` proposal method is synchronous and receives `&self`.
    pub fn peek_consensus_block_txs(&self, max_txs: usize) -> Vec<ConsensusTransaction> {
        let mut result = Vec::with_capacity(max_txs.min(self.len()));
        for pending in &self.evidence {
            if result.len() >= max_txs {
                return result;
            }
            result.push(pending.entry.clone());
        }
        for bucket in [&self.bucket0, &self.bucket1, &self.bucket2] {
            for pending in bucket {
                if result.len() >= max_txs {
                    return result;
                }
                result.push(pending.entry.clone());
            }
        }
        result
    }

    /// Iterate pending consensus entries in the canonical evidence/bucket/FIFO
    /// order without cloning transaction bodies. Proposal schedulers can keep
    /// references while scanning the bounded queues and clone only entries
    /// that actually fit the final payload.
    pub(crate) fn consensus_block_entries(
        &self,
    ) -> impl Iterator<Item = (usize, &ConsensusTransaction)> {
        self.evidence
            .iter()
            .chain(self.bucket0.iter())
            .chain(self.bucket1.iter())
            .chain(self.bucket2.iter())
            .enumerate()
            .map(|(index, pending)| (index, &pending.entry))
    }

    fn contains_hash(&self, hash: &Hash) -> bool {
        [&self.evidence, &self.bucket0, &self.bucket1, &self.bucket2]
            .into_iter()
            .any(|bucket| bucket.iter().any(|pending| &pending.hash == hash))
    }

    /// Check pending authenticated envelopes for a signer/nonce pair.  This
    /// closes the admission race where the account nonce has not changed yet
    /// but the same nonce is already waiting in the mempool.
    pub fn contains_signer_nonce(&self, signer: &str, nonce: u64) -> bool {
        let signer = signer.to_ascii_lowercase();
        [&self.evidence, &self.bucket0, &self.bucket1, &self.bucket2]
            .into_iter()
            .flat_map(|bucket| bucket.iter())
            .any(|pending| match &pending.entry {
                ConsensusTransaction::Signed(envelope) => {
                    envelope.nonce == nonce
                        && envelope.signer_address().to_ascii_lowercase() == signer
                }
                ConsensusTransaction::System(_) => false,
            })
    }

    /// Return a bounded rotating batch of pending authenticated user
    /// envelopes in the same deterministic bucket/FIFO order used for block
    /// construction.
    ///
    /// The caller supplies the current wall-clock timestamp so envelopes that
    /// are not yet valid, already expired, or older than the mempool age limit
    /// are not selected for retransmission. Only the selected batch is cloned;
    /// the full mempool is never materialized as an intermediate vector.
    pub fn pending_user_envelopes_batch_at(
        &self,
        timestamp: u64,
        cursor: usize,
        max_count: usize,
        max_encoded_bytes: usize,
    ) -> (Vec<SignedEnvelope>, usize) {
        if max_count == 0 || max_encoded_bytes == 0 {
            return (Vec::new(), 0);
        }

        let eligible_count = self.pending_user_envelope_count_at(timestamp);
        if eligible_count == 0 {
            return (Vec::new(), 0);
        }

        let start = cursor % eligible_count;
        let requested = eligible_count.min(max_count);
        let mut selected = Vec::with_capacity(requested);
        let mut encoded_bytes = 0usize;
        let mut visited = 0usize;

        // Walk the eligible queues at most twice (tail, then wrapped prefix)
        // and clone only selected envelopes. Once the byte budget is reached,
        // leave that unselected envelope at the next cursor so it cannot be
        // starved by earlier small entries.
        let rotating = self
            .eligible_user_envelopes_at(timestamp)
            .skip(start)
            .chain(self.eligible_user_envelopes_at(timestamp).take(start));
        for envelope in rotating.take(requested) {
            let Ok(envelope_bytes) = envelope.encoded_bytes() else {
                visited += 1;
                continue;
            };
            let Some(next_bytes) = encoded_bytes.checked_add(envelope_bytes.len()) else {
                if selected.is_empty() {
                    visited += 1;
                    continue;
                }
                break;
            };
            if next_bytes > max_encoded_bytes {
                if selected.is_empty() {
                    visited += 1;
                    continue;
                }
                break;
            }
            selected.push(envelope.clone());
            encoded_bytes = next_bytes;
            visited += 1;
        }

        let next_cursor = (start + visited) % eligible_count;
        (selected, next_cursor)
    }

    fn pending_user_envelope_count_at(&self, timestamp: u64) -> usize {
        self.eligible_user_envelopes_at(timestamp).count()
    }

    fn eligible_user_envelopes_at(&self, timestamp: u64) -> impl Iterator<Item = &SignedEnvelope> {
        [&self.bucket0, &self.bucket1, &self.bucket2]
            .into_iter()
            .flat_map(|bucket| bucket.iter())
            .filter_map(move |pending| {
                if !self.is_eligible_user_envelope(pending, timestamp) {
                    return None;
                }
                match &pending.entry {
                    ConsensusTransaction::Signed(envelope) => Some(envelope),
                    ConsensusTransaction::System(_) => None,
                }
            })
    }

    fn is_eligible_user_envelope(&self, pending: &PendingTx, timestamp: u64) -> bool {
        let ConsensusTransaction::Signed(envelope) = &pending.entry else {
            return false;
        };
        let fresh_enough =
            self.max_age_ms == 0 || pending.timestamp >= timestamp.saturating_sub(self.max_age_ms);
        fresh_enough && envelope.valid_after <= timestamp && timestamp <= envelope.valid_until
    }

    /// Return the queued transaction hash for an equivocation proof, if one
    /// is already present.  The canonical pending key is `(context,
    /// offender)`, rather than local observation time or the particular
    /// conflicting vote pair a node happened to observe.  This prevents
    /// duplicate local proposals when different nodes observe valid proofs
    /// for the same offense.
    pub fn find_equivocation_evidence_hash(&self, evidence: &Evidence) -> Option<Hash> {
        [&self.evidence, &self.bucket0, &self.bucket1, &self.bucket2]
            .into_iter()
            .flat_map(|bucket| bucket.iter())
            .find_map(|pending| match &pending.entry {
                ConsensusTransaction::System(Transaction::SubmitEvidence {
                    evidence: queued,
                    ..
                }) if same_equivocation_key(queued, evidence) => Some(pending.hash),
                _ => None,
            })
    }

    /// Remove every locally pending proof for the same canonical
    /// `(context, offender)` key.  Nodes can observe different valid vote
    /// pairs for one offender/context; committing one pair must therefore
    /// clear all local alternatives, not only the exact transaction hash.
    pub(crate) fn remove_equivocation_evidence(&mut self, evidence: &Evidence) -> usize {
        let hashes: Vec<Hash> = [&self.evidence, &self.bucket0, &self.bucket1, &self.bucket2]
            .into_iter()
            .flat_map(|bucket| bucket.iter())
            .filter_map(|pending| match &pending.entry {
                ConsensusTransaction::System(Transaction::SubmitEvidence {
                    evidence: queued,
                    ..
                }) if same_equivocation_key(queued, evidence) => Some(pending.hash),
                _ => None,
            })
            .collect();
        let removed = hashes.len();
        if removed > 0 {
            self.remove_by_hashes(&hashes);
        }
        removed
    }

    /// Get transactions for a block (ordered by bucket)
    pub fn prepare_block(&mut self, max_txs: usize) -> Vec<Transaction> {
        let mut result = Vec::with_capacity(max_txs);
        let mut to_decrement: Vec<String> = Vec::new();

        // Drain verified evidence before ordinary bucket 0.  Evidence is not
        // part of the per-address accounting because it has a dedicated
        // bounded reserve.
        while result.len() < max_txs {
            if let Some(pending) = self.evidence.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                if let ConsensusTransaction::System(tx) = pending.entry {
                    result.push(tx);
                }
            } else {
                break;
            }
        }

        // Drain from bucket 0 first (highest priority)
        while result.len() < max_txs {
            if let Some(pending) = self.bucket0.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                if let ConsensusTransaction::System(tx) = pending.entry {
                    result.push(tx);
                }
            } else {
                break;
            }
        }

        // Then bucket 1
        while result.len() < max_txs {
            if let Some(pending) = self.bucket1.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                if let ConsensusTransaction::System(tx) = pending.entry {
                    result.push(tx);
                }
            } else {
                break;
            }
        }

        // Finally bucket 2
        while result.len() < max_txs {
            if let Some(pending) = self.bucket2.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                if let ConsensusTransaction::System(tx) = pending.entry {
                    result.push(tx);
                }
            } else {
                break;
            }
        }

        // Decrement per-address counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }

        result
    }

    /// Peek transactions for a block without removing them (two-phase: step 1)
    ///
    /// Marks transactions as "in proposal" for the given view.
    /// Call `commit_proposal()` after block commits, or `rollback_proposal()` on view change.
    pub fn peek_block(&mut self, max_txs: usize, view: View) -> Vec<(Transaction, Hash)> {
        // Clear any stale proposal from a previous view
        if self.proposal_view != view {
            self.pending_proposal.clear();
            self.proposal_view = view;
        }

        let mut result = Vec::with_capacity(max_txs);

        // Read verified evidence first.  This mirrors consensus proposal
        // ordering while retaining two-phase bookkeeping.
        for pending in &self.evidence {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                if let ConsensusTransaction::System(tx) = &pending.entry {
                    result.push((tx.clone(), pending.hash));
                }
                self.pending_proposal.insert(pending.hash);
            }
        }

        // Read from bucket 0 first (highest priority)
        for pending in &self.bucket0 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                if let ConsensusTransaction::System(tx) = &pending.entry {
                    result.push((tx.clone(), pending.hash));
                }
                self.pending_proposal.insert(pending.hash);
            }
        }

        // Then bucket 1
        for pending in &self.bucket1 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                if let ConsensusTransaction::System(tx) = &pending.entry {
                    result.push((tx.clone(), pending.hash));
                }
                self.pending_proposal.insert(pending.hash);
            }
        }

        // Finally bucket 2
        for pending in &self.bucket2 {
            if result.len() >= max_txs {
                break;
            }
            if !self.pending_proposal.contains(&pending.hash) {
                if let ConsensusTransaction::System(tx) = &pending.entry {
                    result.push((tx.clone(), pending.hash));
                }
                self.pending_proposal.insert(pending.hash);
            }
        }

        result
    }

    /// Legacy peek for backward compatibility (single-node mode)
    pub fn peek_block_txs(&self, max_txs: usize) -> Vec<Transaction> {
        let mut result = Vec::with_capacity(max_txs);

        for pending in &self.evidence {
            if result.len() >= max_txs {
                return result;
            }
            if let ConsensusTransaction::System(tx) = &pending.entry {
                result.push(tx.clone());
            }
        }

        for pending in &self.bucket0 {
            if result.len() >= max_txs {
                return result;
            }
            if let ConsensusTransaction::System(tx) = &pending.entry {
                result.push(tx.clone());
            }
        }

        for pending in &self.bucket1 {
            if result.len() >= max_txs {
                return result;
            }
            if let ConsensusTransaction::System(tx) = &pending.entry {
                result.push(tx.clone());
            }
        }

        for pending in &self.bucket2 {
            if result.len() >= max_txs {
                return result;
            }
            if let ConsensusTransaction::System(tx) = &pending.entry {
                result.push(tx.clone());
            }
        }

        result
    }

    /// Commit a proposal by removing transactions by hash (two-phase: step 2)
    ///
    /// Called after a block is committed to finalize removal.
    ///
    /// **View Safety**: The `view` parameter must match the view that was used in `peek_block`.
    /// If the view doesn't match (e.g., a view change occurred), the commit is rejected
    /// to prevent removing transactions that belong to a different proposal.
    ///
    /// Returns `true` if commit succeeded, `false` if view mismatch (stale commit).
    pub fn commit_proposal(&mut self, tx_hashes: &[Hash], view: View) -> bool {
        // View safety check: reject commits from stale views
        if view != self.proposal_view {
            tracing::warn!(
                expected_view = self.proposal_view,
                commit_view = view,
                "Rejecting stale commit_proposal (view mismatch)"
            );
            return false;
        }

        self.remove_by_hashes(tx_hashes);

        // Clear pending set
        self.pending_proposal.clear();
        true
    }

    /// Commit a proposal without view checking (legacy, single-node mode)
    ///
    /// Use `commit_proposal` with view parameter for multi-node safety.
    pub fn commit_proposal_unchecked(&mut self, tx_hashes: &[Hash]) {
        self.remove_by_hashes(tx_hashes);

        // Clear pending set
        self.pending_proposal.clear();
    }

    /// Helper to remove transactions by hash and update per-address counts
    fn remove_by_hashes(&mut self, tx_hashes: &[Hash]) {
        let hash_set: HashSet<_> = tx_hashes.iter().collect();
        self.pending_proposal
            .retain(|pending_hash| !hash_set.contains(pending_hash));

        // Collect addresses to decrement (to avoid borrow issues)
        let mut to_decrement: Vec<String> = Vec::new();

        // Evidence has no per-address reservation to release.
        self.evidence
            .retain(|pending| !hash_set.contains(&pending.hash));

        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            bucket.retain(|p| {
                if hash_set.contains(&p.hash) {
                    to_decrement.push(p.entry.trader_address());
                    false
                } else {
                    true
                }
            });
        }

        // Decrement counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }
    }

    /// Rollback a proposal on view change (two-phase: abort)
    ///
    /// Transactions stay in mempool for the next leader.
    /// The `view` parameter is used for logging/debugging.
    pub fn rollback_proposal(&mut self, view: View) {
        if !self.pending_proposal.is_empty() {
            tracing::debug!(
                view,
                proposal_view = self.proposal_view,
                pending_count = self.pending_proposal.len(),
                "Rolling back proposal (view change)"
            );
        }
        self.pending_proposal.clear();
    }

    /// Get the current proposal view
    pub fn proposal_view(&self) -> View {
        self.proposal_view
    }

    /// Drain transactions that were previously peeked (legacy, for single-node)
    pub fn drain_block(&mut self, count: usize) {
        let mut remaining = count;
        let mut to_decrement: Vec<String> = Vec::new();

        // Drain verified evidence before ordinary bucket 0.
        while remaining > 0 {
            if let Some(pending) = self.evidence.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                remaining -= 1;
            } else {
                break;
            }
        }

        // Drain from bucket 0 first
        while remaining > 0 {
            if let Some(pending) = self.bucket0.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Then bucket 1
        while remaining > 0 {
            if let Some(pending) = self.bucket1.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Finally bucket 2
        while remaining > 0 {
            if let Some(pending) = self.bucket2.pop_front() {
                self.pending_proposal.remove(&pending.hash);
                to_decrement.push(pending.entry.trader_address());
                remaining -= 1;
            } else {
                break;
            }
        }

        // Decrement per-address counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }
    }

    /// Check how many transactions are pending
    pub fn len(&self) -> usize {
        self.evidence.len() + self.bucket0.len() + self.bucket1.len() + self.bucket2.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get pending count per bucket
    pub fn bucket_counts(&self) -> (usize, usize, usize) {
        (
            self.evidence.len() + self.bucket0.len(),
            self.bucket1.len(),
            self.bucket2.len(),
        )
    }

    /// Remove a transaction by hash (for cancellation)
    pub fn remove(&mut self, hash: &Hash) -> bool {
        if let Some(pos) = self.evidence.iter().position(|p| &p.hash == hash) {
            if let Some(removed) = self.evidence.remove(pos) {
                self.pending_proposal.remove(&removed.hash);
            }
            return true;
        }

        // Check each bucket
        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            if let Some(pos) = bucket.iter().position(|p| &p.hash == hash) {
                if let Some(removed) = bucket.remove(pos) {
                    self.pending_proposal.remove(&removed.hash);
                    let addr = removed.entry.trader_address();
                    if let Some(count) = self.per_address_count.get_mut(&addr) {
                        *count = count.saturating_sub(1);
                        if *count == 0 {
                            self.per_address_count.remove(&addr);
                        }
                    }
                }
                return true;
            }
        }
        false
    }

    /// Prune transactions older than max_age_ms
    ///
    /// Should be called at the start of each block to prevent stale tx accumulation.
    /// Returns the number of transactions pruned.
    pub fn prune_stale(&mut self, current_time: u64) -> usize {
        if self.max_age_ms == 0 {
            return 0; // Age eviction disabled
        }

        let cutoff = current_time.saturating_sub(self.max_age_ms);
        let mut pruned = 0;
        let mut to_decrement: Vec<String> = Vec::new();
        let mut pruned_hashes: Vec<Hash> = Vec::new();

        for bucket in [&mut self.bucket0, &mut self.bucket1, &mut self.bucket2] {
            let before = bucket.len();
            bucket.retain(|p| {
                // A cryptographically verified equivocation proof is a
                // protocol input, not a user order.  It must survive local
                // wall-clock aging until a proposer includes it, while the
                // normal per-bucket/per-address caps still bound memory.
                let is_equivocation_evidence = matches!(
                    &p.entry,
                    ConsensusTransaction::System(Transaction::SubmitEvidence { .. })
                );
                if !is_equivocation_evidence && p.timestamp < cutoff {
                    pruned_hashes.push(p.hash);
                    to_decrement.push(p.entry.trader_address());
                    false
                } else {
                    true
                }
            });
            pruned += before - bucket.len();
        }

        for hash in pruned_hashes {
            self.pending_proposal.remove(&hash);
        }

        // Decrement counts
        for addr in to_decrement {
            if let Some(count) = self.per_address_count.get_mut(&addr) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_address_count.remove(&addr);
                }
            }
        }

        if pruned > 0 {
            tracing::debug!(pruned, "Pruned stale transactions from mempool");
        }

        pruned
    }

    /// Clear all pending transactions
    pub fn clear(&mut self) {
        self.evidence.clear();
        self.bucket0.clear();
        self.bucket1.clear();
        self.bucket2.clear();
        self.pending_proposal.clear();
        self.per_address_count.clear();
    }
}

fn same_equivocation_key(left: &Evidence, right: &Evidence) -> bool {
    left.offender == right.offender && left.context == right.context
}

fn validate_evidence_shape(submitter: &str, evidence: &Evidence) -> Result<(), MempoolError> {
    let expected_submitter = format!("system:equivocation:{}", hex::encode(evidence.offender));
    if submitter != expected_submitter {
        return Err(MempoolError::InvalidEvidenceShape(
            "invalid system submitter",
        ));
    }
    if evidence.timestamp != 0 {
        return Err(MempoolError::InvalidEvidenceShape("timestamp must be zero"));
    }

    let first = (evidence.hash_a, evidence.app_hash_a, &evidence.signature_a);
    let second = (evidence.hash_b, evidence.app_hash_b, &evidence.signature_b);
    if second < first {
        return Err(MempoolError::InvalidEvidenceShape(
            "vote tuple is not canonical",
        ));
    }
    Ok(())
}

impl Default for Mempool {
    fn default() -> Self {
        Self::new(10_000)
    }
}

/// Mempool errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum MempoolError {
    #[error("invalid transaction envelope: {0}")]
    Envelope(EnvelopeError),
    #[error("bucket full")]
    BucketFull,
    #[error("transaction already exists")]
    Duplicate,
    #[error("signer nonce already exists in mempool")]
    DuplicateSignerNonce,
    #[error("address has too many pending transactions")]
    AddressLimitReached,
    #[error("evidence requires the verified system admission path")]
    EvidenceRequiresVerifiedAdmission,
    #[error("verified evidence queue is full")]
    EvidenceQueueFull,
    #[error("invalid evidence shape: {0}")]
    InvalidEvidenceShape(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{OrderType, Side};

    fn test_evidence(
        offender_byte: u8,
        context_epoch: u64,
        context_byte: u8,
        vote_byte: u8,
    ) -> Evidence {
        Evidence {
            evidence_type: crate::app::staking::EvidenceType::DoubleVote,
            offender: [offender_byte; 32],
            view: context_epoch + 1,
            timestamp: 0,
            context: crate::types::ConsensusContext::with_genesis(
                context_epoch,
                [context_byte; 32],
                [0xee; 32],
            ),
            hash_a: [1; 32],
            app_hash_a: [3; 32],
            hash_b: [2; 32],
            app_hash_b: [4; 32],
            signature_a: vec![vote_byte, 1],
            signature_b: vec![vote_byte, 2],
        }
    }

    fn test_evidence_tx(
        offender_byte: u8,
        context_epoch: u64,
        context_byte: u8,
        vote_byte: u8,
    ) -> Transaction {
        Transaction::SubmitEvidence {
            submitter: format!("system:equivocation:{}", hex::encode([offender_byte; 32])),
            evidence: test_evidence(offender_byte, context_epoch, context_byte, vote_byte),
        }
    }

    #[test]
    fn verified_evidence_reserve_bypasses_ordinary_limits_and_precedes_bucket0() {
        let mut mempool = Mempool::with_config(1, 1, 0);
        let submitter = format!("system:equivocation:{}", hex::encode([7u8; 32]));

        // Fill both the ordinary bucket-0 slot and this submitter's address
        // quota.  Verified evidence must use its independent reserve.
        mempool
            .add(
                Transaction::Deposit {
                    trader: submitter.clone(),
                    amount: 1,
                },
                0,
            )
            .unwrap();

        let evidence_hash = mempool
            .add_verified_evidence(test_evidence_tx(7, 1, 1, 1), 0)
            .unwrap();

        assert_eq!(mempool.len(), 2);
        assert_eq!(mempool.bucket_counts(), (2, 0, 0));
        assert_eq!(mempool.per_address_count.get(&submitter), Some(&1));

        let proposal = mempool.peek_consensus_block(2, 1);
        assert_eq!(proposal.len(), 2);
        assert_eq!(proposal[0].1, evidence_hash);
        assert!(matches!(
            proposal[0].0,
            ConsensusTransaction::System(Transaction::SubmitEvidence { .. })
        ));
        assert!(matches!(
            proposal[1].0,
            ConsensusTransaction::System(Transaction::Deposit { .. })
        ));
    }

    #[test]
    fn verified_evidence_queue_is_bounded_without_fifo_eviction() {
        let mut mempool = Mempool::with_config(1, 1, 0);
        let first_evidence = test_evidence(0, 0, 0, 0);
        let first_hash = mempool
            .add_verified_evidence(
                Transaction::SubmitEvidence {
                    submitter: format!(
                        "system:equivocation:{}",
                        hex::encode(first_evidence.offender)
                    ),
                    evidence: first_evidence.clone(),
                },
                0,
            )
            .unwrap();

        for index in 1u16..MAX_PENDING_EVIDENCE as u16 {
            let byte = index as u8;
            mempool
                .add_verified_evidence(test_evidence_tx(byte, index as u64, byte, byte), 0)
                .unwrap();
        }
        assert_eq!(mempool.len(), MAX_PENDING_EVIDENCE);

        let result = mempool.add_verified_evidence(test_evidence_tx(0, 256, 0, 1), 0);
        assert!(matches!(result, Err(MempoolError::EvidenceQueueFull)));
        assert_eq!(mempool.len(), MAX_PENDING_EVIDENCE);
        assert_eq!(
            mempool.find_equivocation_evidence_hash(&first_evidence),
            Some(first_hash)
        );
    }

    #[test]
    fn duplicate_evidence_key_is_idempotent() {
        let mut mempool = Mempool::with_config(1, 1, 0);
        let first_hash = mempool
            .add_verified_evidence(test_evidence_tx(9, 4, 4, 1), 0)
            .unwrap();
        let duplicate_hash = mempool
            .add_verified_evidence(test_evidence_tx(9, 4, 4, 2), 0)
            .unwrap();

        assert_eq!(duplicate_hash, first_hash);
        assert_eq!(mempool.len(), 1);
    }

    #[test]
    fn evidence_commit_clears_reservation_and_canonical_key() {
        let mut mempool = Mempool::with_config(1, 1, 0);
        let local_evidence = test_evidence(11, 5, 5, 1);
        let committed_evidence = test_evidence(11, 5, 5, 2);
        let local_hash = mempool
            .add_verified_evidence(
                Transaction::SubmitEvidence {
                    submitter: format!(
                        "system:equivocation:{}",
                        hex::encode(local_evidence.offender)
                    ),
                    evidence: local_evidence.clone(),
                },
                0,
            )
            .unwrap();

        assert_eq!(mempool.peek_consensus_block(1, 7).len(), 1);
        mempool.commit_proposal_unchecked(&[local_hash]);
        assert!(mempool.is_empty());
        assert!(mempool
            .find_equivocation_evidence_hash(&local_evidence)
            .is_none());

        // The reservation was released, and a different proof for the same
        // canonical key can be admitted after the committed one is removed.
        let replacement_hash = mempool
            .add_verified_evidence(
                Transaction::SubmitEvidence {
                    submitter: format!(
                        "system:equivocation:{}",
                        hex::encode(committed_evidence.offender)
                    ),
                    evidence: committed_evidence.clone(),
                },
                0,
            )
            .unwrap();
        assert_ne!(replacement_hash, local_hash);

        // Canonical reconciliation also removes a local alternative whose
        // exact hash differs from the committed proof.
        assert_eq!(mempool.remove_equivocation_evidence(&local_evidence), 1);
        assert!(mempool.is_empty());
    }

    #[test]
    fn test_bucket_ordering() {
        let mut mempool = Mempool::new(100);

        // Add transactions out of order
        mempool
            .add(
                Transaction::PlaceOrder {
                    trader: "alice".into(),
                    symbol: "BTC-USDT".into(),
                    side: Side::Bid,
                    price: 50000,
                    size: 100,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                },
                1,
            )
            .unwrap();

        mempool
            .add(
                Transaction::CancelOrder {
                    trader: "bob".into(),
                    order_id: "order1".into(),
                },
                2,
            )
            .unwrap();

        mempool
            .add(
                Transaction::Deposit {
                    trader: "charlie".into(),
                    amount: 10000,
                },
                3,
            )
            .unwrap();

        // Should come out in bucket order
        let block = mempool.prepare_block(10);

        assert_eq!(block.len(), 3);
        assert!(matches!(block[0], Transaction::Deposit { .. }));
        assert!(matches!(block[1], Transaction::CancelOrder { .. }));
        assert!(matches!(block[2], Transaction::PlaceOrder { .. }));
    }

    #[test]
    fn test_fifo_within_bucket() {
        let mut mempool = Mempool::new(100);

        // Add two orders
        mempool
            .add(
                Transaction::PlaceOrder {
                    trader: "alice".into(),
                    symbol: "BTC-USDT".into(),
                    side: Side::Bid,
                    price: 50000,
                    size: 100,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                },
                1,
            )
            .unwrap();

        mempool
            .add(
                Transaction::PlaceOrder {
                    trader: "bob".into(),
                    symbol: "BTC-USDT".into(),
                    side: Side::Ask,
                    price: 51000,
                    size: 100,
                    order_type: OrderType::Gtc,
                    reduce_only: false,
                },
                2,
            )
            .unwrap();

        let block = mempool.prepare_block(10);

        // First order should be alice's (FIFO)
        if let Transaction::PlaceOrder { trader, .. } = &block[0] {
            assert_eq!(trader, "alice");
        } else {
            panic!("Expected PlaceOrder");
        }
    }

    #[test]
    fn test_bucket_full() {
        let mut mempool = Mempool::new(1);

        mempool
            .add(
                Transaction::Deposit {
                    trader: "alice".into(),
                    amount: 100,
                },
                1,
            )
            .unwrap();

        // Should fail - bucket full
        let result = mempool.add(
            Transaction::Deposit {
                trader: "bob".into(),
                amount: 200,
            },
            2,
        );

        assert!(matches!(result, Err(MempoolError::BucketFull)));
    }

    #[test]
    fn test_max_txs_limit() {
        let mut mempool = Mempool::new(100);

        for i in 0..10 {
            mempool
                .add(
                    Transaction::Deposit {
                        trader: format!("trader{}", i),
                        amount: 100,
                    },
                    i as u64,
                )
                .unwrap();
        }

        // Only get 3
        let block = mempool.prepare_block(3);
        assert_eq!(block.len(), 3);

        // 7 should remain
        assert_eq!(mempool.len(), 7);
    }

    #[test]
    fn test_per_address_limit() {
        // Max 3 pending txs per address
        let mut mempool = Mempool::with_config(100, 3, 0);

        // Add 3 txs from alice - should work
        for i in 0..3 {
            mempool
                .add(
                    Transaction::Deposit {
                        trader: "alice".into(),
                        amount: 100 + i,
                    },
                    i as u64,
                )
                .unwrap();
        }

        // 4th tx from alice should fail
        let result = mempool.add(
            Transaction::Deposit {
                trader: "alice".into(),
                amount: 200,
            },
            4,
        );
        assert!(matches!(result, Err(MempoolError::AddressLimitReached)));

        // But bob can still add
        mempool
            .add(
                Transaction::Deposit {
                    trader: "bob".into(),
                    amount: 100,
                },
                5,
            )
            .unwrap();
    }

    #[test]
    fn test_per_address_count_tracking() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        // Add 3 txs from alice
        let mut hashes = Vec::new();
        for i in 0..3 {
            let hash = mempool
                .add(
                    Transaction::Deposit {
                        trader: "alice".into(),
                        amount: 100 + i,
                    },
                    i as u64,
                )
                .unwrap();
            hashes.push(hash);
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&3));

        // Commit 2 txs
        mempool.commit_proposal_unchecked(&hashes[0..2]);

        // Count should decrement to 1
        assert_eq!(mempool.per_address_count.get("alice"), Some(&1));
        assert_eq!(mempool.len(), 1);

        // Remove last one
        mempool.remove(&hashes[2]);
        assert_eq!(mempool.per_address_count.get("alice"), None);
    }

    #[test]
    fn test_age_eviction() {
        // Max age: 1000ms
        let mut mempool = Mempool::with_config(100, 100, 1000);

        // Add old tx (timestamp 0)
        mempool
            .add(
                Transaction::Deposit {
                    trader: "alice".into(),
                    amount: 100,
                },
                0,
            )
            .unwrap();

        // Add recent tx (timestamp 900)
        mempool
            .add(
                Transaction::Deposit {
                    trader: "bob".into(),
                    amount: 200,
                },
                900,
            )
            .unwrap();

        assert_eq!(mempool.len(), 2);

        // Prune at time 1001 (alice's tx is stale)
        let pruned = mempool.prune_stale(1001);
        assert_eq!(pruned, 1);
        assert_eq!(mempool.len(), 1);

        // Bob's tx should remain
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 1);
        if let Transaction::Deposit { trader, .. } = &block[0] {
            assert_eq!(trader, "bob");
        }
    }

    #[test]
    fn test_age_eviction_disabled() {
        // Max age: 0 = disabled
        let mut mempool = Mempool::with_config(100, 100, 0);

        mempool
            .add(
                Transaction::Deposit {
                    trader: "alice".into(),
                    amount: 100,
                },
                0,
            )
            .unwrap();

        // Should not prune even at very high time
        let pruned = mempool.prune_stale(1_000_000_000);
        assert_eq!(pruned, 0);
        assert_eq!(mempool.len(), 1);
    }

    #[test]
    fn system_evidence_is_not_age_evicted_but_remains_count_bounded() {
        let mut mempool = Mempool::with_config(2, 100, 10);
        let evidence = Evidence {
            evidence_type: crate::app::staking::EvidenceType::DoubleVote,
            offender: [7u8; 32],
            view: 1,
            timestamp: 0,
            context: crate::types::ConsensusContext::with_genesis(0, [1u8; 32], [2u8; 32]),
            hash_a: [1u8; 32],
            app_hash_a: [3u8; 32],
            hash_b: [2u8; 32],
            app_hash_b: [4u8; 32],
            signature_a: vec![1],
            signature_b: vec![2],
        };
        let transaction = Transaction::SubmitEvidence {
            submitter: "system:equivocation:0707070707070707070707070707070707070707070707070707070707070707"
                .to_string(),
            evidence,
        };
        mempool.add_verified_evidence(transaction, 0).unwrap();
        mempool
            .add(
                Transaction::Deposit {
                    trader: "ordinary".to_string(),
                    amount: 1,
                },
                0,
            )
            .unwrap();

        assert_eq!(mempool.prune_stale(1_000), 1);
        assert_eq!(mempool.len(), 1);
        assert!(matches!(
            mempool.peek_consensus_block_txs(1)[0],
            ConsensusTransaction::System(Transaction::SubmitEvidence { .. })
        ));
    }

    #[test]
    fn test_prepare_block_decrements_address_count() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        // Add 5 txs from alice
        for i in 0..5 {
            mempool
                .add(
                    Transaction::Deposit {
                        trader: "alice".into(),
                        amount: 100 + i,
                    },
                    i as u64,
                )
                .unwrap();
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&5));

        // prepare_block should drain and decrement counts
        let block = mempool.prepare_block(3);
        assert_eq!(block.len(), 3);
        assert_eq!(mempool.per_address_count.get("alice"), Some(&2));

        // Drain remaining
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 2);
        assert_eq!(mempool.per_address_count.get("alice"), None); // Removed when 0
    }

    #[test]
    fn test_prepare_block_allows_resubmission() {
        // Regression: prepare_block must decrement counts so new txs can be submitted
        let mut mempool = Mempool::with_config(100, 3, 0);

        // Fill to address limit
        for i in 0..3 {
            mempool
                .add(
                    Transaction::Deposit {
                        trader: "alice".into(),
                        amount: 100 + i,
                    },
                    i as u64,
                )
                .unwrap();
        }

        // Should be at limit
        let result = mempool.add(
            Transaction::Deposit {
                trader: "alice".into(),
                amount: 999,
            },
            10,
        );
        assert!(matches!(result, Err(MempoolError::AddressLimitReached)));

        // Drain all via prepare_block
        let block = mempool.prepare_block(10);
        assert_eq!(block.len(), 3);

        // Should be able to submit again
        mempool
            .add(
                Transaction::Deposit {
                    trader: "alice".into(),
                    amount: 200,
                },
                11,
            )
            .unwrap();
        assert_eq!(mempool.per_address_count.get("alice"), Some(&1));
    }

    #[test]
    fn test_drain_block_decrements_address_count() {
        let mut mempool = Mempool::with_config(100, 10, 0);

        for i in 0..4 {
            mempool
                .add(
                    Transaction::Deposit {
                        trader: "alice".into(),
                        amount: 100 + i,
                    },
                    i as u64,
                )
                .unwrap();
        }

        assert_eq!(mempool.per_address_count.get("alice"), Some(&4));

        // drain_block should also decrement counts
        mempool.drain_block(2);
        assert_eq!(mempool.per_address_count.get("alice"), Some(&2));
        assert_eq!(mempool.len(), 2);

        mempool.drain_block(2);
        assert_eq!(mempool.per_address_count.get("alice"), None);
        assert_eq!(mempool.len(), 0);
    }

    #[test]
    fn signed_nonce_index_clears_on_commit_and_prune() {
        let signer = crate::crypto::Signer::generate();
        let address = format!("{:?}", signer.address());
        let envelope = crate::app::SignedEnvelope::sign(
            [3u8; 32],
            &signer,
            0,
            0,
            1_000,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 100,
            },
        )
        .unwrap();
        let mut mempool = Mempool::with_config(100, 10, 10);
        let hash = mempool.add_envelope(envelope, 0).unwrap();
        assert!(mempool.contains_signer_nonce(&address, 0));
        let different_action = crate::app::SignedEnvelope::sign(
            [3u8; 32],
            &signer,
            0,
            0,
            1_000,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 200,
            },
        )
        .unwrap();
        assert!(matches!(
            mempool.add_envelope(different_action, 0),
            Err(MempoolError::DuplicateSignerNonce)
        ));

        // Pruning a proposed transaction must also clear its proposal index,
        // otherwise a re-submission with the same canonical bytes would be
        // silently skipped for the rest of the view.
        assert_eq!(mempool.peek_consensus_block(1, 1).len(), 1);

        mempool.commit_proposal_unchecked(&[hash]);
        assert!(!mempool.contains_signer_nonce(&address, 0));

        let envelope = crate::app::SignedEnvelope::sign(
            [3u8; 32],
            &signer,
            0,
            0,
            1_000,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 100,
            },
        )
        .unwrap();
        mempool.add_envelope(envelope, 0).unwrap();
        assert_eq!(mempool.peek_consensus_block(1, 1).len(), 1);
        assert_eq!(mempool.prune_stale(11), 1);
        assert!(!mempool.contains_signer_nonce(&address, 0));

        let envelope = crate::app::SignedEnvelope::sign(
            [3u8; 32],
            &signer,
            0,
            0,
            1_000,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 100,
            },
        )
        .unwrap();
        mempool.add_envelope(envelope, 11).unwrap();
        assert_eq!(mempool.peek_consensus_block(1, 1).len(), 1);
    }

    #[test]
    fn pending_user_envelopes_skip_invalid_windows_and_follow_mempool_lifetime() {
        let chain_domain = [3u8; 32];
        let active_signer = crate::crypto::Signer::generate();
        let future_signer = crate::crypto::Signer::generate();
        let expired_signer = crate::crypto::Signer::generate();
        let active_address = format!("{:?}", active_signer.address());
        let future_address = format!("{:?}", future_signer.address());
        let expired_address = format!("{:?}", expired_signer.address());
        let active = SignedEnvelope::sign(
            chain_domain,
            &active_signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: active_address,
                amount: 1,
            },
        )
        .unwrap();
        let active_hash = active.hash().unwrap();
        let future = SignedEnvelope::sign(
            chain_domain,
            &future_signer,
            0,
            50,
            100,
            Transaction::Deposit {
                trader: future_address,
                amount: 1,
            },
        )
        .unwrap();
        let expired = SignedEnvelope::sign(
            chain_domain,
            &expired_signer,
            0,
            0,
            9,
            Transaction::Deposit {
                trader: expired_address,
                amount: 1,
            },
        )
        .unwrap();

        let mut mempool = Mempool::with_config(100, 10, 0);
        mempool.add_envelope(active, 0).unwrap();
        mempool.add_envelope(future, 0).unwrap();
        mempool.add_envelope(expired, 0).unwrap();

        let (eligible, cursor) = mempool.pending_user_envelopes_batch_at(10, 0, 10, usize::MAX);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].hash().unwrap(), active_hash);
        assert_eq!(cursor, 0);

        // Once the canonical proposal commits, the source snapshot no longer
        // contains the envelope and the retry worker naturally stops sending it.
        mempool.commit_proposal_unchecked(&[active_hash]);
        assert!(mempool
            .pending_user_envelopes_batch_at(10, 0, 10, usize::MAX)
            .0
            .is_empty());
        assert_eq!(
            mempool
                .pending_user_envelopes_batch_at(50, 0, 10, usize::MAX)
                .0
                .len(),
            1
        );
    }

    #[test]
    fn pending_user_envelope_batch_enforces_count_bytes_age_and_user_only_filter() {
        let chain_domain = [3u8; 32];
        let signer_a = crate::crypto::Signer::generate();
        let signer_b = crate::crypto::Signer::generate();
        let signer_old = crate::crypto::Signer::generate();
        let signer_future = crate::crypto::Signer::generate();
        let signer_expired = crate::crypto::Signer::generate();

        let signed =
            |signer: &crate::crypto::Signer, nonce: u64, valid_after: u64, valid_until: u64| {
                let trader = format!("{:?}", signer.address());
                SignedEnvelope::sign(
                    chain_domain,
                    signer,
                    nonce,
                    valid_after,
                    valid_until,
                    Transaction::Deposit { trader, amount: 1 },
                )
                .unwrap()
            };

        let active_a = signed(&signer_a, 0, 0, 1_000);
        let active_b = signed(&signer_b, 0, 0, 1_000);
        let old = signed(&signer_old, 0, 0, 1_000);
        let future = signed(&signer_future, 0, 101, 1_000);
        let expired = signed(&signer_expired, 0, 0, 99);
        let active_a_hash = active_a.hash().unwrap();
        let active_a_bytes = active_a.encoded_bytes().unwrap().len();

        let mut mempool = Mempool::with_config(100, 10, 10);
        mempool.add_envelope(active_a, 95).unwrap();
        mempool.add_envelope(active_b, 96).unwrap();
        mempool.add_envelope(old, 80).unwrap();
        mempool.add_envelope(future, 100).unwrap();
        mempool.add_envelope(expired, 100).unwrap();
        mempool
            .add(
                Transaction::Deposit {
                    trader: "system-only".into(),
                    amount: 1,
                },
                100,
            )
            .unwrap();

        let (batch, _) = mempool.pending_user_envelopes_batch_at(100, 0, 10, usize::MAX);
        assert_eq!(batch.len(), 2);
        assert!(batch.iter().all(|envelope| envelope.valid_until == 1_000));

        let (count_limited, _) = mempool.pending_user_envelopes_batch_at(100, 0, 1, usize::MAX);
        assert_eq!(count_limited.len(), 1);

        let (byte_limited, _) = mempool.pending_user_envelopes_batch_at(100, 0, 10, active_a_bytes);
        assert_eq!(byte_limited.len(), 1);

        // Committed entries disappear from the mempool-backed retry source;
        // stale, future-window, expired, and unsigned system entries were
        // never eligible in the first place.
        mempool.commit_proposal_unchecked(&[active_a_hash]);
        let (after_commit, _) = mempool.pending_user_envelopes_batch_at(100, 0, 10, usize::MAX);
        assert_eq!(after_commit.len(), 1);
        assert_eq!(after_commit[0].valid_until, 1_000);
    }

    #[test]
    fn pending_user_envelope_batch_advances_after_byte_budget_boundary() {
        let chain_domain = [4u8; 32];
        let signers: Vec<_> = (0..3).map(|_| crate::crypto::Signer::generate()).collect();
        let envelopes: Vec<_> = signers
            .iter()
            .map(|signer| {
                let trader = format!("{:?}", signer.address());
                SignedEnvelope::sign(
                    chain_domain,
                    signer,
                    0,
                    0,
                    1_000,
                    Transaction::Deposit { trader, amount: 1 },
                )
                .unwrap()
            })
            .collect();
        let one_envelope_bytes = envelopes[0].encoded_bytes().unwrap().len();
        let expected_hashes: Vec<_> = envelopes
            .iter()
            .map(|envelope| envelope.hash().unwrap())
            .collect();
        let mut mempool = Mempool::with_config(100, 10, 0);
        for envelope in envelopes {
            mempool.add_envelope(envelope, 0).unwrap();
        }

        let mut cursor = 0;
        let mut selected_hashes = Vec::new();
        for _ in 0..3 {
            let (batch, next_cursor) =
                mempool.pending_user_envelopes_batch_at(100, cursor, 10, one_envelope_bytes);
            assert_eq!(batch.len(), 1);
            selected_hashes.push(batch[0].hash().unwrap());
            cursor = next_cursor;
        }
        assert_eq!(selected_hashes, expected_hashes);
    }
}
