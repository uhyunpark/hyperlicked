//! Consensus Integration
//!
//! AppHook implementation, state hashing, and snapshot support.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::app::{
    accounts::{AccountError, AccountManager},
    candles::CandleManager,
    mempool::Mempool,
    orderbook::OrderBook,
    staking::{
        rewards::{RewardAccrualResult, STAKING_REWARD_EVENT_SCHEMA_VERSION},
        Evidence, EvidenceType,
    },
    ConsensusTransaction, MarketConfig, SignedEnvelope, Symbol, Transaction,
};
use crate::consensus::AppHook;
use crate::types::{Block, CommitmentV2, Hash, ReceiptStatus};

use super::{
    artifacts::{PendingEventCursor, StakingRewardEpochInfo},
    AppError, AppState, BlockExecutionArtifacts, CowMap,
};

fn unique_snapshot_pairs<K: Eq + std::hash::Hash, V>(
    pairs: Vec<(K, V)>,
    field: &str,
) -> Result<HashMap<K, V>, String> {
    let mut values = HashMap::with_capacity(pairs.len());
    for (key, value) in pairs {
        if values.insert(key, value).is_some() {
            return Err(format!("snapshot contains duplicate {field} key"));
        }
    }
    Ok(values)
}

impl AppState {
    /// Select a deterministic consensus payload while keeping each signer's
    /// transactions in strict contiguous nonce order.
    ///
    /// Admission intentionally accepts a bounded future nonce so transport
    /// reordering does not reject otherwise valid envelopes.  A proposer must
    /// not, however, put a future nonce into a block before its predecessor:
    /// the predecessor may still be in flight.  Such a chain is deferred as a
    /// whole, while ready chains from other signers continue to make progress.
    fn prepare_consensus_transactions(&self, max_txs: usize) -> Vec<ConsensusTransaction> {
        if max_txs == 0 || self.mempool.is_empty() {
            return Vec::new();
        }

        // Scan the complete bounded mempool view. Applying the limit before
        // nonce scheduling could let a blocked future nonce hide ready
        // transactions from other signers later in the queues. The mempool
        // iterator exposes references, so only selected entries are cloned.
        let pending_len = self.mempool.len();
        if pending_len == 0 {
            return Vec::new();
        }

        #[derive(Clone)]
        struct Candidate<'a> {
            index: usize,
            bucket: u8,
            nonce: u64,
            entry: &'a ConsensusTransaction,
        }

        let mut result = Vec::with_capacity(max_txs.min(pending_len));
        let mut evidence = Vec::new();
        let mut ordinary_system = Vec::new();
        let mut by_signer: HashMap<String, Vec<Candidate<'_>>> = HashMap::new();

        for (index, entry) in self.mempool.consensus_block_entries() {
            match entry {
                ConsensusTransaction::System(transaction)
                    if matches!(transaction, Transaction::SubmitEvidence { .. }) =>
                {
                    evidence.push(entry);
                }
                ConsensusTransaction::System(transaction) => {
                    ordinary_system.push((transaction.bucket(), index, entry));
                }
                ConsensusTransaction::Signed(envelope) => {
                    let signer = envelope.signer_address().to_ascii_lowercase();
                    let bucket = envelope.action.bucket();
                    by_signer.entry(signer).or_default().push(Candidate {
                        index,
                        bucket,
                        nonce: envelope.nonce,
                        entry,
                    });
                }
            }
        }

        // Evidence has an independent safety queue and retains its existing
        // first-in-block priority.
        for entry in evidence {
            if result.len() >= max_txs {
                return result;
            }
            result.push(entry.clone());
        }

        // Build one ready chain per signer.  Sorting by nonce makes a
        // cross-bucket predecessor discoverable even when it arrived later in
        // the FIFO queue.  A missing predecessor stops only that signer.
        let mut chains = Vec::with_capacity(by_signer.len());
        for (signer, mut candidates) in by_signer {
            candidates
                .sort_by_key(|candidate| (candidate.nonce, candidate.bucket, candidate.index));
            let mut expected = self.accounts.get_nonce(&signer);
            let mut chain = Vec::new();
            for candidate in candidates {
                if candidate.nonce < expected {
                    // Stale/replayed mempool entries are not proposal input.
                    continue;
                }
                if candidate.nonce != expected || expected == u64::MAX {
                    // A future nonce without its predecessor is deferred.  A
                    // terminal nonce cannot be consumed safely either.
                    break;
                }
                chain.push(candidate);
                expected = match expected.checked_add(1) {
                    Some(next) => next,
                    None => break,
                };
            }
            if !chain.is_empty() {
                chains.push(chain);
            }
        }

        // Merge ready signer chains with unsigned/system transactions using
        // the existing bucket priority and FIFO index.  The merge is
        // deterministic because every candidate retains its original queue
        // index, while chain order is fixed by nonce.
        ordinary_system.sort_by_key(|(bucket, index, _)| (*bucket, *index));
        let mut ordinary_index = 0usize;
        let mut chain_indices = vec![0usize; chains.len()];
        let mut merge_heap: BinaryHeap<Reverse<(u8, usize, usize)>> = BinaryHeap::new();

        if let Some((bucket, index, _)) = ordinary_system.first() {
            // Source 0 is the ordinary system queue. Signer chains use
            // one-based source ids, so each heap item can advance exactly
            // one K-way merge stream after it is selected.
            merge_heap.push(Reverse((*bucket, *index, 0)));
        }
        for (chain_index, chain) in chains.iter().enumerate() {
            if let Some(candidate) = chain.first() {
                merge_heap.push(Reverse((
                    candidate.bucket,
                    candidate.index,
                    chain_index + 1,
                )));
            }
        }

        while result.len() < max_txs {
            let Some(Reverse((_, _, source))) = merge_heap.pop() else {
                break;
            };

            if source == 0 {
                let entry = ordinary_system[ordinary_index].2.clone();
                ordinary_index += 1;
                result.push(entry);
                if let Some((bucket, index, _)) = ordinary_system.get(ordinary_index) {
                    merge_heap.push(Reverse((*bucket, *index, 0)));
                }
            } else {
                let chain_index = source - 1;
                let candidate_index = chain_indices[chain_index];
                let entry = chains[chain_index][candidate_index].entry.clone();
                chain_indices[chain_index] += 1;
                result.push(entry);
                if let Some(candidate) = chains[chain_index].get(chain_indices[chain_index]) {
                    merge_heap.push(Reverse((candidate.bucket, candidate.index, source)));
                }
            }
        }

