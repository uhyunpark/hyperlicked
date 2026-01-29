//! Epoch Management
//!
//! Handles epoch transitions and active validator set computation.

use super::state::{StakingError, StakingState};
use super::types::{
    EpochSnapshot, LivenessRecord, ValidatorStatus, JAIL_DURATION_MS, MAX_ACTIVE_VALIDATORS,
    MIN_LIVENESS_BPS, ROUNDS_PER_EPOCH,
};
use crate::types::NodeId;

/// Result of an epoch transition
#[derive(Debug, Clone)]
pub struct EpochTransitionResult {
    /// New epoch number
    pub epoch: u64,
    /// New active validator set
    pub new_active_set: Vec<NodeId>,
    /// Validators jailed for downtime
    pub jailed: Vec<NodeId>,
    /// Rewards distributed (validator, amount)
    pub rewards: Vec<(NodeId, i64)>,
    /// Unstake completions (delegator, amount)
    pub unstake_completions: Vec<(String, i64)>,
    /// Validator set update for consensus layer
    pub validator_set_update: Option<super::ValidatorSetUpdate>,
}

impl StakingState {
    /// Transition to a new epoch
    ///
    /// This is called when `should_transition_epoch` returns true.
    /// It processes:
    /// 1. Liveness-based jailing
    /// 2. Reward distribution
    /// 3. Unstake queue processing
    /// 4. New active set computation
    pub fn transition_epoch(
        &mut self,
        view: u64,
        timestamp: u64,
    ) -> EpochTransitionResult {
        let new_epoch = self.current_epoch + 1;
        let mut result = EpochTransitionResult {
            epoch: new_epoch,
            new_active_set: Vec::new(),
            jailed: Vec::new(),
            rewards: Vec::new(),
            unstake_completions: Vec::new(),
            validator_set_update: None,
        };

        // 1. Process liveness (only if we have multiple validators)
        if self.validators.len() > 1 {
            result.jailed = self.process_liveness(timestamp);
        }

        // 2. Distribute rewards
        result.rewards = self.distribute_rewards();

        // 3. Process unstake queue
        let completions = self.process_unstake_queue(timestamp);
        result.unstake_completions = completions;

        // 4. Compute new active set
        result.new_active_set = self.compute_active_set(timestamp);

        // 5. Create validator set update for consensus
        result.validator_set_update = Some(self.active_validator_set_for_consensus());

        // 6. Create new epoch snapshot
        let mut snapshot = EpochSnapshot::new(new_epoch, view, timestamp);
        snapshot.active_validators = result.new_active_set.clone();
        snapshot.total_staked = self.total_staked;
        self.epoch_snapshot = Some(snapshot);

        // 7. Reset liveness tracking for new epoch
        self.reset_liveness();

        // 8. Reset validator epoch counters
        for validator in self.validators.values_mut() {
            validator.reset_epoch_counters();
        }

        self.current_epoch = new_epoch;
        result
    }

    /// Process liveness records and jail underperforming validators
    fn process_liveness(&mut self, timestamp: u64) -> Vec<NodeId> {
        // First pass: collect nodes to jail (to avoid borrow conflict)
        let to_jail: Vec<NodeId> = self
            .liveness
            .iter()
            .filter(|(_, record)| record.liveness_bps() < MIN_LIVENESS_BPS)
            .map(|(node_id, _)| *node_id)
            .collect();

        // Second pass: apply jailing
        let mut jailed = Vec::new();
        for node_id in to_jail {
            if let Some(validator) = self.get_validator_by_node_mut(&node_id) {
                if validator.status == ValidatorStatus::Active {
                    validator.status = ValidatorStatus::Jailed;
                    validator.jail_until = timestamp + JAIL_DURATION_MS;
                    jailed.push(node_id);
                }
            }
        }

        jailed
    }

    /// Distribute rewards from the pool
    fn distribute_rewards(&mut self) -> Vec<(NodeId, i64)> {
        if self.rewards_pool == 0 {
            return Vec::new();
        }

        let active_validators: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.status == ValidatorStatus::Active)
            .map(|v| (v.node_id, v.total_stake))
            .collect();

        if active_validators.is_empty() {
            return Vec::new();
        }

        let total_active_stake: i64 = active_validators.iter().map(|(_, s)| s).sum();
        if total_active_stake == 0 {
            return Vec::new();
        }

