//! Jailing
//!
//! Handles validator jailing for downtime and unjailing logic.
//!
//! Jailing is a temporary penalty for validators who fail to participate:
//! - Validators with <90% liveness are jailed at epoch end
//! - Validators with 100 consecutive missed blocks are auto-jailed
//! - Jail duration is 1 hour
//! - Validators must explicitly unjail after jail period expires
//!
//! Note: Most jailing logic is in epoch.rs for epoch-based jailing.
//! This module contains additional jailing utilities.

use super::state::StakingState;
use super::types::{ValidatorStatus, JAIL_DURATION_MS, MAX_CONSECUTIVE_MISSED};
use crate::types::NodeId;

/// Reasons for jailing
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JailReason {
    /// Jailed at epoch end for low liveness
    LowLiveness,
    /// Jailed for too many consecutive missed blocks
    ConsecutiveMisses,
    /// Manually jailed (governance)
    Manual,
}

/// Information about a jailed validator
#[derive(Debug, Clone)]
pub struct JailInfo {
    pub node_id: NodeId,
    pub operator: String,
    pub reason: JailReason,
    pub jail_until: u64,
    pub missed_blocks: u64,
}

impl StakingState {
    /// Get all currently jailed validators
    pub fn jailed_validators(&self) -> Vec<JailInfo> {
        self.validators
            .iter()
            .filter(|(_, v)| v.status == ValidatorStatus::Jailed)
            .map(|(op, v)| JailInfo {
                node_id: v.node_id,
                operator: op.clone(),
                reason: if v.missed_consecutive >= MAX_CONSECUTIVE_MISSED {
                    JailReason::ConsecutiveMisses
                } else {
                    JailReason::LowLiveness
                },
                jail_until: v.jail_until,
                missed_blocks: v.missed_consecutive,
            })
            .collect()
    }

    /// Check if a validator can be unjailed
    pub fn can_unjail(&self, operator: &str, current_time: u64) -> bool {
        self.validators
            .get(operator)
            .map(|v| v.status == ValidatorStatus::Jailed && current_time >= v.jail_until)
            .unwrap_or(false)
    }

    /// Get time remaining in jail (returns 0 if not jailed or can be unjailed)
    pub fn jail_time_remaining(&self, operator: &str, current_time: u64) -> u64 {
        self.validators
            .get(operator)
            .filter(|v| v.status == ValidatorStatus::Jailed)
            .map(|v| v.jail_until.saturating_sub(current_time))
            .unwrap_or(0)
    }

    /// Manually jail a validator (for governance actions)
    pub fn manual_jail(&mut self, operator: &str, current_time: u64, duration_ms: Option<u64>) {
        if let Some(validator) = self.validators.get_mut(operator) {
            if validator.status != ValidatorStatus::Tombstoned {
                validator.status = ValidatorStatus::Jailed;
                validator.jail_until = current_time + duration_ms.unwrap_or(JAIL_DURATION_MS);
            }
        }
    }

    /// Get liveness percentage for a validator (in basis points, 10000 = 100%)
    pub fn validator_liveness(&self, node_id: &NodeId) -> Option<i64> {
        self.liveness.get(node_id).map(|r| r.liveness_bps())
    }

    /// Check if validator is at risk of being jailed (liveness below threshold)
    pub fn at_risk_of_jailing(&self, node_id: &NodeId) -> bool {
        self.liveness
            .get(node_id)
            .map(|r| r.liveness_bps() < super::types::MIN_LIVENESS_BPS)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::staking::types::MIN_SELF_STAKE;
    use crate::crypto::bls::BlsSecretKey;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key() -> Vec<u8> {
        BlsSecretKey::from_seed(&[1u8; 32])
            .public_key()
            .to_bytes()
            .to_vec()
    }

    fn test_bls_proof() -> Vec<u8> {
        let node_id = test_node_id(1);
        BlsSecretKey::from_seed(&[1u8; 32])
            .create_proof_of_possession(&[0u8; 32], &node_id)
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn test_manual_jail() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "v1".into(),
                test_node_id(1),
                test_bls_key(),
                test_bls_proof(),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        // Manually jail
        state.manual_jail("v1", 1000, Some(5000));

        let validator = state.get_validator(&"v1".into()).unwrap();
        assert_eq!(validator.status, ValidatorStatus::Jailed);
        assert_eq!(validator.jail_until, 6000);

        // Check jail info
        let jailed = state.jailed_validators();
        assert_eq!(jailed.len(), 1);
        assert_eq!(jailed[0].operator, "v1");
    }

    #[test]
    fn test_can_unjail() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "v1".into(),
                test_node_id(1),
                test_bls_key(),
                test_bls_proof(),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        state.manual_jail("v1", 1000, Some(5000));

        // Too early to unjail
        assert!(!state.can_unjail("v1", 5000));
        assert_eq!(state.jail_time_remaining("v1", 5000), 1000);

        // Can unjail after jail period
        assert!(state.can_unjail("v1", 6000));
        assert_eq!(state.jail_time_remaining("v1", 6000), 0);
    }
}
