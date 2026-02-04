//! Slashing
//!
//! Handles equivocation detection and stake slashing.

use super::state::{StakingError, StakingState};
use super::types::{Evidence, EvidenceType, ValidatorStatus, EQUIVOCATION_SLASH_BPS};
use crate::crypto::bls::{BlsPublicKey, BlsSignature};
use crate::types::{Certificate, NodeId};

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
    ///
    /// SECURITY: This function cryptographically verifies that:
    /// 1. Both signatures are valid BLS signatures
    /// 2. Both signatures were made by the alleged offender's BLS key
    /// 3. The signatures are over different block hashes at the same view/height
    ///
    /// Without this verification, anyone could submit false slashing evidence.
    fn validate_evidence(&self, evidence: &Evidence) -> bool {
        // Basic validation
        if evidence.hash_a == evidence.hash_b {
            return false; // Same hash isn't equivocation
        }

        // Check validator exists and get their BLS pubkey
        let validator = match self.get_validator_by_node(&evidence.offender) {
            Some(v) => v,
            None => return false,
        };

        // Check signatures are present
        if evidence.signature_a.is_empty() || evidence.signature_b.is_empty() {
            return false;
        }

        // Parse validator's BLS public key
        let bls_pubkey = match Self::parse_bls_pubkey(&validator.bls_pubkey) {
            Some(pk) => pk,
            None => {
                tracing::warn!(
                    offender = %hex::encode(&evidence.offender[..4]),
                    "Evidence validation failed: validator has invalid BLS pubkey"
                );
                return false;
            }
        };

        // Verify BLS signatures based on evidence type
        match evidence.evidence_type {
            EvidenceType::DoubleVote => {
                self.verify_double_vote_evidence(evidence, &bls_pubkey)
            }
            EvidenceType::DoublePropose => {
                // Double propose uses same verification as double vote
                // (proposer signs the block hash they're proposing)
                self.verify_double_vote_evidence(evidence, &bls_pubkey)
            }
        }
    }

    /// Parse a 48-byte BLS public key from bytes
    fn parse_bls_pubkey(bytes: &[u8]) -> Option<BlsPublicKey> {
        if bytes.len() != 48 {
            return None;
        }
        let mut pk_arr = [0u8; 48];
        pk_arr.copy_from_slice(bytes);
        BlsPublicKey::from_bytes(&pk_arr).ok()
    }

    /// Verify BLS signatures for double vote evidence
    ///
    /// For a double vote, the validator signed two different block hashes
    /// at the same view. We verify both signatures are valid BLS signatures
    /// from the validator's registered public key.
    ///
    /// Note: The app_hash may differ between the two votes (validators might
    /// have different execution results), so we use a zero app_hash as placeholder
    /// for evidence verification. In practice, equivocation evidence from the
    /// consensus layer includes the full Vote objects which contain the app_hash.
    fn verify_double_vote_evidence(&self, evidence: &Evidence, bls_pubkey: &BlsPublicKey) -> bool {
        // Parse signatures
        let sig_a = match BlsSignature::from_slice(&evidence.signature_a) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("Evidence validation failed: invalid signature_a format");
                return false;
            }
        };
        let sig_b = match BlsSignature::from_slice(&evidence.signature_b) {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("Evidence validation failed: invalid signature_b format");
                return false;
            }
        };

        // Build signing messages using the common vote signing format:
        // view || block_hash || app_hash
        // Note: For equivocation detection, we use zero app_hash as a placeholder
        // since the evidence structure doesn't include app_hash. The key point is
        // that the same validator signed two different block_hashes at the same view.
        let zero_app_hash = [0u8; 32];
        let msg_a = Certificate::build_signing_message(evidence.height, &evidence.hash_a, &zero_app_hash);
        let msg_b = Certificate::build_signing_message(evidence.height, &evidence.hash_b, &zero_app_hash);

        // Verify both signatures
        let valid_a = bls_pubkey.verify(&msg_a, &sig_a);
        let valid_b = bls_pubkey.verify(&msg_b, &sig_b);

        if !valid_a {
            tracing::warn!(
                offender = %hex::encode(&evidence.offender[..4]),
                height = evidence.height,
                "Evidence validation failed: signature_a verification failed"
            );
        }
        if !valid_b {
            tracing::warn!(
                offender = %hex::encode(&evidence.offender[..4]),
                height = evidence.height,
                "Evidence validation failed: signature_b verification failed"
            );
        }

        valid_a && valid_b
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
    use crate::crypto::bls::BlsSecretKey;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    /// Create a deterministic BLS keypair for testing
    fn test_bls_keypair(seed: u8) -> (BlsSecretKey, Vec<u8>) {
        let mut seed_bytes = [0u8; 32];
        seed_bytes[0] = seed;
        let sk = BlsSecretKey::from_seed(&seed_bytes);
        let pk = sk.public_key();
        (sk, pk.to_bytes().to_vec())
    }

    /// Create valid double vote evidence with real BLS signatures
    fn create_valid_evidence(sk: &BlsSecretKey, node_id: NodeId, view: u64) -> Evidence {
        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];
        let zero_app_hash = [0u8; 32];

        // Sign both block hashes using the common vote signing format
        let msg_a = Certificate::build_signing_message(view, &hash_a, &zero_app_hash);
        let msg_b = Certificate::build_signing_message(view, &hash_b, &zero_app_hash);

        let sig_a = sk.sign(&msg_a);
        let sig_b = sk.sign(&msg_b);

        Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            height: view,
            timestamp: 1000,
            hash_a,
            hash_b,
            signature_a: sig_a.to_bytes().to_vec(),
            signature_b: sig_b.to_bytes().to_vec(),
        }
    }

    #[test]
    fn test_slash_for_equivocation() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Register validator with real BLS pubkey
        state
            .register_validator("v1".into(), node_id, pk_bytes, MIN_SELF_STAKE, 500)
            .unwrap();

        // Add delegation
        state.delegate("delegator".into(), "v1".into(), 100_000_00).unwrap();

        let initial_total = state.total_staked;
        let validator = state.get_validator(&"v1".into()).unwrap();
        let initial_self_stake = validator.self_stake;

        // Create valid evidence with real BLS signatures
        let evidence = create_valid_evidence(&sk, node_id, 100);
        state.submit_evidence(evidence).unwrap();

        // Process evidence
        let results = state.process_pending_evidence();
        assert_eq!(results.len(), 1);

        let result = &results[0];
        assert_eq!(result.offender, node_id);

        // Check 50% slash
        let expected_val_slash = initial_self_stake / 2;
        assert_eq!(result.validator_slash, expected_val_slash);

        // Validator should be tombstoned
        let validator = state.get_validator(&"v1".into()).unwrap();
        assert_eq!(validator.status, ValidatorStatus::Tombstoned);

        // Total staked should be reduced
        assert!(state.total_staked < initial_total);
    }

    #[test]
    fn test_invalid_evidence_rejected() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (_sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Register validator
        state
            .register_validator("v1".into(), node_id, pk_bytes, MIN_SELF_STAKE, 500)
            .unwrap();

        // Submit evidence with invalid signatures (random bytes, not valid BLS)
        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            height: 100,
            timestamp: 1000,
            hash_a: [1u8; 32],
            hash_b: [2u8; 32],
            signature_a: vec![1, 2, 3], // Invalid - too short
            signature_b: vec![4, 5, 6],
        };

        // Evidence should be rejected as invalid
        let result = state.submit_evidence(evidence);
        assert!(matches!(result, Err(StakingError::InvalidEvidence)));
    }

    #[test]
    fn test_evidence_wrong_signer_rejected() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (_sk1, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Create different keypair for signing (attacker trying to frame validator)
        let (sk2, _) = test_bls_keypair(2);

        // Register validator
        state
            .register_validator("v1".into(), node_id, pk_bytes, MIN_SELF_STAKE, 500)
            .unwrap();

        // Create evidence signed by wrong key
        let evidence = create_valid_evidence(&sk2, node_id, 100);

        // Evidence should be rejected - signatures don't match validator's pubkey
        let result = state.submit_evidence(evidence);
        assert!(matches!(result, Err(StakingError::InvalidEvidence)));
    }

    #[test]
    fn test_evidence_same_hash_rejected() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Register validator
        state
            .register_validator("v1".into(), node_id, pk_bytes, MIN_SELF_STAKE, 500)
            .unwrap();

        // Create "evidence" with same hash (not actually equivocation)
        let same_hash = [1u8; 32];
        let zero_app_hash = [0u8; 32];
        let msg = Certificate::build_signing_message(100, &same_hash, &zero_app_hash);
        let sig = sk.sign(&msg);

        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            height: 100,
            timestamp: 1000,
            hash_a: same_hash,
            hash_b: same_hash, // Same hash - not equivocation!
            signature_a: sig.to_bytes().to_vec(),
            signature_b: sig.to_bytes().to_vec(),
        };

        // Evidence should be rejected - same hash isn't equivocation
        let result = state.submit_evidence(evidence);
        assert!(matches!(result, Err(StakingError::InvalidEvidence)));
    }
}