        result
    }

    /// Validate the privileged system evidence transaction at the consensus
    /// boundary.  This is intentionally independent from `execute_tx`: an
    /// invalid proposer payload must be rejected before a trial or canonical
    /// application state is mutated.
    pub(crate) fn validate_system_transaction(
        &self,
        transaction: &Transaction,
    ) -> Result<(), String> {
        let Transaction::SubmitEvidence {
            submitter,
            evidence,
        } = transaction
        else {
            return Ok(());
        };

        let expected_submitter = format!("system:equivocation:{}", hex::encode(evidence.offender));
        if submitter != &expected_submitter {
            return Err("equivocation evidence has an invalid system submitter".to_string());
        }
        if evidence.timestamp != 0 {
            return Err("equivocation evidence timestamp must be zero".to_string());
        }

        let first = (evidence.hash_a, evidence.app_hash_a, &evidence.signature_a);
        let second = (evidence.hash_b, evidence.app_hash_b, &evidence.signature_b);
        if second < first {
            return Err("equivocation evidence vote tuple is not canonical".to_string());
        }

        let expected_context = self
            .staking
            .static_consensus_context()
            .ok_or_else(|| "equivocation evidence has no current consensus context".to_string())?;
        if evidence.context != expected_context {
            return Err("equivocation evidence context does not match current context".to_string());
        }
        if !self.staking.validate_evidence(evidence) {
            return Err("equivocation evidence failed validator proof verification".to_string());
        }
        Ok(())
    }

    /// Convert a consensus detector proof into the application evidence
    /// transaction.  The timestamp is intentionally not sourced from local
    /// wall-clock state: it is not part of the authenticated proof and must
    /// not make the same proof hash differently on different nodes.
    pub(crate) fn equivocation_evidence_from_proof(
        &self,
        proof: &crate::consensus::EquivocationProof,
    ) -> Evidence {
        let first = (proof.hash_a, proof.app_hash_a, &proof.signature_a);
        let second = (proof.hash_b, proof.app_hash_b, &proof.signature_b);
        let reverse = second < first;
        let (hash_a, app_hash_a, signature_a, hash_b, app_hash_b, signature_b) = if reverse {
            (
                proof.hash_b,
                proof.app_hash_b,
                proof.signature_b.clone(),
                proof.hash_a,
                proof.app_hash_a,
                proof.signature_a.clone(),
            )
        } else {
            (
                proof.hash_a,
                proof.app_hash_a,
                proof.signature_a.clone(),
                proof.hash_b,
                proof.app_hash_b,
                proof.signature_b.clone(),
            )
        };

        Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: proof.offender,
            view: proof.view,
            timestamp: 0,
            context: proof.context,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a,
            signature_b,
        }
    }

    /// Verify and enqueue an equivocation proof as a local proposal input.
    ///
    /// This method only changes the node-local mempool.  Validator status,
    /// stake, pending evidence, and every state-root component remain
    /// untouched until the resulting `SubmitEvidence` transaction is included
    /// in and executed from a block.
    pub(crate) fn enqueue_equivocation_evidence(
        &mut self,
        proof: crate::consensus::EquivocationProof,
    ) -> bool {
        let evidence = self.equivocation_evidence_from_proof(&proof);
        if !self.staking.validate_evidence(&evidence) {
            tracing::warn!(
                offender = %hex::encode(&proof.offender[..4]),
                view = proof.view,
                "Rejected invalid equivocation evidence before mempool admission"
            );
            return false;
        }

        if self
            .mempool
            .find_equivocation_evidence_hash(&evidence)
            .is_some()
        {
            return true;
        }

        let transaction = Transaction::SubmitEvidence {
            submitter: format!("system:equivocation:{}", hex::encode(proof.offender)),
            evidence,
        };
        self.validate_system_transaction(&transaction).is_ok()
            && self
                .mempool
                .add_verified_evidence(transaction, self.timestamp)
                .is_ok()
    }

    /// Decode the only transaction encoding accepted in consensus payloads.
    /// Re-serializing and comparing bytes rejects legacy payloads and trailing
    /// or non-canonical bincode encodings before any application mutation.
    pub fn decode_consensus_payload(payload: &[u8]) -> Result<Vec<ConsensusTransaction>, String> {
        if payload.is_empty() {
            return Ok(Vec::new());
        }
        let entries: Vec<ConsensusTransaction> = bincode::deserialize(payload)
            .map_err(|error| format!("invalid consensus transaction payload: {error}"))?;
        let canonical = bincode::serialize(&entries)
            .map_err(|error| format!("cannot re-encode consensus payload: {error}"))?;
        if canonical != payload {
            return Err("non-canonical consensus transaction payload".to_string());
        }
        Ok(entries)
    }

    /// Promote a speculative state's non-mempool data while preserving the
    /// current API-visible mempool. Candidate execution intentionally skips
    /// mempool mutation because its snapshot may predate later submissions.
    pub(crate) fn reconcile_canonical_mempool(
        &mut self,
        canonical: &Self,
        block: &Block,
    ) -> Result<(), String> {
        let entries = Self::decode_consensus_payload(&block.payload)?;
        let tx_hashes: Vec<_> = entries
            .iter()
            .map(ConsensusTransaction::hash)
            .collect::<Result<_, _>>()
            .map_err(|error| format!("cannot hash committed transaction: {error}"))?;

        self.mempool = canonical.mempool.clone();
        self.speculative_execution = false;
        self.mempool.prune_stale(block.timestamp);
        if !tx_hashes.is_empty() {
            self.mempool.commit_proposal_unchecked(&tx_hashes);
        }
        // A follower may have observed a different valid vote pair for the
        // same offender/context.  Remove that local alternative as well as
        // the exact committed transaction identity.
        for entry in &entries {
            if let ConsensusTransaction::System(Transaction::SubmitEvidence { evidence, .. }) =
                entry
            {
                self.mempool.remove_equivocation_evidence(evidence);
            }
        }
        Ok(())
    }

    /// Validate payload encoding, envelope cryptography, signer nonce order,
    /// and production unsigned-transaction policy without mutating canonical
    /// state.  A valid envelope whose action fails is still a valid block:
    /// `execute_consensus_transaction` consumes its nonce and rolls back the
    /// failed action on the validation copy.
    pub fn validate_consensus_block(&self, block: &Block) -> Result<(), String> {
        if block.genesis_hash != self.chain_domain {
            return Err(format!(
                "block genesis domain {} does not match application chain domain {}",
                hex::encode(block.genesis_hash),
                hex::encode(self.chain_domain)
            ));
        }
        if self.staking.static_committee_binding_pending() {
            return Err(
                "static consensus committee must be rebound after snapshot restore before block execution"
                    .to_string(),
            );
        }
        block.validate_parent_timestamp(self.timestamp)?;
        let entries = Self::decode_consensus_payload(&block.payload)?;
        for entry in &entries {
            if let ConsensusTransaction::System(transaction) = entry {
                self.validate_system_transaction(transaction)?;
            }
        }
        let mut trial = self.clone();
        for entry in entries {
            if let ConsensusTransaction::Signed(envelope) = &entry {
                envelope
                    .validate_for_block(
                        self.chain_domain,
                        block.timestamp,
                        self.allow_dev_envelopes,
                    )
                    .map_err(|error| error.to_string())?;
                let signer = envelope.signer_address();
                let expected_nonce = trial.accounts.get_nonce(&signer);
                if expected_nonce == u64::MAX || expected_nonce != envelope.nonce {
                    return Err(format!(
                        "invalid signer nonce at block {}: signer {}, expected {}, got {}",
                        block.height, signer, expected_nonce, envelope.nonce
                    ));
                }
            }

            match trial.execute_consensus_transaction(entry, block.timestamp) {
                Ok(_) => {}
                Err(AppError::InvalidEnvelope(error)) => return Err(error),
                Err(AppError::Account(error))
                    if matches!(
                        error,
                        AccountError::InvalidNonce { .. }
                            | AccountError::NonceGapTooLarge { .. }
                            | AccountError::NonceAlreadyUsed { .. }
                            | AccountError::NonceOverflow
                    ) =>
                {
                    // Nonce-policy violations are block-invalid. They must
                    // never be downgraded to an action-failure receipt.
                    return Err(error.to_string());
                }
                // Action failure is deterministic but does not invalidate the
                // block. The execution method has already consumed a valid
                // envelope nonce and discarded failed action mutations.
                Err(_) => {}
            }
        }
        Ok(())
    }

    /// Compute the authenticated fixed-supply full-state root used by
    /// `Block::app_hash`.
    ///
    /// Keeping this entry point as the application's state-hash API prevents
    /// a raw application runner from silently returning the retired flat
    /// hash.  The legacy encoder remains available only through the explicit
    /// [`Self::compute_state_hash_full`] compatibility/audit helper.
    pub fn compute_state_hash(&self) -> Hash {
        self.compute_full_state_root()
    }

    /// Legacy flat state hash retained for compatibility/audit comparisons.
    ///
    /// This value is not a consensus commitment and must not be placed in a
    /// block header.
    pub fn compute_state_hash_full(&self) -> Hash {
        let mut hasher = Sha256::new();

        // === Accounts ===
        // Get all accounts sorted by address
        let accounts = self.accounts.all_accounts();
        let mut sorted_accounts: Vec<_> = accounts.iter().collect();
        sorted_accounts.sort_by_key(|a| &a.address);

        for account in sorted_accounts {
            hasher.update(account.address.as_bytes());
            hasher.update(account.hyck_balance.to_le_bytes());
            hasher.update(account.balance.to_le_bytes());
            hasher.update(account.locked.to_le_bytes());
            hasher.update(account.nonce.to_le_bytes());

            // Hash pending_nonces (already sorted since BTreeSet)
            // Only include if non-empty to maintain backward compat with old state
            if !account.pending_nonces.is_empty() {
                hasher.update(&[1u8]); // Flag: has pending nonces
                for pending_nonce in &account.pending_nonces {
                    hasher.update(pending_nonce.to_le_bytes());
                }
            } else {
                hasher.update(&[0u8]); // Flag: no pending nonces
            }

            // Hash positions for this account (sorted by symbol)
            let mut position_symbols: Vec<_> = account.positions.keys().collect();
            position_symbols.sort();

            for symbol in position_symbols {
                if let Some(pos) = account.positions.get(symbol) {
                    hasher.update(symbol.as_bytes());
                    hasher.update(pos.size.to_le_bytes());
                    hasher.update(pos.entry_price.to_le_bytes());
                    hasher.update(pos.realized_pnl.to_le_bytes());
                    hasher.update(pos.cumulative_funding.to_le_bytes());
                }
            }
        }

        // === Orderbooks ===
        let mut symbols: Vec<_> = self.orderbooks.keys().collect();
        symbols.sort();

        for symbol in &symbols {
            if let Some(book) = self.orderbooks.get(*symbol) {
                hasher.update(symbol.as_bytes());
                hasher.update(book.best_bid().unwrap_or(0).to_le_bytes());
                hasher.update(book.best_ask().unwrap_or(0).to_le_bytes());
                hasher.update(book.last_price().to_le_bytes());
            }
        }

        // === Mark prices (sorted) ===
        let mut mark_prices: Vec<_> = self.mark_prices.iter().collect();
        mark_prices.sort_by_key(|(k, _)| *k);
        for (symbol, price) in mark_prices {
            hasher.update(symbol.as_bytes());
            hasher.update(price.to_le_bytes());
        }

        // === Mark price EMA (sorted) ===
        let mut mark_ema: Vec<_> = self.mark_price_ema.iter().collect();
        mark_ema.sort_by_key(|(k, _)| *k);
        for (symbol, price) in mark_ema {
            hasher.update(b"ema:");
            hasher.update(symbol.as_bytes());
            hasher.update(price.to_le_bytes());
        }

        // === Insurance fund ===
        hasher.update(self.insurance_fund.to_le_bytes());

        // === Funding rates (sorted) ===
        let mut funding_rates: Vec<_> = self.current_funding_rates.iter().collect();
        funding_rates.sort_by_key(|(k, _)| *k);
        for (symbol, rate) in funding_rates {
            hasher.update(symbol.as_bytes());
            hasher.update(rate.to_le_bytes());
        }

        // === Last funding times (sorted) ===
        let mut last_funding: Vec<_> = self.last_funding_times.iter().collect();
        last_funding.sort_by_key(|(k, _)| *k);
        for (symbol, time) in last_funding {
            hasher.update(symbol.as_bytes());
            hasher.update(time.to_le_bytes());
        }

        // === Staking state ===
        hasher.update(self.staking.current_epoch.to_le_bytes());
        hasher.update(self.staking.total_staked.to_le_bytes());
        hasher.update(self.staking.emissions_reserve.to_le_bytes());
        hasher.update(self.staking.last_reward_accrual_timestamp.to_le_bytes());
        hasher.update([u8::from(self.staking.reward_clock_initialized)]);
        hasher.update(self.staking.reward_accrual_remainder.to_le_bytes());
        hasher.update(self.staking.last_reward_compound_timestamp.to_le_bytes());

        // Hash validators (sorted by operator)
        let mut validators: Vec<_> = self.staking.validators.iter().collect();
        validators.sort_by_key(|(k, _)| *k);
        for (operator, validator) in validators {
            use crate::app::staking::ValidatorStatus;

            hasher.update(operator.as_bytes());
            hasher.update(validator.self_stake.to_le_bytes());
            hasher.update(validator.total_stake.to_le_bytes());
            hasher.update(validator.commission_bps.to_le_bytes());
            hasher.update(validator.pending_rewards.to_le_bytes());
            hasher.update(validator.reward_eligible_stake.to_le_bytes());
            // Use explicit values for enum without repr(u8)
            let status_byte: u8 = match validator.status {
                ValidatorStatus::Active => 0,
                ValidatorStatus::Inactive => 1,
                ValidatorStatus::Jailed => 2,
                ValidatorStatus::Tombstoned => 3,
            };
            hasher.update(&[status_byte]);
            hasher.update(validator.jail_until.to_le_bytes());
        }

        // Hash delegations (sorted by delegator, then validator)
        let mut delegations: Vec<_> = self.staking.delegations.iter().collect();
        delegations.sort_by_key(|(k, _)| *k);
        for ((delegator, validator), delegation) in delegations {
            hasher.update(delegator.as_bytes());
            hasher.update(validator.as_bytes());
            hasher.update(delegation.amount.to_le_bytes());
            hasher.update(delegation.pending_rewards.to_le_bytes());
            hasher.update(delegation.reward_eligible_stake.to_le_bytes());
        }

        // === Trigger orders (sorted by ID) ===
        let mut trigger_orders: Vec<_> = self.trigger_orders.iter().collect();
        trigger_orders.sort_by_key(|(k, _)| *k);
        for (id, order) in trigger_orders {
            use crate::app::trigger::{TriggerCondition, TriggerOrderStatus, TriggerType};

            hasher.update(id.as_bytes());
            hasher.update(order.trader.as_bytes());
            hasher.update(order.symbol.as_bytes());
            // Use explicit values for enums without repr(u8)
            let side_byte: u8 = match order.side {
                crate::app::Side::Bid => 0,
                crate::app::Side::Ask => 1,
            };
            hasher.update(&[side_byte]);
            hasher.update(order.size.to_le_bytes());
            let trigger_type_byte: u8 = match order.trigger_type {
                TriggerType::StopLoss => 0,
                TriggerType::TakeProfit => 1,
            };
            hasher.update(&[trigger_type_byte]);
            let condition_byte: u8 = match order.condition {
                TriggerCondition::PriceAbove => 0,
                TriggerCondition::PriceBelow => 1,
            };
            hasher.update(&[condition_byte]);
            hasher.update(order.trigger_price.to_le_bytes());
            hasher.update(order.limit_price.unwrap_or(0).to_le_bytes());
            let status_byte: u8 = match order.status {
                TriggerOrderStatus::Pending => 0,
                TriggerOrderStatus::Triggered => 1,
                TriggerOrderStatus::Cancelled => 2,
                TriggerOrderStatus::Failed => 3,
            };
            hasher.update(&[status_byte]);
            hasher.update(order.timestamp.to_le_bytes());
        }

        // === Oracle prices (sorted by symbol) ===
        if self.oracle.enabled {
            hasher.update(&[1u8]); // Oracle enabled flag
            let mut oracle_prices: Vec<_> = self.oracle.prices.iter().collect();
            oracle_prices.sort_by_key(|(k, _)| *k);
            for (symbol, oracle_price) in oracle_prices {
                hasher.update(symbol.as_bytes());
                hasher.update(oracle_price.price.to_le_bytes());
                hasher.update(oracle_price.timestamp.to_le_bytes());
                hasher.update(oracle_price.source_count.to_le_bytes());
                hasher.update(oracle_price.confidence_bps.to_le_bytes());
            }
        } else {
            hasher.update(&[0u8]); // Oracle disabled flag
        }

        hasher.finalize().into()
    }

    /// Compute the authenticated fixed-supply complete-state root.
    pub fn compute_full_state_root(&self) -> Hash {
        super::full_state_hash::compute(self)
    }

    /// Compute an independent complete component tree for seal verification.
    pub(crate) fn compute_full_state_tree_fresh(&self) -> super::full_state_hash::ComponentTree {
        super::full_state_hash::compute_tree(self)
    }

    /// Derive a candidate tree from a verified parent tree using this state's
    /// transient dirty mask. A missing/unknown/chain-domain-mismatched
    /// baseline falls back to a full recomputation. The tracker is cleared
    /// only after the candidate tree has been materialized.
    pub(crate) fn derive_full_state_tree(
        &mut self,
        parent: Option<&super::full_state_hash::ComponentTree>,
    ) -> super::full_state_hash::ComponentTree {
        let dirty = self.full_state_dirty();
        let tree = match parent {
            Some(parent_tree) => super::full_state_hash::derive_tree(self, parent_tree, dirty),
            _ => super::full_state_hash::compute_tree(self),
        };
        self.clear_full_state_dirty();
        debug_assert_eq!(
            self.full_state_dirty(),
            super::full_state_hash::COMPONENT_DIRTY_NONE
        );
        tree
    }

    /// Create snapshot of current state
    pub fn create_snapshot(&self, height: u64) -> crate::storage::AppSnapshot {
        // Runtime committee/context material is not application state and
        // must not survive even an in-memory snapshot round trip. Persistent
        // serializers also skip these fields, but clearing them here keeps
        // direct snapshot callers honest and makes recovery fail closed until
        // the node reinjects trusted committee configuration.
        let mut staking = (*self.staking).clone();
        staking.consensus_context = None;
        staking.consensus_genesis_hash = [0u8; 32];
        staking.authoritative_committee = None;
        staking.require_authoritative_committee = false;
        crate::storage::AppSnapshot {
            height,
            timestamp: self.timestamp,
            accounts: self.accounts.all_accounts(),
            market_configs: self.configs.values().cloned().collect(),
            mark_prices: self
                .mark_prices
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            insurance_fund: self.insurance_fund,
            funding_rates: self
                .current_funding_rates
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_funding_times: self
                .last_funding_times
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            staking: Some(staking),
            oracle: Some((*self.oracle).clone()),
            trigger_orders: self.trigger_orders.values().cloned().collect(),
            premium_samples: self
                .premium_samples
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().copied().collect()))
                .collect(),
            trigger_seq: self.trigger_seq,
            mark_price_ema: self
                .mark_price_ema
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
        }
    }

    /// Restore state from snapshot (for recovery)
    pub fn from_snapshot(snapshot: crate::storage::AppSnapshot) -> Self {
        Self::from_snapshot_with_chain_domain(snapshot, [0u8; 32], true)
    }

    /// Restore state with the node-configured chain domain and envelope
    /// policy.  This convenience wrapper is retained for trusted fixtures;
    /// production/import code must use the fallible `try_` constructor below.
    pub fn from_snapshot_with_chain_domain(
        snapshot: crate::storage::AppSnapshot,
        chain_domain: [u8; 32],
        allow_dev_envelopes: bool,
    ) -> Self {
        Self::try_from_snapshot_with_chain_domain(snapshot, chain_domain, allow_dev_envelopes)
            .expect("invalid application snapshot")
    }

    /// Restore state from an untrusted snapshot, failing closed on malformed
    /// primary records or duplicate vector entries.
    ///
    /// Snapshots intentionally omit orderbook queues.  The empty orderbooks
    /// created from market configs are therefore rebuilt and validated here,
    /// while canonical exchange recovery must still replay blocks from
    /// genesis to reconstruct open orders.
    pub fn try_from_snapshot_with_chain_domain(
        snapshot: crate::storage::AppSnapshot,
        chain_domain: [u8; 32],
        allow_dev_envelopes: bool,
    ) -> Result<Self, String> {
        use crate::app::oracle::OracleState;
        use std::collections::HashSet;

        // Apply resource limits before taking any snapshot records into
        // runtime maps or rebuilding derived indexes.  This keeps direct
        // import callers fail-closed even when they bypass storage decoding.
        snapshot.validate_resource_limits()?;

        let crate::storage::AppSnapshot {
            timestamp,
            accounts: snapshot_accounts,
            market_configs: snapshot_market_configs,
            mark_prices: snapshot_mark_prices,
            insurance_fund,
            funding_rates: snapshot_funding_rates,
            last_funding_times: snapshot_last_funding_times,
            staking: snapshot_staking,
            oracle,
            trigger_orders: snapshot_trigger_orders,
            premium_samples: snapshot_premium_samples,
            trigger_seq,
            mark_price_ema: snapshot_mark_price_ema,
            ..
        } = snapshot;

        // Vec-backed snapshot fields are primary records.  Never let a
        // duplicate silently overwrite an earlier entry while converting to
        // runtime maps.
        let mark_prices = unique_snapshot_pairs(snapshot_mark_prices, "mark_prices")?;
        let mark_price_ema = unique_snapshot_pairs(snapshot_mark_price_ema, "mark_price_ema")?;
        let funding_rates = unique_snapshot_pairs(snapshot_funding_rates, "funding_rates")?;
        let last_funding_times =
            unique_snapshot_pairs(snapshot_last_funding_times, "last_funding_times")?;
        let premium_samples = CowMap::from_map(
            unique_snapshot_pairs(snapshot_premium_samples, "premium_samples")?
                .into_iter()
                .map(|(symbol, values)| (symbol, values.into_iter().collect()))
                .collect(),
        );

        let mut seen_accounts = HashSet::with_capacity(snapshot_accounts.len());
        let mut accounts = Vec::with_capacity(snapshot_accounts.len());
        for mut account in snapshot_accounts {
            // Account lookups are case-insensitive at execution time, so
            // reject case-folded duplicates and normalize the unique record
            // before handing it to AccountManager.
            let address = account.address.to_lowercase();
            if !seen_accounts.insert(address.clone()) {
                return Err(format!(
                    "snapshot contains duplicate account address: {address}"
                ));
            }
            account.address = address;
            accounts.push(account);
        }

        let mut configs = HashMap::with_capacity(snapshot_market_configs.len());
        for config in snapshot_market_configs {
            let symbol = config.symbol.clone();
            if configs.insert(symbol.clone(), config).is_some() {
                return Err(format!(
                    "snapshot contains duplicate market config: {symbol}"
                ));
            }
        }

        // Restore staking state if present
        let mut staking = snapshot_staking
            .ok_or_else(|| "snapshot is missing mandatory staking state".to_string())?;
        staking.set_consensus_genesis_hash(chain_domain);
        // Runtime committee material is intentionally absent from snapshots.
        // Until the node reinjects the trusted committee, evidence must fail
        // closed even when the snapshot contains registered validators.
        staking.require_authoritative_committee = true;

        // Restore oracle state if present
        let oracle = oracle.unwrap_or_else(OracleState::new);

        // Restore trigger primary records.  Indexes are rebuilt atomically
        // after the complete state has been constructed.
        let mut trigger_orders = HashMap::new();
        let mut seen_trigger_cloids = HashSet::new();
        for order in snapshot_trigger_orders {
            let id = order.id.clone();
            if trigger_orders.insert(id.clone(), order.clone()).is_some() {
                return Err(format!(
                    "snapshot contains duplicate trigger order ID: {id}"
                ));
            }
            if let Some(cloid) = order.cloid {
                let key = (order.trader, order.symbol, cloid);
                if !seen_trigger_cloids.insert(key) {
                    return Err("snapshot contains duplicate trigger client order ID".to_string());
                }
            }
        }

        let mut state = Self {
            chain_domain,
            full_state_dirty: super::full_state_hash::DirtyTracker::all(),
            allow_dev_envelopes,
            speculative_execution: false,
            orderbooks: HashMap::new().into(),
            accounts: AccountManager::from_accounts(accounts).into(),
            mempool: Mempool::default().into(),
            configs: configs.into(),
            mark_prices: mark_prices.into(),
            mark_price_ema: mark_price_ema.into(),
            timestamp,
            pending_fills: Vec::new().into(),
            pending_order_updates: Vec::new().into(),
            trade_history: CowMap::new().into(),
            insurance_fund,
            pending_liquidations: Vec::new().into(),
            premium_samples: premium_samples.into(),
            current_funding_rates: funding_rates.into(),
            last_funding_times: last_funding_times.into(),
            pending_funding: Vec::new().into(),
            pending_deposits: Vec::new().into(),
            candle_manager: CandleManager::new().into(), // Candles are rebuilt from trades
            staking: staking.into(),
            pending_staking_events: Vec::new().into(),
            pending_validator_update: None,
            current_view: 0,
            trigger_orders: CowMap::from_map(trigger_orders).into(),
            trigger_orders_by_trader: CowMap::new().into(),
            trigger_orders_by_symbol: CowMap::new().into(),
            trigger_orders_by_cloid: CowMap::new().into(),
            trigger_seq,
            pending_trigger_events: Vec::new().into(),
            pending_adl_events: Vec::new().into(),
            last_execution_artifacts: None,
            oracle: oracle.into(),
            committed_height: 0, // Will be set by consensus after replay
            prev_day_prices: HashMap::new().into(),
            day_start: 0,
            day_volume: HashMap::new().into(),
            day_notional_volume: HashMap::new().into(),
        };

        // Snapshot orderbooks are intentionally absent; create one empty
        // primary queue per unique market config before rebuilding indexes.
        for symbol in state.configs.keys().cloned().collect::<Vec<_>>() {
            state
                .orderbooks
                .insert(symbol.clone(), OrderBook::new(&symbol));
        }

        state.validate_and_rebuild_derived_indexes()?;
        Ok(state)
    }
}

