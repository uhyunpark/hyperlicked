//! Rewards
//!
//! Staking rewards are funded solely by an explicitly funded emissions
//! reserve.  The reward curve and all allocations use integer arithmetic so
//! every validator deterministically reaches the same result.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::state::{StakingError, StakingState};
use super::types::{
    ValidatorStatus, STAKING_AUTO_COMPOUND_INTERVAL_MS, STAKING_REWARD_ANCHOR_STAKE,
    STAKING_REWARD_APY_BPS, STAKING_REWARD_BPS_DENOMINATOR, STAKING_REWARD_EPOCH_MS,
    STAKING_REWARD_YEAR_MS,
};
use crate::app::Address;
use crate::types::NodeId;

/// Reward claim result.
#[derive(Debug, Clone)]
pub struct RewardClaimResult {
    /// Address that claimed.
    pub claimant: Address,
    /// Validator address (for delegation rewards).
    pub validator: Option<Address>,
    /// Amount claimed.
    pub amount: i64,
}

/// Integer reward curve output for one year at a given aggregate stake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RewardCurve {
    /// Annual reward in HYCK base units.
    pub annual_reward: i64,
    /// Integer APY in basis points.
    pub apy_bps: i64,
}

/// Schema version for canonical reward-credit payloads emitted as a block
/// system event.  ClaimRewards receipts remain a separate transaction event.
pub const STAKING_REWARD_EVENT_SCHEMA_VERSION: u16 = 1;

/// The economic recipient represented by one canonical reward credit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RewardRecipientType {
    ValidatorSelf,
    ValidatorCommission,
    Delegator,
}

impl RewardRecipientType {
    fn sort_rank(self) -> u8 {
        match self {
            Self::ValidatorSelf => 0,
            Self::ValidatorCommission => 1,
            Self::Delegator => 2,
        }
    }
}

/// Canonical, indexer-facing description of one funded reward credit.
///
/// `gross` is the entitlement before a delegator commission deduction;
/// `commission` is that deduction on delegator rows; `net` is the amount
/// credited to `recipient`.  The validator commission row is the recipient of
/// that deduction and therefore has `gross == net` and `commission == 0`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardCredit {
    pub recipient_type: RewardRecipientType,
    pub recipient: Address,
    pub validator: Address,
    pub gross: i64,
    pub commission: i64,
    pub net: i64,
    /// Deterministic portion of this newly credited amount included in the
    /// same block's compound. The complete balance movement, including older
    /// pending rewards, is recorded separately in `compoundings`.
    pub compounded: i64,
}

/// The exact pending-reward balance moved into bonded stake by an automatic
/// compound. This is separate from [`RewardCredit`] because a compound may
/// consume rewards accumulated in earlier blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardCompounding {
    pub recipient_type: RewardCompoundingRecipientType,
    pub recipient: Address,
    pub validator: Address,
    pub amount: i64,
}

/// The bonded bucket receiving an automatic compound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RewardCompoundingRecipientType {
    Validator,
    Delegator,
}

/// Result of one reward-clock tick, including the exact credits and bonded
/// balance movements that were applied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RewardAccrualResult {
    pub validator_rewards: Vec<(NodeId, i64)>,
    pub credits: Vec<RewardCredit>,
    pub compoundings: Vec<RewardCompounding>,
    pub total_distributed: i64,
    pub auto_compounded: i64,
}

#[derive(Debug, Clone)]
enum RewardCreditPlan {
    ValidatorSelf {
        operator: Address,
        gross: i64,
    },
    ValidatorCommission {
        operator: Address,
        gross: i64,
    },
    Delegation {
        delegator: Address,
        validator: Address,
        gross: i64,
        commission: i64,
        net: i64,
    },
}

impl RewardCreditPlan {
    fn record(&self) -> RewardCredit {
        match self {
            Self::ValidatorSelf { operator, gross } => RewardCredit {
                recipient_type: RewardRecipientType::ValidatorSelf,
                recipient: operator.clone(),
                validator: operator.clone(),
                gross: *gross,
                commission: 0,
                net: *gross,
                compounded: 0,
            },
            Self::ValidatorCommission { operator, gross } => RewardCredit {
                recipient_type: RewardRecipientType::ValidatorCommission,
                recipient: operator.clone(),
                validator: operator.clone(),
                gross: *gross,
                commission: 0,
                net: *gross,
                compounded: 0,
            },
            Self::Delegation {
                delegator,
                validator,
                gross,
                commission,
                net,
            } => RewardCredit {
                recipient_type: RewardRecipientType::Delegator,
                recipient: delegator.clone(),
                validator: validator.clone(),
                gross: *gross,
                commission: *commission,
                net: *net,
                compounded: 0,
            },
        }
    }
}

fn sort_reward_credits(credits: &mut [RewardCredit]) {
    credits.sort_by(|left, right| {
        left.recipient_type
            .sort_rank()
            .cmp(&right.recipient_type.sort_rank())
            .then_with(|| left.recipient.cmp(&right.recipient))
            .then_with(|| left.validator.cmp(&right.validator))
            .then_with(|| left.gross.cmp(&right.gross))
            .then_with(|| left.commission.cmp(&right.commission))
            .then_with(|| left.net.cmp(&right.net))
    });
}

fn total_compounded(compoundings: &[RewardCompounding]) -> Result<i64, StakingError> {
    compoundings.iter().try_fold(0i64, |total, entry| {
        total
            .checked_add(entry.amount)
            .ok_or(StakingError::RewardAggregateOverflow)
    })
}

