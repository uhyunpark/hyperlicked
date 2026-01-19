//! Rewards
//!
//! Handles reward calculation and distribution to validators and delegators.
//!
//! Reward flow:
//! 1. Block rewards and transaction fees accumulate in rewards_pool
//! 2. At epoch end, pool is distributed proportionally by stake
//! 3. Validators take commission from delegator rewards
//! 4. Users can claim rewards at any time

use super::state::{StakingError, StakingState};
use super::types::BLOCK_REWARD;
use crate::app::Address;

/// Reward claim result
#[derive(Debug, Clone)]
pub struct RewardClaimResult {
    /// Address that claimed
    pub claimant: Address,
    /// Validator address (for delegation rewards)
    pub validator: Option<Address>,
    /// Amount claimed
    pub amount: i64,
}

impl StakingState {
    /// Add block reward to the pool (called for each committed block)
    pub fn add_block_reward(&mut self) {
        self.rewards_pool += BLOCK_REWARD;
    }

    /// Add fee reward to the pool
    pub fn add_fee_reward(&mut self, amount: i64) {
        if amount > 0 {
            self.rewards_pool += amount;
        }
    }

    /// Calculate pending rewards for a validator (excluding delegator share)
    pub fn validator_pending_rewards(&self, operator: &Address) -> i64 {
        self.validators
            .get(operator)
            .map(|v| v.pending_rewards)
            .unwrap_or(0)
    }

    /// Calculate pending rewards for a delegation
    pub fn delegation_pending_rewards(&self, delegator: &Address, validator: &Address) -> i64 {
        self.delegations
            .get(&(delegator.clone(), validator.clone()))
            .map(|d| d.pending_rewards)
            .unwrap_or(0)
    }

    /// Claim validator rewards
    pub fn claim_validator_rewards(
        &mut self,
        operator: &Address,
    ) -> Result<RewardClaimResult, StakingError> {
        let validator = self
            .validators
            .get_mut(operator)
            .ok_or(StakingError::ValidatorNotFound)?;

        let amount = validator.pending_rewards;
        validator.pending_rewards = 0;

        Ok(RewardClaimResult {
            claimant: operator.clone(),
            validator: None,
            amount,
        })
    }

    /// Claim delegation rewards
    pub fn claim_delegation_rewards(
        &mut self,
        delegator: &Address,
        validator: &Address,
    ) -> Result<RewardClaimResult, StakingError> {
        let delegation = self
            .delegations
            .get_mut(&(delegator.clone(), validator.clone()))
            .ok_or(StakingError::DelegationNotFound)?;

        let amount = delegation.pending_rewards;
        delegation.pending_rewards = 0;

        Ok(RewardClaimResult {
            claimant: delegator.clone(),
            validator: Some(validator.clone()),
            amount,
        })
    }

    /// Distribute rewards to delegators after epoch ends
    /// Called internally after main reward distribution
    pub fn distribute_delegator_rewards(&mut self) {
        // Get all active validators and their rewards
        let validator_rewards: Vec<_> = self
            .validators
            .iter()
            .filter(|(_, v)| v.pending_rewards > 0)
            .map(|(op, v)| (op.clone(), v.pending_rewards, v.commission_bps, v.total_stake, v.self_stake))
            .collect();

        for (operator, total_reward, commission_bps, total_stake, self_stake) in validator_rewards {
            if total_stake == 0 || total_reward == 0 {
                continue;
            }

            // Get all delegations to this validator
            let delegations: Vec<_> = self
                .delegations
                .iter()
                .filter(|((_, v), d)| v == &operator && d.amount > 0)
                .map(|((del, _), d)| (del.clone(), d.amount))
                .collect();

            if delegations.is_empty() {
                continue;
            }

            // Calculate total delegated (excluding self-stake)
            let total_delegated: i64 = delegations.iter().map(|(_, a)| a).sum();

            // Delegator share of rewards (proportional to delegated stake)
            let delegator_pool_share = (total_reward as i128 * total_delegated as i128
                / total_stake as i128) as i64;

            // Apply commission
            let commission = (delegator_pool_share as i128 * commission_bps as i128 / 10000) as i64;
            let net_delegator_rewards = delegator_pool_share - commission;

            // Distribute to delegators
            for (delegator, amount) in delegations {
                if total_delegated > 0 {
                    let share =
                        (net_delegator_rewards as i128 * amount as i128 / total_delegated as i128)
                            as i64;
                    if let Some(del) =
                        self.delegations.get_mut(&(delegator, operator.clone()))
                    {
                        del.pending_rewards += share;
                    }
                }
            }

            // Validator keeps their self-stake share + commission
            if let Some(validator) = self.validators.get_mut(&operator) {
                // Validator's share is proportional to self-stake
                let self_stake_share = (total_reward as i128 * self_stake as i128
                    / total_stake as i128) as i64;
                // Reduce pending rewards (they were already added in distribute_rewards)
                // and set to self_stake_share + commission
                validator.pending_rewards = self_stake_share + commission;
            }
        }
    }

    /// Get estimated APY for a validator (in basis points, 10000 = 100%)
    pub fn estimated_apy(&self, _operator: &Address) -> i64 {
        // Simplified APY calculation
        // In reality, this would use historical data and more sophisticated math
        // APY = (rewards_per_epoch * epochs_per_year) / total_stake * 10000

        // Assume ~390 epochs per year (365 * 24 * 60 / 90 minutes)
        // This is a placeholder - real implementation would track historical rewards
        500 // 5% default APY
    }

    /// Get total rewards distributed to date
    pub fn total_rewards_distributed(&self) -> i64 {
        // Sum of all pending rewards (not yet claimed)
        let validator_rewards: i64 = self.validators.values().map(|v| v.pending_rewards).sum();
        let delegator_rewards: i64 = self.delegations.values().map(|d| d.pending_rewards).sum();
        validator_rewards + delegator_rewards
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::staking::types::MIN_SELF_STAKE;
    use crate::types::NodeId;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key() -> Vec<u8> {
        vec![0u8; 48]
    }

    #[test]
    fn test_add_rewards() {
        let mut state = StakingState::new();

        state.add_block_reward();
        assert_eq!(state.rewards_pool, BLOCK_REWARD);

        state.add_fee_reward(100);
        assert_eq!(state.rewards_pool, BLOCK_REWARD + 100);
    }

    #[test]
    fn test_claim_validator_rewards() {
        let mut state = StakingState::new();
        let v1 = String::from("v1");
        state
            .register_validator(v1.clone(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();

        // Add some pending rewards directly
        state.validators.get_mut(&v1).unwrap().pending_rewards = 1000;

        // Claim
        let result = state.claim_validator_rewards(&v1).unwrap();
        assert_eq!(result.amount, 1000);

        // Should be cleared now
        assert_eq!(state.validator_pending_rewards(&v1), 0);
    }

    #[test]
    fn test_claim_delegation_rewards() {
        let mut state = StakingState::new();
        let v1 = String::from("v1");
        let d1 = String::from("d1");
        state
            .register_validator(v1.clone(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();
        state.delegate(d1.clone(), v1.clone(), 1000_00).unwrap();

        // Add some pending rewards directly
        state
            .delegations
            .get_mut(&(d1.clone(), v1.clone()))
            .unwrap()
            .pending_rewards = 500;

        // Claim
        let result = state.claim_delegation_rewards(&d1, &v1).unwrap();
        assert_eq!(result.amount, 500);
        assert_eq!(result.validator, Some(v1));
    }
}