impl AppHook for AppState {
    fn validate_block(&self, block: &Block) -> Result<(), String> {
        self.validate_consensus_block(block)
    }

    fn submit_user_transaction(
        &mut self,
        envelope: SignedEnvelope,
        timestamp: u64,
    ) -> Result<Hash, String> {
        self.submit_envelope_at(envelope, timestamp)
            .map_err(|error| error.to_string())
    }

    fn take_validator_update(&mut self) -> Option<crate::app::staking::ValidatorSetUpdate> {
        self.pending_validator_update.take()
    }

    fn validator_set_update_for_transition(
        &self,
        finalized_block: &Block,
    ) -> Result<Option<crate::app::staking::ValidatorSetUpdate>, String> {
        finalized_block.validate()?;
        if finalized_block.genesis_hash != self.chain_domain {
            return Err(
                "transition block chain domain does not match application state".to_string(),
            );
        }
        if finalized_block.height != self.committed_height {
            return Err(format!(
                "transition block height {} does not match application head {}",
                finalized_block.height, self.committed_height
            ));
        }
        let expected_next_epoch = finalized_block
            .epoch
            .checked_add(1)
            .ok_or_else(|| "transition next epoch overflows u64".to_string())?;
        if expected_next_epoch != self.current_epoch() {
            return Err(format!(
                "application epoch {} is not finalized transition epoch {} + 1",
                self.current_epoch(),
                finalized_block.epoch,
            ));
        }
        self.validate_consensus_state()?;
        if self.compute_full_state_root() != finalized_block.app_hash {
            return Err("transition block app hash does not match application state".to_string());
        }

        // The update is intentionally transient.  Refuse to stage when the
        // exact result of this finalized transition is no longer available;
        // deriving a fresh set could bind a proof to later application state.
        Ok(self.pending_validator_update.clone())
    }