impl StakingState {
    /// Return the deterministic annual reward curve for aggregate stake.
    ///
    /// The annual payout is
    ///
    /// `237 bps * sqrt(400,000,000 HYCK * total_staked)`.
    ///
    /// Both operands are base-unit amounts, so at exactly 400M staked the
    /// result is exactly 2.37% annual.  Integer square root and checked
    /// arithmetic avoid floating point and overflow-dependent consensus.
    pub fn reward_curve_for_stake(total_staked: i64) -> Result<RewardCurve, StakingError> {
        if total_staked < 0 {
            return Err(StakingError::InvalidValidatorStake);
        }
        if total_staked == 0 {
            return Ok(RewardCurve {
                annual_reward: 0,
                apy_bps: 0,
            });
        }

        let anchor = u128::try_from(STAKING_REWARD_ANCHOR_STAKE)
            .map_err(|_| StakingError::InvalidValidatorStake)?;
        let stake =
            u128::try_from(total_staked).map_err(|_| StakingError::InvalidValidatorStake)?;
        let product = anchor
            .checked_mul(stake)
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let root = integer_sqrt(product);
        let annual_scaled = root
            .checked_mul(u128::from(STAKING_REWARD_APY_BPS))
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let annual_reward = annual_scaled
            .checked_div(u128::from(STAKING_REWARD_BPS_DENOMINATOR))
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let annual_reward =
            i64::try_from(annual_reward).map_err(|_| StakingError::RewardAggregateOverflow)?;
        let apy_bps = u128::try_from(annual_reward)
            .map_err(|_| StakingError::RewardAggregateOverflow)?
            .checked_mul(u128::from(STAKING_REWARD_BPS_DENOMINATOR))
            .and_then(|value| value.checked_div(stake))
            .and_then(|value| i64::try_from(value).ok())
            .ok_or(StakingError::RewardAggregateOverflow)?;

        Ok(RewardCurve {
            annual_reward,
            apy_bps,
        })
    }

    /// Return the annual reward in base units for aggregate stake.
    pub fn annual_reward_for_stake(total_staked: i64) -> Result<i64, StakingError> {
        Self::reward_curve_for_stake(total_staked).map(|curve| curve.annual_reward)
    }

    /// Calculate pending rewards for a validator (excluding delegator share).
    pub fn validator_pending_rewards(&self, operator: &Address) -> i64 {
        self.validators
            .get(operator)
            .map(|v| v.pending_rewards)
            .unwrap_or(0)
    }

    /// Calculate pending rewards for a delegation.
    pub fn delegation_pending_rewards(&self, delegator: &Address, validator: &Address) -> i64 {
        self.delegations
            .get(&(delegator.clone(), validator.clone()))
            .map(|d| d.pending_rewards)
            .unwrap_or(0)
    }

    /// Claim validator rewards.
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

    /// Claim delegation rewards.
    pub fn claim_delegation_rewards(
        &mut self,
        delegator: &Address,
        validator: &Address,
    ) -> Result<RewardClaimResult, StakingError> {
        let key = (delegator.clone(), validator.clone());
        let delegation = self
            .delegations
            .get_mut(&key)
            .ok_or(StakingError::DelegationNotFound)?;

        let amount = delegation.pending_rewards;
        delegation.pending_rewards = 0;
        let remove_claim_record = delegation.amount == 0;
        if remove_claim_record {
            self.delegations.remove(&key);
        }

        Ok(RewardClaimResult {
            claimant: delegator.clone(),
            validator: Some(validator.clone()),
            amount,
        })
    }

    /// Accrue and distribute rewards through the explicit reserve.
    ///
    /// This method is safe to call at every block.  `last_reward_accrual_timestamp`
    /// and the numerator remainder make the result independent of whether the
    /// caller invokes it once per block or once per epoch.  A timestamp
    /// regression returns an error before mutating any reward state.
    pub fn accrue_rewards_at(
        &mut self,
        timestamp: u64,
    ) -> Result<RewardAccrualResult, StakingError> {
        if timestamp < self.last_reward_compound_timestamp {
            return Err(StakingError::RewardTimestampRegression);
        }
        if !self.reward_clock_initialized {
            self.last_reward_accrual_timestamp = timestamp;
            self.last_reward_compound_timestamp = timestamp;
            self.reward_clock_initialized = true;
            self.reward_accrual_remainder = 0;
            return Ok(RewardAccrualResult::default());
        }
        if timestamp < self.last_reward_accrual_timestamp {
            return Err(StakingError::RewardTimestampRegression);
        }

        let elapsed = timestamp - self.last_reward_accrual_timestamp;
        let completed_epochs = elapsed / STAKING_REWARD_EPOCH_MS;
        if completed_epochs == 0 {
            let compoundings = self.maybe_auto_compound_rewards_detail(timestamp)?;
            let compounded = total_compounded(&compoundings)?;
            return Ok(RewardAccrualResult {
                auto_compounded: compounded,
                compoundings,
                ..RewardAccrualResult::default()
            });
        }

        let accrual_elapsed = completed_epochs
            .checked_mul(STAKING_REWARD_EPOCH_MS)
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let processed_timestamp = self
            .last_reward_accrual_timestamp
            .checked_add(accrual_elapsed)
            .ok_or(StakingError::RewardAggregateOverflow)?;

        let active = self.active_reward_validators()?;
        let eligible_total = active.iter().try_fold(0i64, |total, (_, _, stake)| {
            total
                .checked_add(*stake)
                .ok_or(StakingError::StakeAggregateOverflow)
        })?;
        // Rewardless intervals still advance the reward clock.  In particular,
        // a validator becoming active later must not claim an old inactive
        // interval.
        if active.is_empty() {
            self.last_reward_accrual_timestamp = processed_timestamp;
            self.reward_accrual_remainder = 0;
            self.reset_reward_eligibility();
            let compoundings = self.maybe_auto_compound_rewards_detail(timestamp)?;
            let compounded = total_compounded(&compoundings)?;
            return Ok(RewardAccrualResult {
                auto_compounded: compounded,
                compoundings,
                ..RewardAccrualResult::default()
            });
        }

        let curve = Self::reward_curve_for_stake(eligible_total)?;
        let numerator = u128::try_from(curve.annual_reward)
            .map_err(|_| StakingError::RewardAggregateOverflow)?
            .checked_mul(u128::from(accrual_elapsed))
            .and_then(|value| value.checked_add(u128::from(self.reward_accrual_remainder)))
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let denominator = u128::from(STAKING_REWARD_YEAR_MS);
        let generated = numerator / denominator;
        let remainder = u64::try_from(numerator % denominator)
            .map_err(|_| StakingError::RewardAggregateOverflow)?;
        let generated =
            i64::try_from(generated).map_err(|_| StakingError::RewardAggregateOverflow)?;
        if generated <= 0 || self.emissions_reserve <= 0 {
            self.last_reward_accrual_timestamp = processed_timestamp;
            self.reward_accrual_remainder = remainder;
            self.reset_reward_eligibility();
            let compoundings = self.maybe_auto_compound_rewards_detail(timestamp)?;
            let compounded = total_compounded(&compoundings)?;
            return Ok(RewardAccrualResult {
                auto_compounded: compounded,
                compoundings,
                ..RewardAccrualResult::default()
            });
        }

        // Exhaustion is a hard upper bound: no reward is created once the
        // reserve reaches zero.  Unpaid theoretical emission is not retained
        // as an IOU because it was never funded.
        let payout = generated.min(self.emissions_reserve);
        let credits = self.plan_distribution(payout, &active)?;
        self.validate_credit_plan(&credits, payout)?;

        self.apply_credit_plan(&credits)?;
        self.emissions_reserve = self
            .emissions_reserve
            .checked_sub(payout)
            .ok_or(StakingError::InvalidRewardAmount)?;
        self.last_reward_accrual_timestamp = processed_timestamp;
        self.reward_accrual_remainder = remainder;
        self.reset_reward_eligibility();

        let compound_details = self.maybe_auto_compound_rewards_detail(timestamp)?;
        let compounded = total_compounded(&compound_details)?;
        let mut reward_credits: Vec<_> = credits.iter().map(RewardCreditPlan::record).collect();
        sort_reward_credits(&mut reward_credits);
        let mut compound_remaining: HashMap<(Address, Address), i64> = HashMap::new();
        for detail in &compound_details {
            let entry = compound_remaining
                .entry((detail.recipient.clone(), detail.validator.clone()))
                .or_insert(0);
            *entry = entry
                .checked_add(detail.amount)
                .ok_or(StakingError::RewardAggregateOverflow)?;
        }
        for credit in &mut reward_credits {
            // A validator's self and commission rows share one pending bucket;
            // consume its deterministic compound amount in row sort order.
            let key = (credit.recipient.clone(), credit.validator.clone());
            let available = compound_remaining.entry(key).or_insert(0);
            let compounded = (*available).min(credit.net).max(0);
            credit.compounded = compounded;
            *available -= compounded;
        }

        Ok(RewardAccrualResult {
            validator_rewards: active
                .iter()
                .zip(proportional_allocations(
                    payout,
                    &active
                        .iter()
                        .map(|v| (v.0.clone(), v.2))
                        .collect::<Vec<_>>(),
                )?)
                .filter_map(|(validator, amount)| {
                    i64::try_from(amount)
                        .ok()
                        .filter(|amount| *amount > 0)
                        .map(|amount| (validator.1, amount))
                })
                .collect(),
            credits: reward_credits,
            compoundings: compound_details,
            total_distributed: payout,
            auto_compounded: compounded,
        })
    }

