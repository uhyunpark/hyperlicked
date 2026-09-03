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
        bls_proof_of_possession: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    },
    /// Rotate a validator's BLS key for the next epoch.
    RotateValidatorKey {
        operator: Address,
        new_bls_pubkey: Vec<u8>,
        bls_proof_of_possession: Vec<u8>,
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
    ClaimUnstaked { delegator: Address },
    /// Claim staking rewards
    ClaimRewards {
        /// Claimant address
        claimant: Address,
        /// Validator to claim from (None for validator claiming own rewards)
        validator: Option<Address>,
    },
    /// Unjail a jailed validator
    Unjail { operator: Address },
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StakingTxResult {
    /// Validator registered successfully
    ValidatorRegistered { operator: Address, node_id: NodeId },
    /// Validator key rotated for the next epoch
    ValidatorKeyRotated { operator: Address },
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
    RewardsClaimed { claimant: Address, amount: i64 },
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
        chain_domain: [u8; 32],
    ) -> Result<StakingTxResult, StakingError> {
        match tx {
            StakingTransaction::RegisterValidator {
                operator,
                node_id,
                bls_pubkey,
                bls_proof_of_possession,
                self_stake,
                commission_bps,
            } => {
                self.register_validator(
                    operator.clone(),
                    node_id,
                    bls_pubkey,
                    bls_proof_of_possession,
                    chain_domain,
                    self_stake,
                    commission_bps,
                )?;
                Ok(StakingTxResult::ValidatorRegistered { operator, node_id })
            }

            StakingTransaction::RotateValidatorKey {
                operator,
                new_bls_pubkey,
                bls_proof_of_possession,
            } => {
                self.rotate_validator_key(
                    &operator,
                    new_bls_pubkey,
                    bls_proof_of_possession,
                    chain_domain,
                )?;
                Ok(StakingTxResult::ValidatorKeyRotated { operator })
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
                let amount = self.process_unstake_queue_for(&delegator, timestamp);
                Ok(StakingTxResult::UnstakeClaimed { delegator, amount })
            }

            StakingTransaction::ClaimRewards {
                claimant,
                validator,
            } => {
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

            StakingTransaction::SubmitEvidence {
                submitter: _,
                evidence,
            } => {
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
    use crate::app::staking::types::EvidenceType;
    use crate::app::staking::types::{MIN_SELF_STAKE, UNSTAKE_DELAY_MS};
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::ConsensusContext;

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

    #[test]
    fn test_execute_register_validator() {
        let mut state = StakingState::new();

        let tx = StakingTransaction::RegisterValidator {
            operator: "v1".into(),
            node_id: test_node_id(1),
            bls_pubkey: test_bls_key(1),
            bls_proof_of_possession: test_bls_proof(1),
            self_stake: MIN_SELF_STAKE,
            commission_bps: 500,
        };

        let result = state.execute_tx(tx, 1000, [0u8; 32]).unwrap();
        assert!(matches!(
            result,
            StakingTxResult::ValidatorRegistered { .. }
        ));
        assert!(state.get_validator(&"v1".into()).is_some());
    }

    #[test]
    fn test_execute_delegate_undelegate() {
        let mut state = StakingState::new();

        // Register validator first
        state
            .execute_tx(
                StakingTransaction::RegisterValidator {
                    operator: "v1".into(),
                    node_id: test_node_id(1),
                    bls_pubkey: test_bls_key(1),
                    bls_proof_of_possession: test_bls_proof(1),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 500,
                },
                1000,
                [0u8; 32],
            )
            .unwrap();

        // Delegate
        let tx = StakingTransaction::Delegate {
            delegator: "d1".into(),
            validator: "v1".into(),
            amount: 1000_00,
        };
        let result = state.execute_tx(tx, 1000, [0u8; 32]).unwrap();
        assert!(matches!(
            result,
            StakingTxResult::Delegated {
                amount: 1000_00,
                ..
            }
        ));

        // Undelegate
        let tx = StakingTransaction::Undelegate {
            delegator: "d1".into(),
            validator: "v1".into(),
            amount: 500_00,
        };
        let result = state.execute_tx(tx, 2000, [0u8; 32]).unwrap();
        assert!(matches!(
            result,
            StakingTxResult::Undelegated { amount: 500_00, .. }
        ));
    }

    #[test]
    fn test_claim_unstaked_only_processes_requesting_delegator() {
        let mut state = StakingState::new();
        state
            .execute_tx(
                StakingTransaction::RegisterValidator {
                    operator: "v1".into(),
                    node_id: test_node_id(1),
                    bls_pubkey: test_bls_key(1),
                    bls_proof_of_possession: test_bls_proof(1),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 500,
                },
                0,
                [0u8; 32],
            )
            .unwrap();
        state.delegate("alice".into(), "v1".into(), 300).unwrap();
        state.delegate("bob".into(), "v1".into(), 300).unwrap();

        state
            .undelegate("alice".into(), "v1".into(), 100, 0)
            .unwrap();
        state
            .undelegate("alice".into(), "v1".into(), 100, 1)
            .unwrap();
        state.undelegate("bob".into(), "v1".into(), 100, 0).unwrap();

        let alice_before = state.unstake_queue.get("alice").unwrap().clone();
        let bob_before = state.unstake_queue.get("bob").unwrap().clone();
        let ready = UNSTAKE_DELAY_MS;

        let result = state
            .execute_tx(
                StakingTransaction::ClaimUnstaked {
                    delegator: "alice".into(),
                },
                ready,
                [0u8; 32],
            )
            .unwrap();
        assert!(matches!(
            result,
            StakingTxResult::UnstakeClaimed { amount: 100, .. }
        ));
        assert_eq!(
            bincode::serialize(state.unstake_queue.get("alice").unwrap()).unwrap(),
            bincode::serialize(&vec![alice_before[1].clone()]).unwrap()
        );
        assert_eq!(
            bincode::serialize(state.unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );

        let repeated = state
            .execute_tx(
                StakingTransaction::ClaimUnstaked {
                    delegator: "alice".into(),
                },
                ready,
                [0u8; 32],
            )
            .unwrap();
        assert!(matches!(
            repeated,
            StakingTxResult::UnstakeClaimed { amount: 0, .. }
        ));
        assert_eq!(
            bincode::serialize(state.unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );

        let final_claim = state
            .execute_tx(
                StakingTransaction::ClaimUnstaked {
                    delegator: "alice".into(),
                },
                ready + 1,
                [0u8; 32],
            )
            .unwrap();
        assert!(matches!(
            final_claim,
            StakingTxResult::UnstakeClaimed { amount: 100, .. }
        ));
        assert!(!state.unstake_queue.contains_key("alice"));
        assert_eq!(
            bincode::serialize(state.unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );
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

        assert!(matches!(
            parsed,
            StakingTransaction::Delegate {
                amount: 1000_00,
                ..
            }
        ));
    }

    #[test]
    fn test_evidence_serialization_preserves_signed_vote_fields() {
        let context = ConsensusContext::with_genesis(0, [1u8; 32], [2u8; 32]);
        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: test_node_id(1),
            view: 7,
            timestamp: 8,
            context,
            hash_a: [3u8; 32],
            app_hash_a: [4u8; 32],
            hash_b: [5u8; 32],
            app_hash_b: [6u8; 32],
            signature_a: vec![7u8; 96],
            signature_b: vec![8u8; 96],
        };
        let tx = StakingTransaction::SubmitEvidence {
            submitter: "reporter".into(),
            evidence,
        };

        let parsed = StakingTransaction::from_bytes(&tx.to_bytes()).unwrap();
        let StakingTransaction::SubmitEvidence { evidence, .. } = parsed else {
            panic!("expected evidence transaction");
        };
        assert_eq!(evidence.context, context);
        assert_eq!(evidence.app_hash_a, [4u8; 32]);
        assert_eq!(evidence.app_hash_b, [6u8; 32]);
    }
}