    fn submit_equivocation_evidence(&mut self, proof: crate::consensus::EquivocationProof) -> bool {
        self.enqueue_equivocation_evidence(proof)
    }

    fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
        // The payload always carries the versioned consensus transaction enum.
        // Unsigned legacy actions are wrapped as the explicit System variant;
        // user API actions are Signed envelopes.
        let txs = self.prepare_consensus_transactions(1000);
        if txs.is_empty() {
            return vec![];
        }
        // Serialize consensus transactions for propagation to followers.
        bincode::serialize(&txs).unwrap_or_default()
    }

    fn execute(&mut self, block: &Block) -> Hash {
        // Never expose artifacts from a previous block when this execution is
        // rejected or when a caller executes a new block before consuming the
        // previous result.
        self.last_execution_artifacts = None;
        if let Err(error) = self.validate_consensus_block(block) {
            tracing::warn!(
                height = block.height,
                error = %error,
                "Rejected application block before execution"
            );
            return crate::types::hash(b"invalid-application-payload");
        }

        // Decode and bound the complete payload before mutating execution
        // state.  Commitment v2 cannot represent more than this many ordered
        // receipts, so an oversized block is deterministically invalid and
        // must fail closed before any transaction is applied.
        let entries = Self::decode_consensus_payload(&block.payload)
            .expect("consensus payload validated immediately before execution");
        if entries.len() > crate::types::MAX_RECEIPTS_PER_COMMITMENT {
            tracing::warn!(
                height = block.height,
                count = entries.len(),
                max = crate::types::MAX_RECEIPTS_PER_COMMITMENT,
                "Rejected block with too many execution receipts"
            );
            self.last_execution_artifacts = None;
            return crate::types::hash(b"invalid-execution-artifacts");
        }

        // Settle the closed reward interval on a private staking clone before
        // mutating any application metadata. An arithmetic or timestamp error
        // therefore rejects the candidate without partially consuming the
        // fixed emissions reserve.
        let reward_before = (
            self.staking.emissions_reserve,
            self.staking.last_reward_accrual_timestamp,
            self.staking.reward_clock_initialized,
            self.staking.reward_accrual_remainder,
            self.staking.last_reward_compound_timestamp,
            self.staking.total_staked,
        );
        let mut reward_staking = self.staking.clone();
        let reward_result = if reward_staking.enabled {
            match reward_staking.accrue_rewards_at(block.timestamp) {
                Ok(rewards) => rewards,
                Err(error) => {
                    tracing::warn!(
                        height = block.height,
                        error = %error,
                        "Rejected block during staking reward settlement"
                    );
                    return crate::types::hash(b"invalid-staking-reward-settlement");
                }
            }
        } else {
            RewardAccrualResult::default()
        };
        let validator_rewards = reward_result.validator_rewards.clone();
        let reward_after = (
            reward_staking.emissions_reserve,
            reward_staking.last_reward_accrual_timestamp,
            reward_staking.reward_clock_initialized,
            reward_staking.reward_accrual_remainder,
            reward_staking.last_reward_compound_timestamp,
            reward_staking.total_staked,
        );
        let reward_state_changed =
            reward_before != reward_after || !reward_result.credits.is_empty();
        let epochs_processed = if reward_before.2 {
            reward_after.1.saturating_sub(reward_before.1)
                / crate::app::staking::STAKING_REWARD_EPOCH_MS
        } else {
            0
        };
        let distributed = reward_before.0.saturating_sub(reward_after.0);
        let auto_compounded = reward_result.auto_compounded;
        let reward_event =
            (epochs_processed > 0 || auto_compounded > 0).then(|| StakingRewardEpochInfo {
                schema_version: STAKING_REWARD_EVENT_SCHEMA_VERSION,
                accrual_timestamp: reward_after.1,
                epochs_processed,
                total_distributed: distributed,
                emissions_reserve_remaining: reward_after.0,
                auto_compounded,
                validator_rewards,
                credits: reward_result.credits.clone(),
                compoundings: reward_result.compoundings.clone(),
            });

        self.timestamp = block.timestamp;
        self.current_view = block.view;
        self.committed_height = block.height;
        self.mark_full_state_dirty(super::full_state_hash::COMPONENT_DIRTY_METADATA);
        if reward_state_changed {
            self.staking = reward_staking;
            self.mark_full_state_dirty(super::full_state_hash::COMPONENT_DIRTY_STAKING);
        }

        // Speculative candidates carry an old view of the API mempool. Never
        // mutate it here: promotion reconciles the finalized hashes against
        // the current canonical pool so post-proposal submissions survive.
        if !self.speculative_execution {
            self.mempool.prune_stale(block.timestamp);
        }

        // Clear pending events from previous block
        self.pending_fills.clear();
        self.pending_order_updates.clear();
        self.pending_liquidations.clear();
        self.pending_funding.clear();
        self.pending_deposits.clear();
        self.pending_staking_events.clear();
        self.pending_trigger_events.clear();
        self.pending_adl_events.clear();

        // === Staking: Epoch Transition ===
        if self.staking.enabled
            && !self.staking.requires_authoritative_committee()
            && self.staking.should_transition_epoch(block.view)
        {
            self.mark_full_state_dirty(
                super::full_state_hash::COMPONENT_DIRTY_ACCOUNTS
                    | super::full_state_hash::COMPONENT_DIRTY_STAKING,
            );
            let result = self.staking.transition_epoch(block.view, block.timestamp);
            tracing::info!(
                epoch = result.epoch,
                active_validators = result.new_active_set.len(),
                jailed = result.jailed.len(),
                "Epoch transition"
            );

            // Store validator set update for consensus layer to consume
            if let Some(update) = result.validator_set_update {
                tracing::debug!(
                    validators = update.len(),
                    "Storing validator set update for consensus"
                );
                self.pending_validator_update = Some(update);
            }
        }

        // HYCK has a fixed genesis supply in this showcase tranche. There is
        // no per-block issuance; future rewards must spend an explicitly
        // funded reserve instead of minting here.

        // Decode only the versioned consensus transaction enum. Validation at
        // the top of this method guarantees this cannot silently become an
        // empty transaction list.
        let tx_hashes: Vec<crate::types::Hash> = entries
            .iter()
            .filter_map(|entry| entry.hash().ok())
            .collect();

        // Execute every entry in block order.  Signed envelopes are validated
        // against this block timestamp on every validator.  Build transient
        // receipts from the exact canonical entry bytes while preserving the
        // existing action and pending-queue semantics.
        let mut artifacts =
            BlockExecutionArtifacts::new(block.height, block.hash(), block.timestamp);
        for (tx_index, entry) in entries.into_iter().enumerate() {
            let tx_index = tx_index as u32;
            let payload_entry_bytes = bincode::serialize(&entry)
                .expect("validated consensus transaction must have canonical bytes");
            let canonical_bytes = match &entry {
                ConsensusTransaction::Signed(envelope) => envelope
                    .encoded_bytes()
                    .expect("validated signed envelope must have canonical bytes"),
                ConsensusTransaction::System(transaction) => bincode::serialize(transaction)
                    .expect("validated system transaction must have canonical bytes"),
            };
            let entry_for_artifact = entry.clone();
            let cursor = PendingEventCursor::capture(self);
            match self.execute_consensus_transaction(entry, block.timestamp) {
                Ok(fills) => {
                    self.pending_fills.extend(fills.clone());
                    let artifact = match self.transaction_artifact(
                        tx_index,
                        &entry_for_artifact,
                        canonical_bytes,
                        payload_entry_bytes,
                        cursor,
                        &fills,
                        ReceiptStatus::SUCCESS,
                        None,
                    ) {
                        Ok(artifact) => artifact,
                        Err(error) => {
                            tracing::warn!(
                                height = block.height,
                                tx_index,
                                error = %error,
                                "Rejected block with invalid execution artifact"
                            );
                            self.last_execution_artifacts = None;
                            return crate::types::hash(b"invalid-execution-artifacts");
                        }
                    };
                    artifacts.transactions.push(artifact);
                }
                Err(error) => {
                    tracing::warn!(error = %error, "Consensus transaction failed");
                    let artifact = match self.transaction_artifact(
                        tx_index,
                        &entry_for_artifact,
                        canonical_bytes,
                        payload_entry_bytes,
                        cursor,
                        &[],
                        ReceiptStatus::FAILURE,
                        Some(&error),
                    ) {
                        Ok(artifact) => artifact,
                        Err(error) => {
                            tracing::warn!(
                                height = block.height,
                                tx_index,
                                error = %error,
                                "Rejected block with invalid execution artifact"
                            );
                            self.last_execution_artifacts = None;
                            return crate::types::hash(b"invalid-execution-artifacts");
                        }
                    };
                    artifacts.transactions.push(artifact);
                }
            }
        }

        // Capture the direct transaction prefixes before block/system phases
        // append their own events.
        let direct_order_updates = self.pending_order_updates.len();
        let direct_fills = self.pending_fills.len();
        let direct_triggers = self.pending_trigger_events.len();

        // Commit the proposal (two-phase: finalize removal from mempool)
        // This replaces the old drain_block() approach
        // Use unchecked commit since in execute() we're committing the block,
        // view checking happens in the consensus layer
        if !self.speculative_execution && !tx_hashes.is_empty() {
            self.mempool.commit_proposal_unchecked(&tx_hashes);
        }

        // Check and execute liquidations after all transactions (with circuit breaker)
        let max_liquidations = crate::config::Config::global().max_liquidations_per_block;
        let liquidation_batch = crate::app::liquidation::check_and_liquidate_limited(
            &mut self.accounts,
            &self.mark_prices,
            max_liquidations,
        );
        self.pending_liquidations.replace(liquidation_batch.results);

        // Process liquidation results with ADL if needed
        self.process_liquidations_with_adl();

        // === Funding Rate Logic ===
        self.process_funding();

        // === Trigger Order Processing ===
        // Check and execute trigger orders after all transactions are processed
        let trigger_fills = self.process_triggers();
        self.pending_fills.extend(trigger_fills);

        artifacts.block_events = match self.block_execution_events(
            reward_event.as_ref(),
            direct_order_updates,
            direct_fills,
            direct_triggers,
        ) {
            Ok(events) => events,
            Err(error) => {
                tracing::warn!(
                    height = block.height,
                    error = %error,
                    "Rejected block with invalid block execution events"
                );
                self.last_execution_artifacts = None;
                return crate::types::hash(b"invalid-execution-artifacts");
            }
        };
        self.last_execution_artifacts = Some(Arc::new(artifacts));

        // Return state hash for Byzantine detection
        self.compute_state_hash()
    }

    fn derive_execution_commitment(&self, block: &Block) -> Result<Option<CommitmentV2>, String> {
        let artifacts = self
            .execution_artifacts()
            .ok_or_else(|| "execution produced no commitment artifact".to_string())?;
        if artifacts.height != block.height || artifacts.timestamp != block.timestamp {
            return Err("execution artifact metadata does not match block".to_string());
        }

        let mut pre_execution = block.clone();
        pre_execution.app_hash = [0u8; 32];
        pre_execution.commitment_root = [0u8; 32];
        if artifacts.block_hash != block.hash() && artifacts.block_hash != pre_execution.hash() {
            return Err("execution artifact is bound to a different block".to_string());
        }

        artifacts
            .commitment_with_block_events()
            .map(Some)
            .map_err(|error| format!("invalid execution commitment: {error}"))
    }

    fn preflight_commitment(&self, block: &Block) -> Result<Option<CommitmentV2>, String> {
        let commitment = self
            .derive_execution_commitment(block)?
            .ok_or_else(|| "execution produced no commitment artifact".to_string())?;
        let root = commitment
            .root()
            .map_err(|error| format!("invalid execution commitment root: {error}"))?;
        if root != block.commitment_root {
            return Err(format!(
                "execution commitment root mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.commitment_root),
                hex::encode(root)
            ));
        }
        Ok(Some(commitment))
    }

    fn preflight_state_root(&self, block: &Block) -> Result<Option<Hash>, String> {
        let root = self.compute_full_state_root();
        if root != block.app_hash {
            return Err(format!(
                "authenticated state-root mismatch at height {}: expected {}, got {}",
                block.height,
                hex::encode(block.app_hash),
                hex::encode(root)
            ));
        }
        Ok(Some(root))
    }
}