    /// Short alias used by application callers at block execution time.
    pub fn accrue_rewards(&mut self, timestamp: u64) -> Result<RewardAccrualResult, StakingError> {
        self.accrue_rewards_at(timestamp)
    }

    /// Auto-compound all safe pending rewards into bonded stake.
    ///
    /// A zero-amount delegation retained solely for a post-undelegation claim
    /// is intentionally not compounded.  Overflow or tombstoned records stay
    /// pending and remain claimable.
    fn auto_compound_rewards_detail_at(
        &mut self,
        timestamp: u64,
    ) -> Result<Vec<RewardCompounding>, StakingError> {
        if timestamp < self.last_reward_compound_timestamp {
            return Err(StakingError::RewardTimestampRegression);
        }

        let validator_pending: Vec<_> = self
            .validators
            .iter()
            .filter(|(_, validator)| {
                validator.pending_rewards > 0 && validator.status != ValidatorStatus::Tombstoned
            })
            .map(|(operator, validator)| (operator.clone(), validator.pending_rewards))
            .collect();

        let delegation_pending: Vec<_> =
            self.delegations
                .iter()
                .filter(|((_, operator), delegation)| {
                    delegation.amount > 0
                        && delegation.pending_rewards > 0
                        && self.validators.get(operator).is_some_and(|validator| {
                            validator.status != ValidatorStatus::Tombstoned
                        })
                })
                .map(|(key, delegation)| (key.clone(), delegation.pending_rewards))
                .collect();
        // Build a complete mutation plan first.  Entries that would overflow
        // their stake bucket are simply left pending; no earlier successful
        // compound may be committed before a later check fails.
        let mut projected_global = self.total_staked;
        let mut projected_validator_totals: HashMap<Address, i64> = self
            .validators
            .iter()
            .map(|(operator, validator)| (operator.clone(), validator.total_stake))
            .collect();
        let mut validator_plan = Vec::new();
        for (operator, pending) in &validator_pending {
            let Some(validator) = self.validators.get(operator) else {
                continue;
            };
            let Some(new_self_stake) = validator.self_stake.checked_add(*pending) else {
                continue;
            };
            let Some(new_total_stake) = projected_validator_totals
                .get(operator)
                .copied()
                .and_then(|total| total.checked_add(*pending))
            else {
                continue;
            };
            let Some(new_global_stake) = projected_global.checked_add(*pending) else {
                continue;
            };
            projected_global = new_global_stake;
            projected_validator_totals.insert(operator.clone(), new_total_stake);
            validator_plan.push((operator.clone(), *pending, new_self_stake, new_total_stake));
        }

        let mut delegation_plan = Vec::new();
        for ((delegator, operator), pending) in &delegation_pending {
            let key = (delegator.clone(), operator.clone());
            let Some(delegation) = self.delegations.get(&key) else {
                continue;
            };
            let current_amount = delegation.amount;
            let Some(new_amount) = current_amount.checked_add(*pending) else {
                continue;
            };
            let Some(new_validator_total) = projected_validator_totals
                .get(operator)
                .copied()
                .and_then(|total| total.checked_add(*pending))
            else {
                continue;
            };
            let Some(new_global_stake) = projected_global.checked_add(*pending) else {
                continue;
            };
            projected_global = new_global_stake;
            projected_validator_totals.insert(operator.clone(), new_validator_total);
            delegation_plan.push((key, *pending, new_amount, new_validator_total));
        }

        let mut compounded_details: Vec<_> = validator_plan
            .iter()
            .map(|(operator, pending, _, _)| RewardCompounding {
                recipient_type: RewardCompoundingRecipientType::Validator,
                recipient: operator.clone(),
                validator: operator.clone(),
                amount: *pending,
            })
            .chain(
                delegation_plan
                    .iter()
                    .map(|((delegator, operator), pending, _, _)| RewardCompounding {
                        recipient_type: RewardCompoundingRecipientType::Delegator,
                        recipient: delegator.clone(),
                        validator: operator.clone(),
                        amount: *pending,
                    }),
            )
            .collect();
        compounded_details.sort_by(|left, right| {
            left.recipient_type
                .cmp(&right.recipient_type)
                .then_with(|| left.recipient.cmp(&right.recipient))
                .then_with(|| left.validator.cmp(&right.validator))
                .then_with(|| left.amount.cmp(&right.amount))
        });

        for (operator, _pending, new_self_stake, new_total_stake) in validator_plan {
            let validator = self
                .validators
                .get_mut(&operator)
                .ok_or(StakingError::ValidatorNotFound)?;
            validator.self_stake = new_self_stake;
            validator.total_stake = new_total_stake;
            validator.pending_rewards = 0;
        }
        for ((delegator, operator), _pending, new_amount, new_validator_total) in delegation_plan {
            let delegation = self
                .delegations
                .get_mut(&(delegator, operator.clone()))
                .ok_or(StakingError::DelegationNotFound)?;
            delegation.amount = new_amount;
            delegation.pending_rewards = 0;
            self.validators
                .get_mut(&operator)
                .ok_or(StakingError::ValidatorNotFound)?
                .total_stake = new_validator_total;
        }

        self.total_staked = projected_global;
        self.last_reward_compound_timestamp = timestamp;
        Ok(compounded_details)
    }

