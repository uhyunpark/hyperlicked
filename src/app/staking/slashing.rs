//! Slashing
//!
//! Handles equivocation detection and stake slashing.

use super::state::{StakingError, StakingState};
use super::types::{Evidence, EvidenceType, ValidatorStatus, EQUIVOCATION_SLASH_BPS};
use crate::types::NodeId;

/// Result of processing evidence
#[derive(Debug, Clone)]
pub struct SlashResult {
    /// Validator that was slashed
    pub offender: NodeId,
    /// Amount slashed from validator
    pub validator_slash: i64,
    /// Amount slashed from delegators
    pub delegator_slash: i64,
    /// Total amount added to insurance/burn
    pub total_slashed: i64,
}

impl StakingState {
    /// Submit evidence of misbehavior
    pub fn submit_evidence(&mut self, evidence: Evidence) -> Result<(), StakingError> {
        // Validate evidence
        if !self.validate_evidence(&evidence) {
            return Err(StakingError::InvalidEvidence);
        }

        // Check validator exists and isn't already tombstoned
        let validator = self
            .get_validator_by_node(&evidence.offender)
            .ok_or(StakingError::ValidatorNotFound)?;

        if validator.status == ValidatorStatus::Tombstoned {
            // Already slashed, no double punishment
            return Ok(());
        }

        // Add to pending evidence for processing
        self.pending_evidence.push(evidence);
        Ok(())
    }

    /// Validate evidence of misbehavior
    fn validate_evidence(&self, evidence: &Evidence) -> bool {
        // Basic validation
        if evidence.hash_a == evidence.hash_b {
            return false; // Same hash isn't equivocation
        }

        // Check validator exists
        if self.get_validator_by_node(&evidence.offender).is_none() {
            return false;
        }

        // In production, we would also:
        // 1. Verify signatures on both items
        // 2. Check that evidence is recent (within lookback window)
        // 3. Verify the same validator signed both items
        // 4. For double vote: verify same view/height
        // 5. For double propose: verify same view

        // For now, trust evidence format (BLS verification would go here)
        match evidence.evidence_type {
            EvidenceType::DoubleVote | EvidenceType::DoublePropose => {
                !evidence.signature_a.is_empty() && !evidence.signature_b.is_empty()
            }
        }
    }

    /// Process all pending evidence
    pub fn process_pending_evidence(&mut self) -> Vec<SlashResult> {
        let evidence = std::mem::take(&mut self.pending_evidence);
        evidence
            .into_iter()
            .filter_map(|e| self.process_evidence(e))
            .collect()
    }

    /// Process a single piece of evidence
    fn process_evidence(&mut self, evidence: Evidence) -> Option<SlashResult> {
        let offender = evidence.offender;

        // Get operator address
        let operator = self.node_to_operator.get(&offender)?.clone();

        // Calculate slash amounts
        let validator = self.validators.get(&operator)?;

        // Slash validator's self-stake
        let validator_slash = (validator.self_stake as i128 * EQUIVOCATION_SLASH_BPS as i128 / 10000) as i64;

        // Slash delegators proportionally
        let delegations: Vec<_> = self
            .delegations
            .iter()
            .filter(|((_, v), _)| v == &operator)
            .map(|(k, d)| (k.clone(), d.amount))
            .collect();

        let mut delegator_slash = 0i64;
        for ((delegator, validator_addr), amount) in &delegations {
            let slash = (*amount as i128 * EQUIVOCATION_SLASH_BPS as i128 / 10000) as i64;
            delegator_slash += slash;

            // Update delegation
            if let Some(del) = self.delegations.get_mut(&(delegator.clone(), validator_addr.clone())) {
                del.amount -= slash;
                if del.amount <= 0 {
                    self.delegations.remove(&(delegator.clone(), validator_addr.clone()));
                }
            }
        }

        // Apply slash to validator
        let validator = self.validators.get_mut(&operator)?;
        validator.self_stake -= validator_slash;
        validator.total_stake -= validator_slash + delegator_slash;
        validator.status = ValidatorStatus::Tombstoned;

        // Update total staked
        self.total_staked -= validator_slash + delegator_slash;

        let total_slashed = validator_slash + delegator_slash;

        Some(SlashResult {
            offender,
            validator_slash,
            delegator_slash,
            total_slashed,
        })
    }

    /// Check for equivocation (called when receiving votes)
    pub fn check_equivocation(
        &self,
        voter: NodeId,
        view: u64,
        block_hash: [u8; 32],
        _signature: &[u8],
        existing_votes: &[(u64, [u8; 32], Vec<u8>)], // (view, hash, sig)
    ) -> Option<Evidence> {
        // Look for conflicting vote at same view
        for (v_view, v_hash, v_sig) in existing_votes {
            if *v_view == view && *v_hash != block_hash {
                // Found equivocation!
                return Some(Evidence {
                    evidence_type: EvidenceType::DoubleVote,
                    offender: voter,
                    height: view,
                    timestamp: 0, // Will be filled by caller
                    hash_a: *v_hash,
                    hash_b: block_hash,
                    signature_a: v_sig.clone(),
                    signature_b: vec![], // Will be filled by caller
                });
            }
        }
        None
    }

    /// Create evidence for double proposal
    pub fn create_double_propose_evidence(
        proposer: NodeId,
        view: u64,
        hash_a: [u8; 32],
        hash_b: [u8; 32],
        signature_a: Vec<u8>,
        signature_b: Vec<u8>,
        timestamp: u64,
    ) -> Evidence {
        Evidence {
            evidence_type: EvidenceType::DoublePropose,
            offender: proposer,
            height: view,
            timestamp,
            hash_a,
            hash_b,
            signature_a,
            signature_b,
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
    fn test_slash_for_equivocation() {
        let mut state = StakingState::new();

        // Register validator
        state
            .register_validator("v1".into(), test_node_id(1), test_bls_key(), MIN_SELF_STAKE, 500)
            .unwrap();

        // Add delegation
        state.delegate("delegator".into(), "v1".into(), 100_000_00).unwrap();

        let initial_total = state.total_staked;
        let validator = state.get_validator(&"v1".into()).unwrap();
        let initial_self_stake = validator.self_stake;
        let initial_total_stake = validator.total_stake;

        // Submit evidence
        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: test_node_id(1),
            height: 100,
            timestamp: 1000,
            hash_a: [1u8; 32],
            hash_b: [2u8; 32],
            signature_a: vec![1, 2, 3],
            signature_b: vec![4, 5, 6],
        };
        state.submit_evidence(evidence).unwrap();

        // Process evidence
        let results = state.process_pending_evidence();
        assert_eq!(results.len(), 1);

        let result = &results[0];
        assert_eq!(result.offender, test_node_id(1));

        // Check 50% slash
        let expected_val_slash = initial_self_stake / 2;
        assert_eq!(result.validator_slash, expected_val_slash);

        // Validator should be tombstoned
        let validator = state.get_validator(&"v1".into()).unwrap();
        assert_eq!(validator.status, ValidatorStatus::Tombstoned);

        // Total staked should be reduced
        assert!(state.total_staked < initial_total);
    }
}