impl AppState {
    /// Process liquidation results with ADL (auto-deleveraging) if needed
    ///
    /// For each liquidation:
    /// 1. Calculate total loss (position PnL + underwater amount)
    /// 2. If loss, check if ADL is needed (insurance fund insufficient)
    /// 3. ADL absorbs losses from profitable counter-parties
    /// 4. Remaining loss goes to insurance fund
    /// 5. Remaining balance from liquidated account goes to insurance fund
    fn process_liquidations_with_adl(&mut self) {
        // Take ownership of pending_liquidations to avoid borrow issues
        let liquidations = std::mem::take(&mut *self.pending_liquidations);
        if !liquidations.is_empty() {
            self.mark_full_state_dirty(
                super::full_state_hash::COMPONENT_DIRTY_ACCOUNTS
                    | super::full_state_hash::COMPONENT_DIRTY_FUNDING,
            );
        }

        for liq in &liquidations {
            // Calculate total loss for ADL consideration:
            // - Position PnL (negative = loss)
            // - Negative insurance_fund_delta means account went underwater
            // Both should be considered for ADL since they represent losses to cover
            let underwater_loss = if liq.insurance_fund_delta < 0 {
                liq.insurance_fund_delta
            } else {
                0
            };
            let total_loss = liq.pnl.saturating_add(underwater_loss);

            if total_loss < 0 {
                // Loss - check if ADL is needed
                let mark_price = self.mark_prices.get(&liq.symbol).copied().unwrap_or(0);

                if let Some(adl_summary) = crate::app::adl::process_adl_if_needed(
                    &mut self.accounts,
                    &liq.symbol,
                    total_loss, // Include underwater amount in ADL calculation
                    self.insurance_fund,
                    mark_price,
                    liq.was_long,
                    &liq.address,
                    self.timestamp,
                ) {
                    // ADL absorbed (part of) the loss
                    // Insurance fund takes remaining loss after ADL
                    let remaining_loss = total_loss.saturating_add(adl_summary.total_absorbed);
                    self.insurance_fund = self.insurance_fund.saturating_add(remaining_loss);
                    self.pending_adl_events.extend(adl_summary.events);
                } else {
                    // No ADL needed - insurance fund takes the total loss
                    self.insurance_fund = self.insurance_fund.saturating_add(total_loss);
                }

                // Positive remaining balance still goes to insurance fund
                if liq.insurance_fund_delta > 0 {
                    self.insurance_fund =
                        self.insurance_fund.saturating_add(liq.insurance_fund_delta);
                }
            } else {
                // No position loss - handle insurance_fund_delta
                // Positive = remaining balance goes to insurance fund
                // Negative = should not happen in profit case, but handle defensively
                if liq.insurance_fund_delta != 0 {
                    self.insurance_fund =
                        self.insurance_fund.saturating_add(liq.insurance_fund_delta);
                }
                // Profit from position also goes to insurance fund
                if liq.pnl > 0 {
                    self.insurance_fund = self.insurance_fund.saturating_add(liq.pnl);
                }
            }
        }

        // Restore the liquidations for event emission
        self.pending_liquidations.replace(liquidations);

        // CRITICAL-5: Ensure insurance fund never goes negative after ADL processing.
        // If ADL couldn't fully cover losses, cap fund at zero to prevent negative balance.
        // A negative fund would cause incorrect accounting in subsequent liquidations.
        if self.insurance_fund < 0 {
            tracing::warn!(
                fund = self.insurance_fund,
                "Insurance fund went negative after liquidations, flooring at zero"
            );
            self.insurance_fund = 0;
        }

        // CRITICAL-5: Warn when fund drops below warning threshold
        if self.insurance_fund < super::INSURANCE_FUND_WARNING_THRESHOLD && self.insurance_fund > 0
        {
            tracing::warn!(
                fund = self.insurance_fund,
                threshold = super::INSURANCE_FUND_WARNING_THRESHOLD,
                "Insurance fund below warning threshold"
            );
        }
    }