    /// Auto-compound all safe pending rewards into bonded stake.
    pub fn auto_compound_rewards_at(&mut self, timestamp: u64) -> Result<i64, StakingError> {
        let compoundings = self.auto_compound_rewards_detail_at(timestamp)?;
        total_compounded(&compoundings)
    }

    fn maybe_auto_compound_rewards_detail(
        &mut self,
        timestamp: u64,
    ) -> Result<Vec<RewardCompounding>, StakingError> {
        if timestamp.saturating_sub(self.last_reward_compound_timestamp)
            >= STAKING_AUTO_COMPOUND_INTERVAL_MS
        {
            return self.auto_compound_rewards_detail_at(timestamp);
        }
        Ok(Vec::new())
    }

    /// Distribute rewards that were manually placed in validator pending
    /// balances.  The normal accrual path plans the same split directly; this
    /// compatibility method remains useful for migration and tests.
    pub fn distribute_delegator_rewards(&mut self) {
        let validators: Vec<_> = self
            .validators
            .iter()
            .filter(|(_, validator)| validator.pending_rewards > 0)
            .map(|(operator, validator)| {
                (
                    operator.clone(),
                    validator.node_id,
                    validator.pending_rewards,
                )
            })
            .collect();

        for (operator, node_id, amount) in validators {
            let Ok(credits) = self
                .plan_distribution_for_validators(&[(operator.clone(), node_id, amount)], false)
            else {
                continue;
            };
            if self.validate_credit_plan(&credits, amount).is_err() {
                continue;
            }
            if let Some(validator) = self.validators.get_mut(&operator) {
                validator.pending_rewards = 0;
            }
            if self.apply_credit_plan(&credits).is_err() {
                // The plan was preflighted, so this is unreachable unless a
                // caller concurrently mutates the state (which Rust prevents).
                break;
            }
        }
    }

    /// Get estimated APY for a validator (in basis points).
    pub fn estimated_apy(&self, _operator: &Address) -> i64 {
        if self.emissions_reserve == 0 {
            return 0;
        }
        Self::reward_curve_for_stake(self.total_staked)
            .map(|curve| curve.apy_bps)
            .unwrap_or(0)
    }

    /// Get total rewards currently held in pending balances.
    pub fn total_rewards_distributed(&self) -> i64 {
        self.validators
            .values()
            .map(|v| v.pending_rewards)
            .chain(self.delegations.values().map(|d| d.pending_rewards))
            .try_fold(0i64, |total, reward| total.checked_add(reward))
            .unwrap_or(i64::MAX)
    }

    fn active_reward_validators(&self) -> Result<Vec<(Address, NodeId, i64)>, StakingError> {
        let mut active = Vec::new();
        for (operator, validator) in self.validators.iter() {
            if validator.status != ValidatorStatus::Active {
                continue;
            }
            let self_eligible = validator.reward_eligible_stake.min(validator.self_stake);
            let delegated_eligible = self
                .delegations
                .iter()
                .filter(|((_, validator_key), delegation)| {
                    validator_key == operator && delegation.reward_eligible_stake > 0
                })
                .try_fold(0i64, |total, (_, delegation)| {
                    total
                        .checked_add(delegation.reward_eligible_stake)
                        .ok_or(StakingError::StakeAggregateOverflow)
                })?;
            let weight = self_eligible
                .checked_add(delegated_eligible)
                .ok_or(StakingError::StakeAggregateOverflow)?;
            if weight > 0 {
                active.push((operator.clone(), validator.node_id, weight));
            }
        }
        active.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(active)
    }

    fn reset_reward_eligibility(&mut self) {
        for validator in self.validators.values_mut() {
            validator.reward_eligible_stake = validator.self_stake.max(0);
        }
        for delegation in self.delegations.values_mut() {
            delegation.reward_eligible_stake = delegation.amount.max(0);
        }
    }

    fn plan_distribution(
        &self,
        total_reward: i64,
        active: &[(Address, NodeId, i64)],
    ) -> Result<Vec<RewardCreditPlan>, StakingError> {
        let weights: Vec<_> = active
            .iter()
            .map(|(operator, _, stake)| (operator.clone(), *stake))
            .collect();
        let allocations = proportional_allocations(total_reward, &weights)?;
        let validators: Vec<_> = active
            .iter()
            .zip(allocations)
            .filter_map(|((operator, node_id, _), amount)| {
                i64::try_from(amount)
                    .ok()
                    .filter(|amount| *amount > 0)
                    .map(|amount| (operator.clone(), *node_id, amount))
            })
            .collect();
        self.plan_distribution_for_validators(&validators, true)
    }

