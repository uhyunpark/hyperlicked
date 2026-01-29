//! Staking State
//!
//! Core state management for the staking system.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::types::{
    Delegation, EpochSnapshot, Evidence, LivenessRecord, UnstakeRequest, ValidatorInfo,
    ValidatorSetUpdate, ValidatorStatus, MAX_ACTIVE_VALIDATORS, MIN_SELF_STAKE, ROUNDS_PER_EPOCH,
};
use crate::app::Address;
use crate::types::NodeId;

/// Complete staking state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingState {
    /// Registered validators by operator address
    pub validators: HashMap<Address, ValidatorInfo>,
    /// Validator lookup by node_id
    #[serde(skip)]
    pub node_to_operator: HashMap<NodeId, Address>,
    /// Delegations: (delegator, validator) -> Delegation
    pub delegations: HashMap<(Address, Address), Delegation>,
    /// Unstake queue by delegator
    pub unstake_queue: HashMap<Address, Vec<UnstakeRequest>>,
    /// Current epoch number
    pub current_epoch: u64,
    /// Snapshot of validator set at epoch start
    pub epoch_snapshot: Option<EpochSnapshot>,
    /// Liveness records by node_id
    pub liveness: HashMap<NodeId, LivenessRecord>,
    /// Pending evidence to process
    pub pending_evidence: Vec<Evidence>,
    /// Total staked across all validators
    pub total_staked: i64,
    /// Rewards pool (accumulated from fees, distributed at epoch end)
    pub rewards_pool: i64,
    /// Whether staking is enabled (for single-node compatibility)
    pub enabled: bool,
}

impl Default for StakingState {
    fn default() -> Self {
        Self::new()
    }
}

impl StakingState {
    /// Create new staking state
    pub fn new() -> Self {
        Self {
            validators: HashMap::new(),
            node_to_operator: HashMap::new(),
            delegations: HashMap::new(),
            unstake_queue: HashMap::new(),
            current_epoch: 0,
            epoch_snapshot: None,
            liveness: HashMap::new(),
            pending_evidence: Vec::new(),
            total_staked: 0,
            rewards_pool: 0,
            enabled: true,
        }
    }

    /// Register a new validator
    pub fn register_validator(
        &mut self,
        operator: Address,
        node_id: NodeId,
        bls_pubkey: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    ) -> Result<(), StakingError> {
        // Validate inputs
        if self_stake < MIN_SELF_STAKE {
            return Err(StakingError::InsufficientSelfStake);
        }
        if commission_bps < 0 || commission_bps > super::types::MAX_COMMISSION_BPS {
            return Err(StakingError::InvalidCommission);
        }
        if bls_pubkey.len() != 48 {
            return Err(StakingError::InvalidBlsKey);
        }
        if self.validators.contains_key(&operator) {
            return Err(StakingError::ValidatorAlreadyExists);
        }
        if self.node_to_operator.contains_key(&node_id) {
            return Err(StakingError::NodeIdAlreadyRegistered);
        }

        let validator = ValidatorInfo::new(operator.clone(), node_id, bls_pubkey, self_stake, commission_bps);
        self.validators.insert(operator.clone(), validator);
        self.node_to_operator.insert(node_id, operator);
        self.total_staked += self_stake;

        Ok(())
    }

    /// Get validator by operator address
    pub fn get_validator(&self, operator: &Address) -> Option<&ValidatorInfo> {
        self.validators.get(operator)
    }

    /// Get validator by node_id
    pub fn get_validator_by_node(&self, node_id: &NodeId) -> Option<&ValidatorInfo> {
        self.node_to_operator
            .get(node_id)
            .and_then(|op| self.validators.get(op))
    }

    /// Get mutable validator by operator address
    pub fn get_validator_mut(&mut self, operator: &Address) -> Option<&mut ValidatorInfo> {
        self.validators.get_mut(operator)
    }

    /// Get mutable validator by node_id
    pub fn get_validator_by_node_mut(&mut self, node_id: &NodeId) -> Option<&mut ValidatorInfo> {
        let operator = self.node_to_operator.get(node_id)?.clone();
        self.validators.get_mut(&operator)
    }