    /// Process funding rate sampling and application
    fn process_funding(&mut self) {
        // Collect symbols to process (avoid borrow issues)
        let mut symbols: Vec<(Symbol, MarketConfig)> = self
            .configs
            .iter()
            .map(|(s, c)| (s.clone(), c.clone()))
            .collect();
        // Configs are stored in a HashMap; funding events and any resulting
        // state mutations must follow canonical symbol order.
        symbols.sort_by(|a, b| a.0.cmp(&b.0));

        for (symbol, config) in symbols {
            // Get index price (oracle with mark price fallback)
            let index_price = self.index_price(&symbol).unwrap_or(0);
            if index_price == 0 {
                continue;
            }

            // Sample premium from orderbook
            if let Some(book) = self.orderbooks.get(&symbol) {
                let premium = crate::app::funding::sample_premium(book, index_price);
                self.premium_samples
                    .entry(symbol.clone())
                    .or_default()
                    .push_back(premium);
                self.mark_full_state_dirty(super::full_state_hash::COMPONENT_DIRTY_FUNDING);

                // Keep only samples for the funding interval (~1 hour of blocks)
                // At 100ms blocks, 1 hour = 36000 blocks
                let max_samples = (config.funding_interval_ms / 100).max(1) as usize;
                let samples = self.premium_samples.get_mut(&symbol).unwrap();
                while samples.len() > max_samples {
                    samples.pop_front();
                }
            }

            // Check if funding interval has elapsed
            let last_funding = self.last_funding_times.get(&symbol).copied().unwrap_or(0);
            if self.timestamp >= last_funding + config.funding_interval_ms {
                // Calculate average premium
                let samples: Vec<i64> = self
                    .premium_samples
                    .get(&symbol)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                let avg_premium = crate::app::funding::average_premium(&samples);

                // Calculate funding rate
                let funding_rate = crate::app::funding::calculate_funding_rate(
                    avg_premium,
                    config.interest_rate_bps,
                    config.max_funding_rate_bps,
                );

                // Apply funding to all positions
                let index_price = self.index_price(&symbol).unwrap_or(0);
                let result = crate::app::funding::apply_funding(
                    &mut self.accounts,
                    &symbol,
                    funding_rate,
                    index_price,
                    self.timestamp,
                );

                // Update state
                self.current_funding_rates
                    .insert(symbol.clone(), funding_rate);
                self.last_funding_times
                    .insert(symbol.clone(), self.timestamp);
                self.pending_funding.push(result);
                self.mark_full_state_dirty(
                    super::full_state_hash::COMPONENT_DIRTY_ACCOUNTS
                        | super::full_state_hash::COMPONENT_DIRTY_FUNDING,
                );

                // Clear premium samples for next period
                self.premium_samples.remove(&symbol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        orderbook::Side, ConsensusTransaction, OrderType, StaticValidatorBootstrap, Transaction,
    };
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::{
        Block, Certificate, CommitmentV2, ConsensusConfig, ConsensusContext, EventRecord, EventType,
    };

    fn curated_app_fixture() -> (AppState, crate::types::Committee, BlsSecretKey, [u8; 32]) {
        let node_id = [1u8; 32];
        let mut seed = [0u8; 32];
        seed[0] = 1;
        let secret = BlsSecretKey::from_seed(&seed);
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3_000,
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            bls_secret_key: None,
        };
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        state.set_consensus_context(context);
        state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(node_id)),
                    node_id,
                    voting_power: 1,
                    bls_pubkey: secret.public_key().to_bytes().to_vec(),
                    bls_proof_of_possession: secret
                        .create_proof_of_possession(&context.genesis_hash, &node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: crate::app::staking::MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
            )
            .unwrap();
        (state, committee, secret, node_id)
    }

    fn evidence_for_app_state(
        state: &AppState,
        secret: &BlsSecretKey,
        offender: [u8; 32],
    ) -> crate::app::staking::Evidence {
        let context = state.staking().static_consensus_context().unwrap();
        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];
        let app_hash_a = [3u8; 32];
        let app_hash_b = [4u8; 32];
        let signature_a = secret
            .sign(&Certificate::build_signing_message(
                context,
                7,
                &hash_a,
                &app_hash_a,
            ))
            .to_bytes()
            .to_vec();
        let signature_b = secret
            .sign(&Certificate::build_signing_message(
                context,
                7,
                &hash_b,
                &app_hash_b,
            ))
            .to_bytes()
            .to_vec();
        crate::app::staking::Evidence {
            evidence_type: crate::app::staking::EvidenceType::DoubleVote,
            offender,
            view: 7,
            timestamp: 0,
            context,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a,
            signature_b,
        }
    }

    #[test]
    fn fresh_curated_app_startup_has_slashable_members() {
        let (mut state, committee, _, node_id) = curated_app_fixture();
        state.bind_authoritative_committee(committee).unwrap();
        let validator = state
            .staking()
            .get_validator_by_node(&node_id)
            .expect("curated member must be bootstrapped");
        assert_eq!(validator.self_stake, crate::app::staking::MIN_SELF_STAKE);
        assert_eq!(
            validator.status,
            crate::app::staking::ValidatorStatus::Active
        );
    }

    #[test]
    fn bound_static_committee_does_not_emit_epoch_update_at_boundary() {
        let (mut state, committee, _, _) = curated_app_fixture();
        let context = state.staking().static_consensus_context().unwrap();
        state.bind_authoritative_committee(committee).unwrap();

        let mut block = Block::genesis(context);
        block.height = 1;
        block.view = crate::app::staking::ROUNDS_PER_EPOCH;
        block.timestamp = 1;
        block.commitment_root = [1u8; 32];

        <AppState as crate::consensus::AppHook>::execute(&mut state, &block);

        assert_eq!(state.current_epoch(), 0);
        assert!(state.pending_validator_update().is_none());
    }

    #[test]
    fn static_committee_rewards_spend_reserve_and_emit_authenticated_epoch_event() {
        let (mut state, committee, _, node_id) = curated_app_fixture();
        let context = state.staking().static_consensus_context().unwrap();
        state.bind_authoritative_committee(committee).unwrap();
        let reserve_before = state.staking().emissions_reserve;

        let empty_payload = bincode::serialize(&Vec::<ConsensusTransaction>::new()).unwrap();
        let mut parent = Block::genesis(context);
        let anchor = 1_700_000_000_000u64;
        for height in 1..=181 {
            let block = Block {
                epoch: context.epoch,
                committee_hash: context.committee_hash,
                genesis_hash: context.genesis_hash,
                view: height,
                height,
                parent: parent.hash(),
                payload: empty_payload.clone(),
                proposer: node_id,
                commitment_root: [1u8; 32],
                app_hash: [0u8; 32],
                timestamp: anchor + (height - 1) * 30_000,
                justify: None,
            };
            <AppState as crate::consensus::AppHook>::execute(&mut state, &block);
            parent = block;
        }

        let operator = format!("system:genesis:{}", hex::encode(node_id));
        let pending = state.staking().validator_pending_rewards(&operator);
        assert!(pending > 0);
        assert_eq!(reserve_before - state.staking().emissions_reserve, pending);
        state.validate_hyck_supply().unwrap();

        let artifacts = state.take_execution_artifacts().unwrap();
        let reward_event = artifacts
            .block_events
            .iter()
            .find(|event| event.event_type == EventType::EPOCH)
            .expect("reward settlement must be indexer-visible");
        let info: StakingRewardEpochInfo = bincode::deserialize(&reward_event.payload).unwrap();
        assert_eq!(
            info.schema_version,
            crate::app::staking::rewards::STAKING_REWARD_EVENT_SCHEMA_VERSION
        );
        assert_eq!(info.epochs_processed, 1);
        assert_eq!(info.total_distributed, pending);
        assert_eq!(info.emissions_reserve_remaining, reserve_before - pending);
        assert_eq!(
            info.credits.iter().map(|credit| credit.net).sum::<i64>(),
            pending
        );
        assert!(info.compoundings.is_empty());
    }

    #[test]
    fn runtime_committee_binding_does_not_change_state_root() {
        let (mut state, committee, _, _) = curated_app_fixture();
        let before = state.compute_full_state_root();
        state.bind_authoritative_committee(committee).unwrap();
        assert_eq!(state.compute_full_state_root(), before);
    }

    #[test]
    fn snapshot_restore_rejects_evidence_until_committee_is_reinjected() {
        let (mut state, committee, secret, node_id) = curated_app_fixture();
        state
            .bind_authoritative_committee(committee.clone())
            .unwrap();
        let evidence = evidence_for_app_state(&state, &secret, node_id);
        let snapshot = state.create_snapshot(0);
        let mut restored =
            AppState::try_from_snapshot_with_chain_domain(snapshot, [9u8; 32], true).unwrap();
        assert!(restored.staking().authoritative_committee().is_none());
        assert!(restored.staking().static_consensus_context().is_none());
        assert!(restored
            .staking_mut()
            .submit_evidence(evidence.clone())
            .is_err());
        assert!(restored.staking().static_committee_binding_pending());
        let rotation_error = restored.staking_mut().rotate_validator_key(
            &format!("system:genesis:{}", hex::encode(node_id)),
            Vec::new(),
            Vec::new(),
            [9u8; 32],
        );
        assert!(matches!(
            rotation_error,
            Err(crate::app::staking::StakingError::StaticCommitteeKeyRotationDisabled)
        ));
        let mut boundary_block = Block::genesis(ConsensusContext::with_genesis(
            0,
            committee.hash(),
            [9u8; 32],
        ));
        boundary_block.height = 1;
        boundary_block.view = crate::app::staking::ROUNDS_PER_EPOCH;
        assert!(restored
            .validate_consensus_block(&boundary_block)
            .expect_err("restored snapshots must not execute before committee rebinding")
            .contains("committee must be rebound"));
        assert_eq!(restored.current_epoch(), 0);
        restored.set_consensus_context(ConsensusContext::with_genesis(
            0,
            committee.hash(),
            [9u8; 32],
        ));
        restored.bind_authoritative_committee(committee).unwrap();
        restored.staking_mut().submit_evidence(evidence).unwrap();
    }

    #[test]
    fn consensus_block_rejects_another_application_chain_domain() {
        let state = AppState::new_with_chain_domain([1u8; 32]);
        let block = Block::genesis(ConsensusContext::with_genesis(0, [7u8; 32], [2u8; 32]));

        assert!(state
            .validate_consensus_block(&block)
            .expect_err("cross-domain block must fail closed")
            .contains("does not match application chain domain"));
    }