    fn plan_distribution_for_validators(
        &self,
        validators: &[(Address, NodeId, i64)],
        use_eligible_stake: bool,
    ) -> Result<Vec<RewardCreditPlan>, StakingError> {
        let mut credits = Vec::new();
        for (operator, _node_id, total_reward) in validators {
            let validator = self
                .validators
                .get(operator)
                .ok_or(StakingError::ValidatorNotFound)?;
            if *total_reward <= 0 {
                continue;
            }

            let delegations: Vec<_> = self
                .delegations
                .iter()
                .filter(|((_, validator_key), delegation)| {
                    validator_key == operator
                        && if use_eligible_stake {
                            delegation.reward_eligible_stake > 0
                        } else {
                            delegation.amount > 0
                        }
                })
                .map(|((delegator, _), delegation)| {
                    (
                        delegator.clone(),
                        if use_eligible_stake {
                            delegation.reward_eligible_stake
                        } else {
                            delegation.amount
                        },
                    )
                })
                .collect();
            let total_delegated = delegations.iter().try_fold(0i64, |total, (_, amount)| {
                total
                    .checked_add(*amount)
                    .ok_or(StakingError::StakeAggregateOverflow)
            })?;
            let total_stake = if use_eligible_stake {
                validator
                    .reward_eligible_stake
                    .min(validator.self_stake)
                    .checked_add(total_delegated)
                    .ok_or(StakingError::StakeAggregateOverflow)?
            } else {
                validator.total_stake
            };
            if total_stake <= 0 {
                return Err(StakingError::InvalidValidatorStake);
            }

            let total_reward_u128 =
                u128::try_from(*total_reward).map_err(|_| StakingError::InvalidRewardAmount)?;
            let total_delegated_u128 = u128::try_from(total_delegated)
                .map_err(|_| StakingError::StakeAggregateOverflow)?;
            let total_stake_u128 =
                u128::try_from(total_stake).map_err(|_| StakingError::StakeAggregateOverflow)?;
            let delegated_gross = total_reward_u128
                .checked_mul(total_delegated_u128)
                .and_then(|value| value.checked_div(total_stake_u128))
                .ok_or(StakingError::RewardAggregateOverflow)?;
            let delegated_gross = i64::try_from(delegated_gross)
                .map_err(|_| StakingError::RewardAggregateOverflow)?;
            let validator_self_share = total_reward
                .checked_sub(delegated_gross)
                .ok_or(StakingError::RewardAggregateOverflow)?;
            let commission = u128::try_from(delegated_gross)
                .map_err(|_| StakingError::RewardAggregateOverflow)?
                .checked_mul(
                    u128::try_from(validator.commission_bps)
                        .map_err(|_| StakingError::InvalidCommission)?,
                )
                .and_then(|value| value.checked_div(u128::from(STAKING_REWARD_BPS_DENOMINATOR)))
                .ok_or(StakingError::RewardAggregateOverflow)?;
            let commission =
                i64::try_from(commission).map_err(|_| StakingError::RewardAggregateOverflow)?;
            if validator_self_share > 0 {
                credits.push(RewardCreditPlan::ValidatorSelf {
                    operator: operator.clone(),
                    gross: validator_self_share,
                });
            }
            if commission > 0 {
                credits.push(RewardCreditPlan::ValidatorCommission {
                    operator: operator.clone(),
                    gross: commission,
                });
            }

            let delegator_net = delegated_gross
                .checked_sub(commission)
                .ok_or(StakingError::RewardAggregateOverflow)?;
            if delegator_net == 0 {
                continue;
            }
            if delegations.is_empty() {
                return Err(StakingError::DelegationNotFound);
            }

            // Allocate net and commission independently with the same stable
            // largest-remainder rule.  Gross is reconstructed as net plus the
            // corresponding commission, preserving exact conservation even
            // when integer dust lands on different delegators.
            let net_allocations = proportional_allocations(delegator_net, &delegations)?;
            let commission_allocations = proportional_allocations(commission, &delegations)?;
            for (((delegator, _), net), commission) in delegations
                .iter()
                .zip(net_allocations)
                .zip(commission_allocations)
            {
                let net = i64::try_from(net).map_err(|_| StakingError::RewardAggregateOverflow)?;
                let commission =
                    i64::try_from(commission).map_err(|_| StakingError::RewardAggregateOverflow)?;
                if net > 0 || commission > 0 {
                    let gross = net
                        .checked_add(commission)
                        .ok_or(StakingError::RewardAggregateOverflow)?;
                    credits.push(RewardCreditPlan::Delegation {
                        delegator: delegator.clone(),
                        validator: operator.clone(),
                        gross,
                        commission,
                        net,
                    });
                }
            }
        }
        Ok(credits)
    }

    fn validate_credit_plan(
        &self,
        credits: &[RewardCreditPlan],
        expected: i64,
    ) -> Result<(), StakingError> {
        let mut sum = 0i64;
        let mut validator_deltas: HashMap<&Address, i64> = HashMap::new();
        let mut delegation_deltas: HashMap<(&Address, &Address), i64> = HashMap::new();
        for credit in credits {
            let amount = match credit {
                RewardCreditPlan::ValidatorSelf { operator, gross }
                | RewardCreditPlan::ValidatorCommission { operator, gross } => {
                    let delta = validator_deltas.entry(operator).or_insert(0);
                    *delta = delta
                        .checked_add(*gross)
                        .ok_or(StakingError::RewardAggregateOverflow)?;
                    *gross
                }
                RewardCreditPlan::Delegation {
                    delegator,
                    validator,
                    net,
                    ..
                } => {
                    let delta = delegation_deltas.entry((delegator, validator)).or_insert(0);
                    *delta = delta
                        .checked_add(*net)
                        .ok_or(StakingError::RewardAggregateOverflow)?;
                    *net
                }
            };
            sum = sum
                .checked_add(amount)
                .ok_or(StakingError::RewardAggregateOverflow)?;
        }
        for (operator, delta) in validator_deltas {
            self.validators
                .get(operator)
                .ok_or(StakingError::ValidatorNotFound)?
                .pending_rewards
                .checked_add(delta)
                .ok_or(StakingError::RewardAggregateOverflow)?;
        }
        for ((delegator, validator), delta) in delegation_deltas {
            self.delegations
                .get(&(delegator.clone(), validator.clone()))
                .ok_or(StakingError::DelegationNotFound)?
                .pending_rewards
                .checked_add(delta)
                .ok_or(StakingError::RewardAggregateOverflow)?;
        }
        if sum != expected {
            return Err(StakingError::RewardAggregateOverflow);
        }
        Ok(())
    }

