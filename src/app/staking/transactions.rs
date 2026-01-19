//! Staking Transactions
//!
//! Transaction types and execution for staking operations.

use serde::{Deserialize, Serialize};

use super::state::{StakingError, StakingState};
use super::types::Evidence;
use crate::app::Address;
use crate::types::NodeId;

/// Staking-specific transaction types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StakingTransaction {
    /// Register a new validator
    RegisterValidator {
        operator: Address,
        node_id: NodeId,
        bls_pubkey: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    },
    /// Delegate stake to a validator
    Delegate {
        delegator: Address,
        validator: Address,
        amount: i64,
    },
    /// Undelegate stake from a validator (enters unbonding queue)
    Undelegate {
        delegator: Address,
        validator: Address,
        amount: i64,
    },
    /// Claim completed unstakes
    ClaimUnstaked {
        delegator: Address,
    },
    /// Claim staking rewards
    ClaimRewards {
        /// Claimant address
        claimant: Address,
        /// Validator to claim from (None for validator claiming own rewards)
        validator: Option<Address>,
    },
    /// Unjail a jailed validator
    Unjail {
        operator: Address,
    },
    /// Submit evidence of misbehavior
    SubmitEvidence {
        submitter: Address,
        evidence: Evidence,
    },
}

impl StakingTransaction {
    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}

/// Result of executing a staking transaction
#[derive(Debug, Clone)]
pub enum StakingTxResult {
    /// Validator registered successfully
    ValidatorRegistered { operator: Address, node_id: NodeId },
    /// Delegation added
    Delegated {
        delegator: Address,
        validator: Address,
        amount: i64,
    },
    /// Undelegation queued
    Undelegated {
        delegator: Address,
        validator: Address,
        amount: i64,
        completion_time: u64,
    },
    /// Unstaked funds claimed
    UnstakeClaimed { delegator: Address, amount: i64 },
    /// Rewards claimed
    RewardsClaimed {
        claimant: Address,
        amount: i64,
    },
    /// Validator unjailed
    Unjailed { operator: Address },
    /// Evidence submitted and processed
    EvidenceProcessed {
        offender: NodeId,
        slashed_amount: i64,
    },
}

impl StakingState {
    /// Execute a staking transaction
    pub fn execute_tx(
        &mut self,
        tx: StakingTransaction,
        timestamp: u64,
    ) -> Result<StakingTxResult, StakingError> {
        match tx {
            StakingTransaction::RegisterValidator {
                operator,
                node_id,
                bls_pubkey,
                self_stake,
                commission_bps,
            } => {
                self.register_validator(
                    operator.clone(),
                    node_id,
                    bls_pubkey,
                    self_stake,
                    commission_bps,
                )?;
                Ok(StakingTxResult::ValidatorRegistered { operator, node_id })
            }

            StakingTransaction::Delegate {
                delegator,
                validator,
                amount,
            } => {
                self.delegate(delegator.clone(), validator.clone(), amount)?;
                Ok(StakingTxResult::Delegated {
                    delegator,
                    validator,
                    amount,
                })
            }

            StakingTransaction::Undelegate {
                delegator,
                validator,
                amount,
            } => {
                self.undelegate(delegator.clone(), validator.clone(), amount, timestamp)?;
                let completion_time = timestamp + super::types::UNSTAKE_DELAY_MS;
                Ok(StakingTxResult::Undelegated {
                    delegator,
                    validator,
                    amount,
                    completion_time,
                })
            }

            StakingTransaction::ClaimUnstaked { delegator } => {
                let completions = self.process_unstake_queue(timestamp);
                let amount = completions
                    .iter()
                    .filter(|(d, _)| d == &delegator)
                    .map(|(_, a)| a)
                    .sum();
                Ok(StakingTxResult::UnstakeClaimed { delegator, amount })
            }

            StakingTransaction::ClaimRewards { claimant, validator } => {
                let amount = match validator {
                    Some(val) => {
                        let result = self.claim_delegation_rewards(&claimant, &val)?;
                        result.amount
                    }
                    None => {
                        let result = self.claim_validator_rewards(&claimant)?;
                        result.amount
                    }
                };
                Ok(StakingTxResult::RewardsClaimed { claimant, amount })
            }

            StakingTransaction::Unjail { operator } => {
                self.unjail(&operator, timestamp)?;
                Ok(StakingTxResult::Unjailed { operator })
            }

            StakingTransaction::SubmitEvidence { submitter: _, evidence } => {
                let offender = evidence.offender;
                self.submit_evidence(evidence)?;

                // Process immediately
                let results = self.process_pending_evidence();
                let slashed_amount = results
                    .iter()
                    .filter(|r| r.offender == offender)
                    .map(|r| r.total_slashed)
                    .sum();

                Ok(StakingTxResult::EvidenceProcessed {
                    offender,
                    slashed_amount,
                })
            }
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
    fn test_execute_register_validator() {
        let mut state = StakingState::new();

        let tx = StakingTransaction::RegisterValidator {
            operator: "v1".into(),
            node_id: test_node_id(1),
            bls_pubkey: test_bls_key(),
            self_stake: MIN_SELF_STAKE,
            commission_bps: 500,
        };

        let result = state.execute_tx(tx, 1000).unwrap();
        assert!(matches!(result, StakingTxResult::ValidatorRegistered { .. }));
        assert!(state.get_validator(&"v1".into()).is_some());
    }

    #[test]
    fn test_execute_delegate_undelegate() {
        let mut state = StakingState::new();

        // Register validator first
        state.execute_tx(
            StakingTransaction::RegisterValidator {
                operator: "v1".into(),
                node_id: test_node_id(1),
                bls_pubkey: test_bls_key(),
                self_stake: MIN_SELF_STAKE,
                commission_bps: 500,
            },
            1000,
        ).unwrap();

        // Delegate
        let tx = StakingTransaction::Delegate {
            delegator: "d1".into(),
            validator: "v1".into(),
            amount: 1000_00,
        };
        let result = state.execute_tx(tx, 1000).unwrap();
        assert!(matches!(result, StakingTxResult::Delegated { amount: 1000_00, .. }));

        // Undelegate
        let tx = StakingTransaction::Undelegate {
            delegator: "d1".into(),
            validator: "v1".into(),
            amount: 500_00,
        };
        let result = state.execute_tx(tx, 2000).unwrap();
        assert!(matches!(result, StakingTxResult::Undelegated { amount: 500_00, .. }));
    }

    #[test]
    fn test_serialize_deserialize() {
        let tx = StakingTransaction::Delegate {
            delegator: "d1".into(),
            validator: "v1".into(),
            amount: 1000_00,
        };

        let bytes = tx.to_bytes();
        let parsed = StakingTransaction::from_bytes(&bytes).unwrap();

        assert!(matches!(parsed, StakingTransaction::Delegate { amount: 1000_00, .. }));
    }
}