        let mut rewards = Vec::new();
        let pool = self.rewards_pool;
        self.rewards_pool = 0;

        for (node_id, stake) in active_validators {
            // Proportional reward based on stake
            let reward = (pool as i128 * stake as i128 / total_active_stake as i128) as i64;
            if reward > 0 {
                if let Some(validator) = self.get_validator_by_node_mut(&node_id) {
                    validator.pending_rewards += reward;
                    rewards.push((node_id, reward));
                }
            }
        }

        rewards
    }

    /// Compute new active validator set
    fn compute_active_set(&mut self, timestamp: u64) -> Vec<NodeId> {
        // First, try to unjail validators whose jail period has expired
        let jailed_validators: Vec<_> = self
            .validators
            .iter()
            .filter(|(_, v)| v.status == ValidatorStatus::Jailed && v.jail_until <= timestamp)
            .map(|(op, _)| op.clone())
            .collect();

        for operator in jailed_validators {
            if let Some(validator) = self.validators.get_mut(&operator) {
                validator.status = ValidatorStatus::Inactive;
                validator.jail_until = 0;
            }
        }

        // Get sorted list of eligible validators
        let mut eligible: Vec<_> = self
            .validators
            .values_mut()
            .filter(|v| v.can_be_active())
            .collect();

        // Sort by stake descending, then by operator for determinism
        eligible.sort_by(|a, b| {
            b.total_stake
                .cmp(&a.total_stake)
                .then_with(|| a.operator.cmp(&b.operator))
        });

        // Take top N and mark as active
        let active_count = eligible.len().min(MAX_ACTIVE_VALIDATORS);
        let mut new_active = Vec::with_capacity(active_count);

        for (i, validator) in eligible.into_iter().enumerate() {
            if i < active_count {
                validator.status = ValidatorStatus::Active;
                new_active.push(validator.node_id);
            } else {
                validator.status = ValidatorStatus::Inactive;
            }
        }

        new_active
    }

    /// Reset liveness tracking for new epoch
    fn reset_liveness(&mut self) {
        self.liveness.clear();
        // Initialize liveness records for active validators
        if let Some(snapshot) = &self.epoch_snapshot {
            for node_id in &snapshot.active_validators {
                self.liveness.insert(*node_id, LivenessRecord::default());
            }
        }
    }

    /// Record that a validator proposed a block
    pub fn record_block_proposed(&mut self, proposer: NodeId) {
        if let Some(record) = self.liveness.get_mut(&proposer) {
            record.actual_proposals += 1;
        }
        if let Some(validator) = self.get_validator_by_node_mut(&proposer) {
            validator.blocks_proposed += 1;
            validator.missed_consecutive = 0;
        }
    }

    /// Record that a validator voted
    pub fn record_vote(&mut self, voter: NodeId) {
        if let Some(record) = self.liveness.get_mut(&voter) {
            record.actual_votes += 1;
        }
        if let Some(validator) = self.get_validator_by_node_mut(&voter) {
            validator.votes_cast += 1;
            validator.missed_consecutive = 0;
        }
    }

    /// Record that a validator missed their turn to propose
    pub fn record_missed_proposal(&mut self, expected_leader: NodeId, timestamp: u64) {
        if let Some(record) = self.liveness.get_mut(&expected_leader) {
            record.expected_proposals += 1;
        }
        if let Some(validator) = self.get_validator_by_node_mut(&expected_leader) {
            validator.missed_consecutive += 1;

            // Auto-jail if too many consecutive misses
            if validator.missed_consecutive >= super::types::MAX_CONSECUTIVE_MISSED {
                if validator.status == ValidatorStatus::Active {
                    validator.status = ValidatorStatus::Jailed;
                    validator.jail_until = timestamp + JAIL_DURATION_MS;
                }
            }
        }
    }

    /// Record expected vote for a block (called for each active validator)
    pub fn record_expected_vote(&mut self, voter: NodeId) {
        if let Some(record) = self.liveness.get_mut(&voter) {
            record.expected_votes += 1;
        }
    }

    /// Manually unjail a validator (must wait for jail period to expire)
    pub fn unjail(&mut self, operator: &str, timestamp: u64) -> Result<(), StakingError> {
        let validator = self
            .validators
            .get_mut(operator)
            .ok_or(StakingError::ValidatorNotFound)?;

        if validator.status != ValidatorStatus::Jailed {
            return Err(StakingError::ValidatorNotJailed);
        }

        if timestamp < validator.jail_until {
            return Err(StakingError::JailPeriodNotExpired);
        }

        validator.status = ValidatorStatus::Inactive;
        validator.jail_until = 0;
        validator.missed_consecutive = 0;

        Ok(())
    }

    /// Get current epoch info
    pub fn current_epoch_info(&self) -> Option<&EpochSnapshot> {
        self.epoch_snapshot.as_ref()
    }

    /// Get rounds remaining in current epoch
    pub fn rounds_until_epoch_end(&self, current_view: u64) -> u64 {
        match &self.epoch_snapshot {
            Some(snapshot) => {
                let rounds_elapsed = current_view.saturating_sub(snapshot.start_view);
                ROUNDS_PER_EPOCH.saturating_sub(rounds_elapsed)
            }
            None => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::staking::types::MIN_SELF_STAKE;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key() -> Vec<u8> {
        vec![0u8; 48]
    }

    #[test]
    fn test_epoch_transition() {
        let mut state = StakingState::new();

        // Register some validators
        state
            .register_validator("v1".into(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();
        state
            .register_validator("v2".into(), test_node_id(2), test_bls_key(), MIN_SELF_STAKE * 2, 500)
            .unwrap();

        // First epoch transition
        let result = state.transition_epoch(0, 1000);
        assert_eq!(result.epoch, 1);
        assert_eq!(result.new_active_set.len(), 2);
        assert_eq!(result.new_active_set[0], test_node_id(2)); // Higher stake first

        // Validators should be active now
        assert_eq!(
            state.get_validator(&"v1".into()).unwrap().status,
            ValidatorStatus::Active
        );
    }

    #[test]
    fn test_liveness_jailing() {
        let mut state = StakingState::new();

        state
            .register_validator("v1".into(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();
        state
            .register_validator("v2".into(), test_node_id(2), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();

        // First epoch to make validators active
        state.transition_epoch(0, 1000);

        // Simulate v1 being active, v2 missing everything
        for _ in 0..100 {
            state.record_expected_vote(test_node_id(1));
            state.record_expected_vote(test_node_id(2));
            state.record_vote(test_node_id(1));
            // v2 doesn't vote
        }

        // Epoch transition should jail v2
        let result = state.transition_epoch(ROUNDS_PER_EPOCH, 2000);
        assert!(result.jailed.contains(&test_node_id(2)));
        assert_eq!(
            state.get_validator(&"v2".into()).unwrap().status,
            ValidatorStatus::Jailed
        );
    }

    #[test]
    fn test_epoch_transition_includes_validator_set_update() {
        let mut state = StakingState::new();

        // Register validators with different stakes
        state
            .register_validator("v1".into(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();
        state
            .register_validator("v2".into(), test_node_id(2), test_bls_key(), MIN_SELF_STAKE * 3, 500)
            .unwrap();
        state
            .register_validator("v3".into(), test_node_id(3), test_bls_key(), MIN_SELF_STAKE * 2, 500)
            .unwrap();

        // First epoch transition
        let result = state.transition_epoch(0, 1000);

        // Should have validator set update
        assert!(result.validator_set_update.is_some());
        let update = result.validator_set_update.unwrap();

        // Should have 3 validators
        assert_eq!(update.len(), 3);

        // Should be sorted by stake (v2 > v3 > v1)
        assert_eq!(update.node_ids[0], test_node_id(2)); // Highest stake
        assert_eq!(update.node_ids[1], test_node_id(3)); // Second highest
        assert_eq!(update.node_ids[2], test_node_id(1)); // Lowest stake

        // BLS keys should match
        assert_eq!(update.bls_pubkeys.len(), 3);
        assert_eq!(update.bls_pubkeys[0].len(), 48);

        // Stakes should be correct
        assert_eq!(update.stakes.len(), 3);
        assert_eq!(update.stakes[0].1, (MIN_SELF_STAKE * 3) as u64);
        assert_eq!(update.stakes[1].1, (MIN_SELF_STAKE * 2) as u64);
        assert_eq!(update.stakes[2].1, MIN_SELF_STAKE as u64);
    }
}