    fn apply_credit_plan(&mut self, credits: &[RewardCreditPlan]) -> Result<(), StakingError> {
        for credit in credits {
            match credit {
                RewardCreditPlan::ValidatorSelf { operator, gross }
                | RewardCreditPlan::ValidatorCommission { operator, gross } => {
                    let validator = self
                        .validators
                        .get_mut(operator)
                        .ok_or(StakingError::ValidatorNotFound)?;
                    validator.pending_rewards = validator
                        .pending_rewards
                        .checked_add(*gross)
                        .ok_or(StakingError::RewardAggregateOverflow)?;
                }
                RewardCreditPlan::Delegation {
                    delegator,
                    validator,
                    net,
                    ..
                } => {
                    let delegation = self
                        .delegations
                        .get_mut(&(delegator.clone(), validator.clone()))
                        .ok_or(StakingError::DelegationNotFound)?;
                    delegation.pending_rewards = delegation
                        .pending_rewards
                        .checked_add(*net)
                        .ok_or(StakingError::RewardAggregateOverflow)?;
                }
            }
        }
        Ok(())
    }
}

/// Deterministically split `total` in proportion to positive integer weights.
/// Every unit is assigned exactly once; largest remainder ties are resolved by
/// the already-sorted stable key in `weights`.
fn proportional_allocations(
    total: i64,
    weights: &[(Address, i64)],
) -> Result<Vec<u128>, StakingError> {
    if total < 0 {
        return Err(StakingError::InvalidRewardAmount);
    }
    if weights.is_empty() {
        return Ok(Vec::new());
    }
    let total_weight = weights.iter().try_fold(0u128, |sum, (_, weight)| {
        if *weight <= 0 {
            return Err(StakingError::InvalidValidatorStake);
        }
        sum.checked_add(u128::try_from(*weight).map_err(|_| StakingError::InvalidValidatorStake)?)
            .ok_or(StakingError::StakeAggregateOverflow)
    })?;
    if total_weight == 0 {
        return Err(StakingError::InvalidValidatorStake);
    }

    let total = u128::try_from(total).map_err(|_| StakingError::InvalidRewardAmount)?;
    let mut allocations = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut assigned = 0u128;
    for (_, weight) in weights {
        let numerator = total
            .checked_mul(u128::try_from(*weight).map_err(|_| StakingError::InvalidValidatorStake)?)
            .ok_or(StakingError::RewardAggregateOverflow)?;
        let base = numerator / total_weight;
        allocations.push(base);
        remainders.push(numerator % total_weight);
        assigned = assigned
            .checked_add(base)
            .ok_or(StakingError::RewardAggregateOverflow)?;
    }

    let mut dust = total
        .checked_sub(assigned)
        .ok_or(StakingError::RewardAggregateOverflow)?;
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|left, right| {
        remainders[*right]
            .cmp(&remainders[*left])
            .then_with(|| weights[*left].0.cmp(&weights[*right].0))
    });
    let mut index = 0usize;
    while dust > 0 {
        allocations[order[index]] = allocations[order[index]]
            .checked_add(1)
            .ok_or(StakingError::RewardAggregateOverflow)?;
        dust -= 1;
        index += 1;
        if index == order.len() {
            index = 0;
        }
    }
    Ok(allocations)
}