    /// Add delegation to a validator
    pub fn delegate(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: i64,
    ) -> Result<(), StakingError> {
        if amount <= 0 {
            return Err(StakingError::InvalidAmount);
        }

        let val = self
            .validators
            .get_mut(&validator)
            .ok_or(StakingError::ValidatorNotFound)?;

        if val.status == ValidatorStatus::Tombstoned {
            return Err(StakingError::ValidatorTombstoned);
        }

        // Update validator total stake
        val.total_stake += amount;
        self.total_staked += amount;

        // Update or create delegation
        let key = (delegator.clone(), validator.clone());
        self.delegations
            .entry(key)
            .and_modify(|d| d.amount += amount)
            .or_insert_with(|| Delegation::new(delegator, validator, amount));

        Ok(())
    }

    /// Queue undelegation (enters 7-day unbonding period)
    pub fn undelegate(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: i64,
        current_time: u64,
    ) -> Result<(), StakingError> {
        if amount <= 0 {
            return Err(StakingError::InvalidAmount);
        }

        let key = (delegator.clone(), validator.clone());
        let delegation = self
            .delegations
            .get_mut(&key)
            .ok_or(StakingError::DelegationNotFound)?;

        if delegation.amount < amount {
            return Err(StakingError::InsufficientDelegation);
        }

        // Reduce delegation immediately
        delegation.amount -= amount;
        if delegation.amount == 0 {
            self.delegations.remove(&key);
        }

        // Reduce validator total stake
        if let Some(val) = self.validators.get_mut(&validator) {
            val.total_stake -= amount;
        }
        self.total_staked -= amount;

        // Add to unstake queue
        let request = UnstakeRequest {
            delegator: delegator.clone(),
            validator: Some(validator),
            amount,
            completion_time: current_time + super::types::UNSTAKE_DELAY_MS,
        };
        self.unstake_queue
            .entry(delegator)
            .or_default()
            .push(request);

        Ok(())
    }

    /// Process completed unstake requests, returns total amount to return
    pub fn process_unstake_queue(&mut self, current_time: u64) -> Vec<(Address, i64)> {
        let mut completed = Vec::new();

        for (delegator, requests) in self.unstake_queue.iter_mut() {
            let (ready, pending): (Vec<_>, Vec<_>) = requests
                .drain(..)
                .partition(|r| r.completion_time <= current_time);

            let total_amount: i64 = ready.iter().map(|r| r.amount).sum();
            if total_amount > 0 {
                completed.push((delegator.clone(), total_amount));
            }
            *requests = pending;
        }

        // Clean up empty entries
        self.unstake_queue.retain(|_, v| !v.is_empty());

        completed
    }

    /// Get active validators sorted by stake (descending)
    pub fn active_validators(&self) -> Vec<NodeId> {
        let mut validators: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.can_be_active())
            .collect();

        // Sort by total stake descending, then by operator address for determinism
        validators.sort_by(|a, b| {
            b.total_stake
                .cmp(&a.total_stake)
                .then_with(|| a.operator.cmp(&b.operator))
        });