    #[test]
    fn proposer_orders_same_signer_nonces_across_buckets_and_defers_gaps() {
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let parent = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let address = format!("{:?}", signer.address());

        // Insert nonce 1 first and put it in the higher-priority bucket. The
        // nonce-0 order arrives later in bucket 2; proposer scheduling must
        // still emit nonce 0 before nonce 1.
        let nonce_one = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            1,
            0,
            100,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 2,
            },
        )
        .unwrap();
        let nonce_zero = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::PlaceOrder {
                trader: address.clone(),
                symbol: "BTC-USDT".to_string(),
                side: Side::Bid,
                price: 5_000_000,
                size: 1,
                order_type: OrderType::Gtc,
                reduce_only: false,
            },
        )
        .unwrap();

        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        state.mempool.add_envelope(nonce_one, 0).unwrap();
        state.mempool.add_envelope(nonce_zero, 0).unwrap();
        let payload = <AppState as crate::consensus::AppHook>::prepare_payload(&state, &parent);
        let entries = AppState::decode_consensus_payload(&payload).unwrap();
        let payload_again =
            <AppState as crate::consensus::AppHook>::prepare_payload(&state, &parent);
        assert_eq!(payload, payload_again);
        let same_signer_nonces: Vec<_> = entries
            .iter()
            .filter_map(|entry| match entry {
                ConsensusTransaction::Signed(envelope)
                    if envelope.signer_address()
                        == format!("0x{}", hex::encode(signer.address())) =>
                {
                    Some(envelope.nonce)
                }
                _ => None,
            })
            .collect();
        assert_eq!(same_signer_nonces, vec![0, 1]);

        // A missing predecessor must not block another signer's ready
        // transaction, nor should the gap transaction itself be proposed.
        let blocked_signer = crate::crypto::Signer::generate();
        let blocked_address = format!("{:?}", blocked_signer.address());
        let blocked_nonce_one = SignedEnvelope::sign(
            context.genesis_hash,
            &blocked_signer,
            1,
            0,
            100,
            Transaction::Deposit {
                trader: blocked_address,
                amount: 1,
            },
        )
        .unwrap();
        let ready_signer = crate::crypto::Signer::generate();
        let ready_address = format!("{:?}", ready_signer.address());
        let ready_nonce_zero = SignedEnvelope::sign(
            context.genesis_hash,
            &ready_signer,
            0,
            0,
            100,
            Transaction::PlaceOrder {
                trader: ready_address,
                symbol: "BTC-USDT".to_string(),
                side: Side::Bid,
                price: 5_000_000,
                size: 1,
                order_type: OrderType::Gtc,
                reduce_only: false,
            },
        )
        .unwrap();
        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        state.mempool.add_envelope(blocked_nonce_one, 0).unwrap();
        state.mempool.add_envelope(ready_nonce_zero, 0).unwrap();
        let payload = <AppState as crate::consensus::AppHook>::prepare_payload(&state, &parent);
        let entries = AppState::decode_consensus_payload(&payload).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(
            &entries[0],
            ConsensusTransaction::Signed(envelope) if envelope.nonce == 0
        ));
    }

    #[test]
    fn consensus_validation_rejects_gap_duplicate_and_exhausted_nonces() {
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let parent = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let address = format!("{:?}", signer.address());
        let envelope = |nonce, amount| {
            SignedEnvelope::sign(
                context.genesis_hash,
                &signer,
                nonce,
                0,
                100,
                Transaction::Deposit {
                    trader: address.clone(),
                    amount,
                },
            )
            .unwrap()
        };

        let mut block = Block {
            payload: bincode::serialize(&vec![ConsensusTransaction::Signed(envelope(1, 1))])
                .unwrap(),
            ..parent.clone()
        };
        block.height = 1;
        block.view = 1;
        block.parent = parent.hash();
        block.timestamp = 1;
        let state = AppState::new_with_chain_domain(context.genesis_hash);
        assert!(state
            .validate_consensus_block(&block)
            .expect_err("gap transaction must be block-invalid")
            .contains("expected 0, got 1"));

        let duplicate_payload = bincode::serialize(&vec![
            ConsensusTransaction::Signed(envelope(0, 1)),
            ConsensusTransaction::Signed(envelope(0, 2)),
        ])
        .unwrap();
        block.payload = duplicate_payload;
        assert!(state
            .validate_consensus_block(&block)
            .expect_err("duplicate nonce must be block-invalid")
            .contains("expected 1, got 0"));

        let max_address = format!("{:?}", signer.address());
        let mut exhausted_state = AppState::new_with_chain_domain(context.genesis_hash);
        exhausted_state
            .accounts_mut()
            .get_or_create(&max_address)
            .nonce = u64::MAX;
        let exhausted = envelope(u64::MAX, 1);
        block.payload = bincode::serialize(&vec![ConsensusTransaction::Signed(exhausted)]).unwrap();
        assert!(exhausted_state
            .validate_consensus_block(&block)
            .expect_err("exhausted nonce must be block-invalid")
            .contains("nonce"));
    }

    #[test]
    fn excessive_timestamp_step_rejects_without_consuming_reward_state() {
        let context = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let mut state = AppState::new_with_chain_domain(context.genesis_hash);
        let mut first = Block::genesis(context);
        first.height = 1;
        first.timestamp = 1_700_000_000_000;
        first.commitment_root = [1u8; 32];
        <AppState as AppHook>::execute(&mut state, &first);

        let before_root = state.compute_full_state_root();
        let before_reserve = state.staking.emissions_reserve;
        let before_timestamp = state.timestamp;
        let mut invalid = first.clone();
        invalid.height = 2;
        invalid.parent = first.hash();
        invalid.timestamp = first.timestamp + crate::types::MAX_BLOCK_TIMESTAMP_STEP_MS + 1;

        assert_eq!(
            <AppState as AppHook>::execute(&mut state, &invalid),
            crate::types::hash(b"invalid-application-payload")
        );
        assert_eq!(state.timestamp, before_timestamp);
        assert_eq!(state.staking.emissions_reserve, before_reserve);
        assert_eq!(state.compute_full_state_root(), before_root);
        assert!(state.take_execution_artifacts().is_none());
    }

    #[test]
    fn test_deposit_and_order() {
        let mut state = AppState::new();

        // Deposit
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000, // $1M in cents
            })
            .unwrap();

        assert_eq!(state.account("alice").unwrap().balance, 100_000_000);

        // Place order
        let fills = state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,  // $50,000
                size: 100_000_000, // 1 BTC
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        assert!(fills.is_empty()); // No counterparty
        assert!(state.orderbook("BTC-USDT").unwrap().best_bid().is_some());
    }

    #[test]
    fn test_matching() {
        let mut state = AppState::new();

        // Alice deposits and bids
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // Bob deposits and asks (should match)
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            })
            .unwrap();

        let fills = state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 4_900_000, // Below bid
                size: 50_000_000, // 0.5 BTC
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50_000_000);
        assert_eq!(fills[0].price, 5_000_000); // At bid price

        // Check positions
        let alice_pos = state.account("alice").unwrap().position("BTC-USDT");
        assert_eq!(alice_pos.size, 50_000_000); // Long 0.5 BTC

        let bob_pos = state.account("bob").unwrap().position("BTC-USDT");
        assert_eq!(bob_pos.size, -50_000_000); // Short 0.5 BTC
    }

    #[test]
    fn test_state_hash_deterministic() {
        let mut state = AppState::new();

        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        let hash1 = state.compute_state_hash();
        let hash2 = state.compute_state_hash();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn oversized_commitment_payload_fails_closed_without_mutating_state() {
        let mut state = AppState::new();
        let context = ConsensusContext::new(0, [0u8; 32]);
        let entries = (0..=crate::types::MAX_RECEIPTS_PER_COMMITMENT)
            .map(|index| {
                ConsensusTransaction::System(Transaction::Deposit {
                    trader: format!("oversized-{index}"),
                    amount: 1,
                })
            })
            .collect::<Vec<_>>();
        let mut block = Block::genesis(context);
        block.height = 1;
        block.view = 1;
        block.timestamp = 1;
        block.payload = bincode::serialize(&entries).expect("payload encodes");

        let result = <AppState as AppHook>::execute(&mut state, &block);
        assert_eq!(result, crate::types::hash(b"invalid-execution-artifacts"));
        assert!(state.take_execution_artifacts().is_none());
        assert_eq!(state.committed_height(), 0);
        assert!(state.account("oversized-0").is_none());
    }

    #[test]
    fn funding_state_and_artifact_roots_ignore_market_insertion_order() {
        fn state_with_markets(order: &[&str]) -> AppState {
            let mut state = AppState::new();
            for symbol in order {
                state.add_market(MarketConfig {
                    symbol: (*symbol).to_string(),
                    ..MarketConfig::default()
                });
            }
            state.timestamp = 3_600_000;
            state
        }

        fn funding_root(state: &AppState) -> [u8; 32] {
            let events = state
                .pending_funding
                .iter()
                .enumerate()
                .map(|(index, result)| {
                    EventRecord::from_bincode(index as u32, EventType::FUNDING, result)
                        .expect("funding payload encodes")
                })
                .collect();
            CommitmentV2::new_with_system_events(Vec::new(), events)
                .expect("commitment validates")
                .root()
                .expect("commitment root computes")
        }

        let mut first = state_with_markets(&["ETH-USDT", "SOL-USDT"]);
        let mut second = state_with_markets(&["SOL-USDT", "ETH-USDT"]);
        first.process_funding();
        second.process_funding();

        assert_eq!(
            first
                .pending_funding
                .iter()
                .map(|result| result.symbol.as_str())
                .collect::<Vec<_>>(),
            vec!["BTC-USDT", "ETH-USDT", "SOL-USDT"]
        );
        assert_eq!(funding_root(&first), funding_root(&second));
        assert_eq!(first.compute_state_hash(), second.compute_state_hash());
    }

    #[test]
    fn empty_payload_execution_ignores_local_mempool() {
        let mut state_a = AppState::new();
        let mut state_b = AppState::new();

        state_a
            .submit_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();
        state_b
            .submit_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 200_000_000,
            })
            .unwrap();

        let block = Block::genesis(ConsensusContext::new(0, [7u8; 32]));
        let hash_a = <AppState as crate::consensus::AppHook>::execute(&mut state_a, &block);
        let hash_b = <AppState as crate::consensus::AppHook>::execute(&mut state_b, &block);

        assert_eq!(hash_a, hash_b);
        assert_eq!(state_a.mempool_stats(), (1, 0, 0));
        assert_eq!(state_b.mempool_stats(), (1, 0, 0));
        assert!(state_a.account("alice").is_none());
        assert!(state_b.account("bob").is_none());
    }

    #[test]
    fn malformed_payload_does_not_execute_or_drain_local_mempool() {
        let mut state = AppState::new();
        state
            .submit_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        let before = state.mempool_stats();
        let mut block = Block::genesis(ConsensusContext::new(0, [7u8; 32]));
        block.payload = vec![0xff, 0x00, 0x01];

        <AppState as crate::consensus::AppHook>::execute(&mut state, &block);

        assert_eq!(state.mempool_stats(), before);
        assert!(state.account("alice").is_none());
    }

    #[test]
    fn test_mark_price_ema() {
        let mut state = AppState::new();

        // Deposit for both traders
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            })
            .unwrap();

        // Initial mark price is $50,000 (5_000_000 cents)
        let initial = state.mark_price("BTC-USDT").unwrap();
        assert_eq!(initial, 5_000_000);

        // Trade at $60,000 - with 1% EMA, mark should move only ~1% toward $60k
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 6_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 6_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        let mark = state.mark_price("BTC-USDT").unwrap();
        // EMA with 1% alpha: new = 0.01 * 6_000_000 + 0.99 * 5_000_000 = 5_010_000
        // Mark should be close to 5_010_000 (or blended with oracle)
        assert!(
            mark < 5_500_000,
            "EMA should dampen sudden price moves, got {}",
            mark
        );
        assert!(
            mark > 4_500_000,
            "Mark should still move toward trade price, got {}",
            mark
        );
    }

    #[test]
    fn test_trade_history() {
        let mut state = AppState::new();

        // Setup: Alice bids, Bob asks -> should match and create trade
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            })
            .unwrap();

        // Alice places bid
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // No trades yet
        assert!(state.get_trades("BTC-USDT", 10).is_empty());

        // Bob places ask (matches Alice's bid)
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 4_900_000,
                size: 50_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // Now we should have 1 trade
        let trades = state.get_trades("BTC-USDT", 10);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 5_000_000);
        assert_eq!(trades[0].size, 50_000_000);

        // Unknown symbol returns empty
        assert!(state.get_trades("ETH-USDT", 10).is_empty());
    }

    #[test]
    fn test_collateral_lock_on_rest() {
        let mut state = AppState::new();
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        // Place GTC order that rests
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        let account = state.account("alice").unwrap();
        // locked should be notional/10 = (1 BTC * $50k) / 10 = $5,000
        assert!(
            account.locked > 0,
            "Collateral should be locked for resting order"
        );
        assert!(
            account.balance < 100_000_000,
            "Balance should decrease by locked amount"
        );
    }

    #[test]
    fn test_collateral_unlock_on_cancel() {
        let mut state = AppState::new();
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        let locked_before = state.account("alice").unwrap().locked;
        assert!(locked_before > 0);

        // Get order id from the orderbook
        let orders = state.orders_by_address("alice");
        let order_id = orders[0].id.clone();

        state
            .execute_tx(Transaction::CancelOrder {
                trader: "alice".into(),
                order_id,
            })
            .unwrap();

        let account = state.account("alice").unwrap();
        assert_eq!(account.locked, 0, "Locked should be 0 after cancel");
        assert_eq!(
            account.balance, 100_000_000,
            "Full balance restored after cancel"
        );
    }

    #[test]
    fn test_add_market_creates_orderbook() {
        let mut state = AppState::new();

        // Verify BTC-USDT exists by default
        assert!(state.orderbook("BTC-USDT").is_some());
        assert!(state.market_config("BTC-USDT").is_some());

        // Add a new market directly (bypassing admin check)
        let eth_config = MarketConfig {
            symbol: "ETH-USDT".to_string(),
            tick_size: 1,
            lot_size: 1,
            min_notional: 500,
            maker_fee: 2,
            taker_fee: 5,
            ..MarketConfig::default()
        };

        state.add_market(eth_config);
        // Override mark price (like execute_add_market does)
        state.mark_prices.insert("ETH-USDT".to_string(), 300_000); // $3,000
        state.mark_price_ema.insert("ETH-USDT".to_string(), 300_000);

        // Verify new market exists
        assert!(state.orderbook("ETH-USDT").is_some());
        assert!(state.market_config("ETH-USDT").is_some());
        assert_eq!(state.mark_price("ETH-USDT"), Some(300_000));
        assert_eq!(state.market_configs().len(), 2);

        // Verify can place order on new market
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();
        let fills = state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "ETH-USDT".into(),
                side: Side::Bid,
                price: 300_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();
        assert!(fills.is_empty()); // No counterparty
        assert!(state.orderbook("ETH-USDT").unwrap().best_bid().is_some());
    }

    #[test]
    fn test_snapshot_preserves_new_markets() {
        let mut state = AppState::new();

        // Add ETH-USDT
        let eth_config = MarketConfig {
            symbol: "ETH-USDT".to_string(),
            tick_size: 1,
            lot_size: 1,
            min_notional: 500,
            maker_fee: 3,
            taker_fee: 6,
            ..MarketConfig::default()
        };
        state.add_market(eth_config);
        state.mark_prices.insert("ETH-USDT".to_string(), 300_000);
        state.mark_price_ema.insert("ETH-USDT".to_string(), 300_000);

        // Create snapshot
        let snapshot = state.create_snapshot(100);
        assert_eq!(snapshot.market_configs.len(), 2);

        // Restore from snapshot
        let restored = AppState::from_snapshot(snapshot);
        assert_eq!(restored.market_configs().len(), 2);
        assert!(restored.market_config("BTC-USDT").is_some());
        assert!(restored.market_config("ETH-USDT").is_some());
        assert!(restored.orderbook("BTC-USDT").is_some());
        assert!(restored.orderbook("ETH-USDT").is_some());
        assert_eq!(restored.mark_price("ETH-USDT"), Some(300_000));

        // Verify config fields were preserved
        let eth = restored.market_config("ETH-USDT").unwrap();
        assert_eq!(eth.maker_fee, 3);
        assert_eq!(eth.taker_fee, 6);
    }

    #[test]
    fn try_snapshot_rejects_duplicate_primary_records() {
        let mut snapshot = crate::storage::AppSnapshot::genesis();
        snapshot.mark_prices.push(("BTC-USDT".to_string(), 1));
        let error = AppState::try_from_snapshot_with_chain_domain(snapshot, [0u8; 32], true)
            .err()
            .expect("duplicate mark price keys must fail closed");
        assert!(error.contains("duplicate mark_prices"));

        let mut snapshot = crate::storage::AppSnapshot::genesis();
        let account = crate::app::Account::new("Alice");
        snapshot.accounts = vec![account.clone(), account];
        let error = AppState::try_from_snapshot_with_chain_domain(snapshot, [0u8; 32], true)
            .err()
            .expect("duplicate case-folded account addresses must fail closed");
        assert!(error.contains("duplicate account address"));
    }

    #[test]
    fn try_snapshot_rejects_invalid_primary_records_and_wrong_pop_domain() {
        let mut snapshot = crate::storage::AppSnapshot::genesis();
        snapshot.mark_prices[0].1 = 0;
        let error = AppState::try_from_snapshot_with_chain_domain(snapshot, [0u8; 32], true)
            .err()
            .expect("zero mark price must fail closed");
        assert!(error.contains("positive mark price"));

        let domain = [7u8; 32];
        let wrong_domain = [8u8; 32];
        let node_id = [1u8; 32];
        let secret = crate::crypto::bls::BlsSecretKey::from_seed(&[9u8; 32]);
        let mut state = AppState::new_with_chain_domain(domain);
        state
            .staking
            .register_validator(
                "validator".into(),
                node_id,
                secret.public_key().to_bytes().to_vec(),
                secret
                    .create_proof_of_possession(&domain, &node_id)
                    .to_bytes()
                    .to_vec(),
                domain,
                crate::app::staking::MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        let mut snapshot = state.create_snapshot(0);
        snapshot
            .staking
            .as_mut()
            .unwrap()
            .validators
            .get_mut("validator")
            .unwrap()
            .bls_proof_of_possession = secret
            .create_proof_of_possession(&wrong_domain, &node_id)
            .to_bytes()
            .to_vec();

        let error = AppState::try_from_snapshot_with_chain_domain(snapshot, domain, true)
            .err()
            .expect("wrong-domain proof must fail closed");
        assert!(error.contains("proof of possession"));
    }

    #[test]
    fn oversized_snapshot_import_does_not_mutate_existing_state() {
        let mut existing = AppState::new();
        existing
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 123,
            })
            .unwrap();
        let balance_before = existing.account("alice").unwrap().balance;
        let markets_before = existing.market_configs().len();

        let mut incoming = crate::storage::AppSnapshot::genesis();
        let mut account = crate::app::Account::new("attacker");
        account.pending_nonces = (1
            ..=crate::storage::snapshot::MAX_SNAPSHOT_PENDING_NONCES_PER_ACCOUNT as u64 + 1)
            .collect();
        incoming.accounts = vec![account];

        let error = match AppState::try_from_snapshot_with_chain_domain(incoming, [0u8; 32], true) {
            Ok(_) => panic!("resource-bounded snapshot must fail before import"),
            Err(error) => error,
        };
        assert!(error.contains("pending nonces"));
        assert_eq!(existing.account("alice").unwrap().balance, balance_before);
        assert_eq!(existing.market_configs().len(), markets_before);
    }

    #[test]
    fn state_import_rebuilds_stale_derived_indexes_atomically() {
        let mut state = AppState::new();
        state
            .orderbooks
            .get_mut("BTC-USDT")
            .expect("genesis market")
            .order_index
            .insert("stale".to_string(), (Side::Bid, 1));
        state
            .staking
            .node_to_operator
            .insert([7u8; 32], "stale".to_string());
        state
            .trigger_orders_by_trader
            .insert("stale".to_string(), vec!["missing".to_string()]);

        state
            .validate_and_rebuild_derived_indexes()
            .expect("stale indexes are repairable");
        assert!(state.orderbooks["BTC-USDT"].order_index.is_empty());
        assert!(state.staking.node_to_operator.is_empty());
        assert!(state.trigger_orders_by_trader.is_empty());
        state
            .validate_trigger_indexes()
            .expect("rebuilt trigger indexes are valid");
    }

    #[test]
    fn test_add_market_tx_rejects_without_admin() {
        let mut state = AppState::new();

        // AddMarket transaction should fail when no admin address is configured
        let result = state.execute_tx(Transaction::AddMarket {
            admin: "0x1234".into(),
            config: MarketConfig {
                symbol: "ETH-USDT".to_string(),
                ..MarketConfig::default()
            },
            initial_mark_price: 300_000,
        });
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("unauthorized"),
            "Expected unauthorized error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_withdraw_blocked_with_positions() {
        let mut state = AppState::new();

        // Setup: Alice and Bob each deposit
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 10_000_000,
            })
            .unwrap();
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 10_000_000,
            })
            .unwrap();

        // Alice buys 1 BTC at $50k from Bob
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // Alice has a position now. Try to withdraw most of her balance
        let account = state.account("alice").unwrap();
        let balance = account.balance;

        if balance > 0 {
            let result = state.execute_tx(Transaction::Withdraw {
                trader: "alice".into(),
                amount: balance,
            });
            // Should fail - withdrawing everything would leave equity below maintenance
            assert!(result.is_err() || state.account("alice").unwrap().balance > 0);
        }
    }
}