/// Exact floor square root for a `u128` without floating point.
fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let mut low = 1u128;
    let mut high = 1u128 << 64;
    while low + 1 < high {
        let mid = (low + high) / 2;
        if mid <= value / mid {
            low = mid;
        } else {
            high = mid;
        }
    }
    low
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::staking::types::{
        HYCK_BASE_UNITS_PER_HYCK, HYCK_GENESIS_EMISSIONS_RESERVE, MIN_SELF_STAKE,
        STAKING_REWARD_ANCHOR_STAKE, STAKING_REWARD_APY_BPS, STAKING_REWARD_EPOCH_MS,
    };
    use crate::crypto::bls::BlsSecretKey;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key(n: u8) -> Vec<u8> {
        let mut seed = [0u8; 32];
        seed[0] = n;
        BlsSecretKey::from_seed(&seed)
            .public_key()
            .to_bytes()
            .to_vec()
    }

    fn test_bls_proof(n: u8) -> Vec<u8> {
        let mut seed = [0u8; 32];
        seed[0] = n;
        let node_id = test_node_id(n);
        BlsSecretKey::from_seed(&seed)
            .create_proof_of_possession(&[0u8; 32], &node_id)
            .to_bytes()
            .to_vec()
    }

    fn active_validator(
        state: &mut StakingState,
        operator: &str,
        node: u8,
        stake: i64,
        commission_bps: i64,
    ) {
        state
            .register_validator(
                operator.to_string(),
                test_node_id(node),
                test_bls_key(node),
                test_bls_proof(node),
                [0u8; 32],
                stake,
                commission_bps,
            )
            .unwrap();
        state.validators.get_mut(operator).unwrap().status = ValidatorStatus::Active;
    }

    #[test]
    fn inverse_sqrt_curve_is_exact_at_anchor_without_float() {
        let curve = StakingState::reward_curve_for_stake(STAKING_REWARD_ANCHOR_STAKE).unwrap();
        assert_eq!(curve.apy_bps, STAKING_REWARD_APY_BPS as i64);
        assert_eq!(
            curve.annual_reward,
            STAKING_REWARD_ANCHOR_STAKE * STAKING_REWARD_APY_BPS as i64 / 10_000
        );
    }

    #[test]
    fn reserve_bounds_payout_and_never_uses_legacy_pool() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state.set_emissions_reserve(1).unwrap();
        // First call establishes the explicit genesis reward clock anchor.
        assert!(state
            .accrue_rewards_at(0)
            .unwrap()
            .validator_rewards
            .is_empty());
        let result = state.accrue_rewards_at(STAKING_REWARD_YEAR_MS).unwrap();
        assert_eq!(
            result
                .validator_rewards
                .iter()
                .map(|(_, amount)| *amount)
                .sum::<i64>(),
            1
        );
        assert_eq!(state.emissions_reserve, 0);

        let pending_after_exhaustion = state.total_rewards_distributed();
        state.accrue_rewards_at(STAKING_REWARD_YEAR_MS * 2).unwrap();
        assert_eq!(state.total_rewards_distributed(), pending_after_exhaustion);
    }

    #[test]
    fn proportional_allocation_conserves_every_base_unit_deterministically() {
        let weights = vec![("a".to_string(), 2), ("b".to_string(), 1)];
        let allocations = proportional_allocations(5, &weights).unwrap();
        assert_eq!(allocations.iter().sum::<u128>(), 5);
        assert_eq!(allocations, vec![3, 2]);
        assert_eq!(proportional_allocations(5, &weights).unwrap(), allocations);
    }

    #[test]
    fn commission_and_delegator_shares_conserve_reward() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 1_000);
        state
            .delegate("alice".into(), "v1".into(), MIN_SELF_STAKE)
            .unwrap();
        let active = state.active_reward_validators().unwrap();
        let credits = state.plan_distribution(10_001, &active).unwrap();
        state.validate_credit_plan(&credits, 10_001).unwrap();
        state.apply_credit_plan(&credits).unwrap();
        assert_eq!(state.total_rewards_distributed(), 10_001);
        assert!(state.validator_pending_rewards(&"v1".into()) > 0);
        assert!(state.delegation_pending_rewards(&"alice".into(), &"v1".into()) > 0);
    }

    #[test]
    fn canonical_credits_record_commission_and_delegator_conservation() {
        let mut state = StakingState::new();
        let stake = STAKING_REWARD_ANCHOR_STAKE / 2;
        active_validator(&mut state, "v1", 1, stake, 1_000);
        state.delegate("alice".into(), "v1".into(), stake).unwrap();
        state
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        state.accrue_rewards_at(0).unwrap();
        let result = state.accrue_rewards_at(STAKING_REWARD_EPOCH_MS).unwrap();

        assert!(result.total_distributed > 0);
        assert_eq!(
            result.credits.iter().map(|credit| credit.net).sum::<i64>(),
            result.total_distributed
        );
        let commission = result
            .credits
            .iter()
            .find(|credit| credit.recipient_type == RewardRecipientType::ValidatorCommission)
            .unwrap();
        let delegator = result
            .credits
            .iter()
            .find(|credit| credit.recipient_type == RewardRecipientType::Delegator)
            .unwrap();
        assert_eq!(delegator.gross, delegator.net + delegator.commission);
        assert_eq!(commission.net, delegator.commission);
        assert!(result
            .credits
            .iter()
            .any(|credit| credit.recipient_type == RewardRecipientType::ValidatorSelf));
    }

    #[test]
    fn canonical_credit_serialization_is_sorted_and_deterministic() {
        fn settle(reverse: bool) -> Vec<RewardCredit> {
            let mut state = StakingState::new();
            let stake = STAKING_REWARD_ANCHOR_STAKE / 2;
            active_validator(&mut state, "v1", 1, stake, 1_000);
            active_validator(&mut state, "v2", 2, stake, 0);
            let delegations = if reverse {
                vec![("bob", "v2"), ("alice", "v1")]
            } else {
                vec![("alice", "v1"), ("bob", "v2")]
            };
            for (delegator, validator) in delegations {
                state
                    .delegate(delegator.to_string(), validator.to_string(), stake)
                    .unwrap();
            }
            state
                .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
                .unwrap();
            state.accrue_rewards_at(0).unwrap();
            state
                .accrue_rewards_at(STAKING_REWARD_EPOCH_MS)
                .unwrap()
                .credits
        }

        let first = settle(false);
        let second = settle(true);
        assert_eq!(first, second);
        assert!(first.windows(2).all(|pair| {
            (pair[0].recipient_type.sort_rank(), &pair[0].recipient)
                <= (pair[1].recipient_type.sort_rank(), &pair[1].recipient)
        }));
        assert_eq!(
            bincode::serialize(&first).unwrap(),
            bincode::serialize(&second).unwrap()
        );
    }

    #[test]
    fn compound_only_result_records_every_recipient() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state
            .delegate("alice".into(), "v1".into(), MIN_SELF_STAKE)
            .unwrap();
        state.validators.get_mut("v1").unwrap().pending_rewards = 123;
        state
            .delegations
            .get_mut(&("alice".into(), "v1".into()))
            .unwrap()
            .pending_rewards = 77;

        state.accrue_rewards_at(0).unwrap();
        let result = state
            .accrue_rewards_at(STAKING_AUTO_COMPOUND_INTERVAL_MS)
            .unwrap();

        assert_eq!(result.total_distributed, 0);
        assert!(result.credits.is_empty());
        assert_eq!(result.auto_compounded, 200);
        assert_eq!(
            result.compoundings,
            vec![
                RewardCompounding {
                    recipient_type: RewardCompoundingRecipientType::Validator,
                    recipient: "v1".into(),
                    validator: "v1".into(),
                    amount: 123,
                },
                RewardCompounding {
                    recipient_type: RewardCompoundingRecipientType::Delegator,
                    recipient: "alice".into(),
                    validator: "v1".into(),
                    amount: 77,
                },
            ]
        );
    }

    #[test]
    fn aggregate_validator_credit_overflow_is_rejected_before_apply() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 1_000);
        state.validators.get_mut("v1").unwrap().pending_rewards = i64::MAX - 5;
        let credits = vec![
            RewardCreditPlan::ValidatorSelf {
                operator: "v1".into(),
                gross: 4,
            },
            RewardCreditPlan::ValidatorCommission {
                operator: "v1".into(),
                gross: 4,
            },
        ];

        assert!(matches!(
            state.validate_credit_plan(&credits, 8),
            Err(StakingError::RewardAggregateOverflow)
        ));
        assert_eq!(
            state.validators.get("v1").unwrap().pending_rewards,
            i64::MAX - 5
        );
    }

    #[test]
    fn full_undelegate_retains_pending_reward_claim_record() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state.delegate("alice".into(), "v1".into(), 100).unwrap();
        state
            .delegations
            .get_mut(&("alice".into(), "v1".into()))
            .unwrap()
            .pending_rewards = 7;
        state
            .undelegate("alice".into(), "v1".into(), 100, 1)
            .unwrap();
        assert_eq!(
            state.delegation_pending_rewards(&"alice".into(), &"v1".into()),
            7
        );
        assert_eq!(
            state
                .delegations
                .get(&("alice".into(), "v1".into()))
                .unwrap()
                .amount,
            0
        );
        let claim = state
            .claim_delegation_rewards(&"alice".into(), &"v1".into())
            .unwrap();
        assert_eq!(claim.amount, 7);
        assert!(!state
            .delegations
            .contains_key(&("alice".into(), "v1".into())));
    }

    #[test]
    fn reward_timestamp_regression_fails_closed() {
        let mut state = StakingState::new();
        state
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        state.accrue_rewards_at(100).unwrap();
        let before = state.clone();
        assert_eq!(
            state.accrue_rewards_at(99).unwrap_err().to_string(),
            StakingError::RewardTimestampRegression.to_string()
        );
        assert_eq!(
            state.last_reward_accrual_timestamp,
            before.last_reward_accrual_timestamp
        );
        assert_eq!(state.emissions_reserve, before.emissions_reserve);
    }

    #[test]
    fn first_reward_tick_anchors_clock_and_compound_clock() {
        let mut state = StakingState::new();
        state
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        assert!(state
            .accrue_rewards_at(1_700_000_000_000)
            .unwrap()
            .validator_rewards
            .is_empty());
        assert_eq!(state.last_reward_accrual_timestamp, 1_700_000_000_000);
        assert_eq!(state.last_reward_compound_timestamp, 1_700_000_000_000);
    }

    #[test]
    fn reward_distribution_is_epoch_cadenced_and_blocks_flash_stake() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        state.accrue_rewards_at(0).unwrap();

        // Stake added during an epoch is not eligible for that epoch.
        state
            .delegate("alice".into(), "v1".into(), MIN_SELF_STAKE)
            .unwrap();
        state.accrue_rewards_at(STAKING_REWARD_EPOCH_MS).unwrap();
        assert_eq!(
            state.delegation_pending_rewards(&"alice".into(), &"v1".into()),
            0
        );
        assert!(state.validator_pending_rewards(&"v1".into()) > 0);

        // The next boundary starts with the now-current delegation stake.
        state
            .accrue_rewards_at(STAKING_REWARD_EPOCH_MS * 2)
            .unwrap();
        assert!(state.delegation_pending_rewards(&"alice".into(), &"v1".into()) > 0);
    }

    #[test]
    fn inactive_interval_is_not_paid_to_a_later_active_validator() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state.validators.get_mut("v1").unwrap().status = ValidatorStatus::Inactive;
        state
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        state.accrue_rewards_at(0).unwrap();
        state.accrue_rewards_at(STAKING_REWARD_EPOCH_MS).unwrap();
        state.validators.get_mut("v1").unwrap().status = ValidatorStatus::Active;
        state
            .accrue_rewards_at(STAKING_REWARD_EPOCH_MS * 2)
            .unwrap();
        assert!(state.validator_pending_rewards(&"v1".into()) > 0);
        let one_epoch = StakingState::annual_reward_for_stake(MIN_SELF_STAKE).unwrap() as u128
            * u128::from(STAKING_REWARD_EPOCH_MS)
            / u128::from(STAKING_REWARD_YEAR_MS);
        assert!(state.validator_pending_rewards(&"v1".into()) <= one_epoch as i64);
    }

    #[test]
    fn sub_epoch_ticks_are_constant_work_and_match_boundary_only() {
        let mut every_block = StakingState::new();
        let mut boundary_only = StakingState::new();
        active_validator(&mut every_block, "v1", 1, MIN_SELF_STAKE, 0);
        active_validator(&mut boundary_only, "v1", 1, MIN_SELF_STAKE, 0);
        every_block
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        boundary_only
            .set_emissions_reserve(HYCK_GENESIS_EMISSIONS_RESERVE)
            .unwrap();
        every_block.accrue_rewards_at(0).unwrap();
        boundary_only.accrue_rewards_at(0).unwrap();
        for timestamp in [1, STAKING_REWARD_EPOCH_MS / 3, STAKING_REWARD_EPOCH_MS - 1] {
            every_block.accrue_rewards_at(timestamp).unwrap();
        }
        every_block
            .accrue_rewards_at(STAKING_REWARD_EPOCH_MS)
            .unwrap();
        boundary_only
            .accrue_rewards_at(STAKING_REWARD_EPOCH_MS)
            .unwrap();
        assert_eq!(
            every_block.emissions_reserve,
            boundary_only.emissions_reserve
        );
        assert_eq!(
            every_block.total_rewards_distributed(),
            boundary_only.total_rewards_distributed()
        );
    }

    #[test]
    fn auto_compound_moves_safe_pending_rewards_into_stake() {
        let mut state = StakingState::new();
        active_validator(&mut state, "v1", 1, MIN_SELF_STAKE, 0);
        state.validators.get_mut("v1").unwrap().pending_rewards = 9;
        state
            .auto_compound_rewards_at(STAKING_AUTO_COMPOUND_INTERVAL_MS)
            .unwrap();
        assert_eq!(state.validator_pending_rewards(&"v1".into()), 0);
        assert_eq!(
            state.get_validator(&"v1".into()).unwrap().self_stake,
            MIN_SELF_STAKE + 9
        );
        assert_eq!(state.total_staked, MIN_SELF_STAKE + 9);
    }

    #[test]
    fn base_unit_constants_match_six_decimal_policy() {
        assert_eq!(HYCK_BASE_UNITS_PER_HYCK, 1_000_000);
    }
}