        // Take top N
        validators
            .into_iter()
            .take(MAX_ACTIVE_VALIDATORS)
            .map(|v| v.node_id)
            .collect()
    }

    /// Get active validator set for consensus
    ///
    /// Returns (node_ids, bls_pubkeys, stakes) tuples for the active set.
    /// Used to update consensus configuration on epoch transitions.
    pub fn active_validator_set_for_consensus(&self) -> ValidatorSetUpdate {
        let mut validators: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.can_be_active())
            .collect();

        // Sort by total stake descending, then by operator address for determinism
        validators.sort_by(|a, b| {
            b.total_stake
                .cmp(&a.total_stake)
                .then_with(|| a.operator.cmp(&b.operator))
        });

        // Take top N
        let active: Vec<_> = validators
            .into_iter()
            .take(MAX_ACTIVE_VALIDATORS)
            .collect();

        ValidatorSetUpdate {
            node_ids: active.iter().map(|v| v.node_id).collect(),
            bls_pubkeys: active.iter().map(|v| v.bls_pubkey.clone()).collect(),
            stakes: active.iter().map(|v| (v.node_id, v.total_stake as u64)).collect(),
        }
    }

    /// Check if should transition to new epoch
    pub fn should_transition_epoch(&self, view: u64) -> bool {
        match &self.epoch_snapshot {
            Some(snapshot) => {
                let rounds_in_epoch = view.saturating_sub(snapshot.start_view);
                rounds_in_epoch >= ROUNDS_PER_EPOCH
            }
            None => true, // First epoch
        }
    }

    /// Get delegations for a specific delegator
    pub fn delegations_for(&self, delegator: &Address) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|((d, _), _)| d == delegator)
            .map(|(_, del)| del)
            .collect()
    }

    /// Get all delegations to a specific validator
    pub fn delegations_to(&self, validator: &Address) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|((_, v), _)| v == validator)
            .map(|(_, del)| del)
            .collect()
    }

    /// Rebuild node_to_operator mapping after deserialization
    pub fn rebuild_index(&mut self) {
        self.node_to_operator.clear();
        for (operator, validator) in &self.validators {
            self.node_to_operator
                .insert(validator.node_id, operator.clone());
        }
    }
}

/// Staking errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum StakingError {
    #[error("insufficient self-stake (minimum ${} required)", MIN_SELF_STAKE / 100)]
    InsufficientSelfStake,
    #[error("invalid commission rate")]
    InvalidCommission,
    #[error("invalid BLS key (must be 48 bytes)")]
    InvalidBlsKey,
    #[error("validator already exists")]
    ValidatorAlreadyExists,
    #[error("node ID already registered")]
    NodeIdAlreadyRegistered,
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("validator is tombstoned")]
    ValidatorTombstoned,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("delegation not found")]
    DelegationNotFound,
    #[error("insufficient delegation")]
    InsufficientDelegation,
    #[error("validator is jailed")]
    ValidatorJailed,
    #[error("validator not jailed")]
    ValidatorNotJailed,
    #[error("jail period not expired")]
    JailPeriodNotExpired,
    #[error("invalid evidence")]
    InvalidEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key() -> Vec<u8> {
        vec![0u8; 48]
    }

    #[test]
    fn test_register_validator() {
        let mut state = StakingState::new();

        // Should succeed with minimum stake
        assert!(state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(),
                MIN_SELF_STAKE,
                500,
            )
            .is_ok());

        assert_eq!(state.validators.len(), 1);
        assert_eq!(state.total_staked, MIN_SELF_STAKE);

        // Should fail with insufficient stake
        assert!(matches!(
            state.register_validator(
                "bob".into(),
                test_node_id(2),
                test_bls_key(),
                MIN_SELF_STAKE - 1,
                500,
            ),
            Err(StakingError::InsufficientSelfStake)
        ));
    }

    #[test]
    fn test_delegation() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "validator".into(),
                test_node_id(1),
                test_bls_key(),
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        // Delegate
        state.delegate("delegator".into(), "validator".into(), 1000_00).unwrap();

        let val = state.get_validator(&"validator".into()).unwrap();
        assert_eq!(val.total_stake, MIN_SELF_STAKE + 1000_00);
        assert_eq!(state.total_staked, MIN_SELF_STAKE + 1000_00);

        // Undelegate
        state.undelegate("delegator".into(), "validator".into(), 500_00, 0).unwrap();

        let val = state.get_validator(&"validator".into()).unwrap();
        assert_eq!(val.total_stake, MIN_SELF_STAKE + 500_00);
    }

    #[test]
    fn test_active_validators_sorted() {
        let mut state = StakingState::new();

        // Register validators with different stakes
        state
            .register_validator("low".into(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();
        state
            .register_validator(
                "high".into(),
                test_node_id(2),
                test_bls_key(),
                MIN_SELF_STAKE * 2,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "mid".into(),
                test_node_id(3),
                test_bls_key(),
                MIN_SELF_STAKE + 100_00,
                500,
            )
            .unwrap();

        let active = state.active_validators();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0], test_node_id(2)); // highest stake
        assert_eq!(active[1], test_node_id(3)); // mid stake
        assert_eq!(active[2], test_node_id(1)); // lowest stake
    }
}
