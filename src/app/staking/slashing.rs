//! Slashing
//!
//! Handles equivocation detection and stake slashing.

use super::state::{StakingError, StakingState};
use super::types::{Evidence, EvidenceType, ValidatorStatus, EQUIVOCATION_SLASH_BPS};
use crate::crypto::bls::{BlsPublicKey, BlsSignature};
use crate::types::{Certificate, ConsensusConfig, ConsensusContext, NodeId};

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
            .validators
            .values()
            .find(|validator| validator.node_id == evidence.offender)
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
    /// 3. The signatures are over different block hashes at the same view
    ///
    /// Without this verification, anyone could submit false slashing evidence.
    pub(crate) fn validate_evidence(&self, evidence: &Evidence) -> bool {
        // The persisted validator map is authoritative. The node index is a
        // rebuildable cache and must not decide whether a slashing target
        // exists after snapshot/recovery.
        let validator = match self
            .validators
            .values()
            .find(|validator| validator.node_id == evidence.offender)
        {
            Some(validator) => validator,
            None => return false,
        };
        self.validate_evidence_for_validator(evidence, validator)
    }

    /// Validate evidence against an authoritative validator record supplied by
    /// the caller. Snapshot primary validation uses this path before the
    /// transient node-to-operator index has been rebuilt.
    pub(crate) fn validate_evidence_for_validator(
        &self,
        evidence: &Evidence,
        validator: &super::types::ValidatorInfo,
    ) -> bool {
        // Basic validation
        if evidence.hash_a == evidence.hash_b {
            return false; // Same hash isn't equivocation
        }
        if evidence.context.epoch != 0 || !evidence.context.has_genesis_domain() {
            // Historical committees and zero-domain contexts are not valid
            // evidence in the current static epoch-0 tranche.
            return false;
        }

        // The context is part of the signed vote envelope. It must also
        // match the node-configured current context; otherwise a valid BLS
        // signature from another chain/committee could be replayed here.
        let expected_context = match self.static_consensus_context() {
            Some(context) => context,
            None => return false,
        };
        if evidence.context != expected_context {
            return false;
        }

        if self.require_authoritative_committee {
            let Some(committee) = self.authoritative_committee.as_ref() else {
                return false;
            };
            let Some(member) = committee.member(&evidence.offender) else {
                // A registered validator outside the active curated set is
                // not eligible for epoch-0 consensus evidence.
                return false;
            };
            if member.bls_pubkey.as_deref() != Some(validator.bls_pubkey.as_slice()) {
                // Never verify against a reporter-supplied key or a stale
                // validator registration that disagrees with the trusted
                // committee.
                return false;
            }
        }

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
            EvidenceType::DoubleVote => self.verify_double_vote_evidence(evidence, &bls_pubkey),
            // Proposer signatures use a distinct message scheme. Until a
            // dedicated proposer-proof verifier is wired here, fail closed
            // instead of treating a proposal as a vote.
            EvidenceType::DoublePropose => false,
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
    /// The app hashes are part of the signed vote message and must be carried
    /// by the evidence. Replacing either with a placeholder would make the
    /// evidence unverifiable (and could turn a valid proof into a replay).
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

        let msg_a = Certificate::build_signing_message(
            evidence.context,
            evidence.view,
            &evidence.hash_a,
            &evidence.app_hash_a,
        );
        let msg_b = Certificate::build_signing_message(
            evidence.context,
            evidence.view,
            &evidence.hash_b,
            &evidence.app_hash_b,
        );

        // Verify both signatures
        let valid_a = bls_pubkey.verify(&msg_a, &sig_a);
        let valid_b = bls_pubkey.verify(&msg_b, &sig_b);

        if !valid_a {
            tracing::warn!(
                offender = %hex::encode(&evidence.offender[..4]),
                view = evidence.view,
                "Evidence validation failed: signature_a verification failed"
            );
        }
        if !valid_b {
            tracing::warn!(
                offender = %hex::encode(&evidence.offender[..4]),
                view = evidence.view,
                "Evidence validation failed: signature_b verification failed"
            );
        }

        valid_a && valid_b
    }

    /// Reconstruct the canonical static committee context used by consensus.
    ///
    /// Dynamic validator-set activation is intentionally not supported in this
    /// phase. Evidence is therefore only valid against the current epoch-0
    /// committee; a missing or invalid committee causes rejection.
    pub(crate) fn static_consensus_context(&self) -> Option<ConsensusContext> {
        if self.require_authoritative_committee {
            let committee = self.authoritative_committee.as_ref()?;
            let context = self.consensus_context?;
            if committee.validate_context(context).is_err() || !context.has_genesis_domain() {
                return None;
            }
            return Some(context);
        }
        if let Some(context) = self.consensus_context {
            if context.epoch != 0 || !context.has_genesis_domain() {
                return None;
            }
            return Some(context);
        }
        if self.consensus_genesis_hash == [0u8; 32] {
            return None;
        }
        let validators: Vec<_> = self
            .validators
            .values()
            .filter(|validator| validator.can_be_active())
            .collect();
        let first = validators.first()?;
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: self.consensus_genesis_hash,
            node_id: first.node_id,
            validators: validators.iter().map(|v| v.node_id).collect(),
            voting_powers: validators
                .iter()
                .map(|v| v.total_stake.max(1) as u64)
                .collect(),
            view_timeout_ms: 0,
            bls_pubkeys: validators.iter().map(|v| v.bls_pubkey.clone()).collect(),
            bls_secret_key: None,
        };
        config.context().ok()
    }

    /// Process all pending evidence
    pub fn process_pending_evidence(&mut self) -> Vec<SlashResult> {
        let evidence = self.pending_evidence.take();
        evidence
            .into_iter()
            .filter_map(|e| self.process_evidence(e))
            .collect()
    }

    /// Process a single piece of evidence
    fn process_evidence(&mut self, evidence: Evidence) -> Option<SlashResult> {
        let offender = evidence.offender;

        // Get operator address
        let operator = self
            .validators
            .iter()
            .find(|(_, validator)| validator.node_id == offender)
            .map(|(operator, _)| operator.clone())?;

        // Calculate slash amounts
        let validator = self.validators.get(&operator)?;

        // Slash validator's self-stake
        let validator_slash =
            (validator.self_stake as i128 * EQUIVOCATION_SLASH_BPS as i128 / 10000) as i64;

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
            if let Some(del) = self
                .delegations
                .get_mut(&(delegator.clone(), validator_addr.clone()))
            {
                del.amount -= slash;
                del.reward_eligible_stake = del.reward_eligible_stake.min(del.amount.max(0));
                if del.amount <= 0 {
                    self.delegations
                        .remove(&(delegator.clone(), validator_addr.clone()));
                }
            }
        }

        // Apply slash to validator
        let validator = self.validators.get_mut(&operator)?;
        validator.self_stake -= validator_slash;
        validator.reward_eligible_stake = validator
            .reward_eligible_stake
            .min(validator.self_stake.max(0));
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
        app_hash: [u8; 32],
        context: ConsensusContext,
        signature: &[u8],
        existing_votes: &[(u64, [u8; 32], [u8; 32], Vec<u8>)], // (view, block hash, app hash, sig)
    ) -> Option<Evidence> {
        // Look for conflicting vote at same view
        for (v_view, v_hash, v_app_hash, v_sig) in existing_votes {
            if *v_view == view && *v_hash != block_hash {
                // Found equivocation!
                return Some(Evidence {
                    evidence_type: EvidenceType::DoubleVote,
                    offender: voter,
                    view,
                    timestamp: 0, // Will be filled by caller
                    context,
                    hash_a: *v_hash,
                    app_hash_a: *v_app_hash,
                    hash_b: block_hash,
                    app_hash_b: app_hash,
                    signature_a: v_sig.clone(),
                    signature_b: signature.to_vec(),
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
        context: ConsensusContext,
        app_hash_a: [u8; 32],
        app_hash_b: [u8; 32],
        signature_a: Vec<u8>,
        signature_b: Vec<u8>,
        timestamp: u64,
    ) -> Evidence {
        Evidence {
            evidence_type: EvidenceType::DoublePropose,
            offender: proposer,
            view,
            timestamp,
            context,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a,
            signature_b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::staking::types::{StaticValidatorBootstrap, MIN_SELF_STAKE};
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::ConsensusConfig;

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
    fn create_valid_evidence(
        state: &StakingState,
        sk: &BlsSecretKey,
        node_id: NodeId,
        view: u64,
    ) -> Evidence {
        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];
        let app_hash_a = [0x11u8; 32];
        let app_hash_b = [0x22u8; 32];

        // Sign both block hashes using the common vote signing format
        let context = state
            .static_consensus_context()
            .expect("registered validator must have a static consensus context");
        let msg_a = Certificate::build_signing_message(context, view, &hash_a, &app_hash_a);
        let msg_b = Certificate::build_signing_message(context, view, &hash_b, &app_hash_b);

        let sig_a = sk.sign(&msg_a);
        let sig_b = sk.sign(&msg_b);

        Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            view,
            timestamp: 1000,
            context,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a: sig_a.to_bytes().to_vec(),
            signature_b: sig_b.to_bytes().to_vec(),
        }
    }

    fn test_bls_proof(sk: &BlsSecretKey, node_id: NodeId) -> Vec<u8> {
        sk.create_proof_of_possession(&[0u8; 32], &node_id)
            .to_bytes()
            .to_vec()
    }

    fn configured_state() -> (StakingState, BlsSecretKey, NodeId, ConsensusContext) {
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3_000,
            bls_pubkeys: vec![pk_bytes.clone()],
            bls_secret_key: None,
        };
        let committee = config.committee().expect("test committee");
        let context = config.context().expect("test context");
        let mut state = StakingState::new();
        state.set_consensus_context(context);
        let proof = sk
            .create_proof_of_possession(&context.genesis_hash, &node_id)
            .to_bytes()
            .to_vec();
        state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: format!("system:genesis:{}", hex::encode(node_id)),
                    node_id,
                    voting_power: 1,
                    bls_pubkey: pk_bytes,
                    bls_proof_of_possession: proof,
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
                context,
            )
            .expect("bootstrap committee");
        state
            .bind_authoritative_committee(committee, context)
            .expect("bind committee");
        (state, sk, node_id, context)
    }

    #[test]
    fn authoritative_committee_rejects_registered_outsider() {
        let (mut state, _, _, context) = configured_state();
        let (outsider_sk, outsider_pk) = test_bls_keypair(2);
        let outsider = test_node_id(2);
        let outsider_pop = outsider_sk
            .create_proof_of_possession(&context.genesis_hash, &outsider)
            .to_bytes()
            .to_vec();
        state
            .register_validator(
                "outsider".into(),
                outsider,
                outsider_pk,
                outsider_pop,
                context.genesis_hash,
                MIN_SELF_STAKE,
                0,
            )
            .unwrap();

        let evidence = create_valid_evidence(&state, &outsider_sk, outsider, 100);
        assert!(matches!(
            state.submit_evidence(evidence),
            Err(StakingError::InvalidEvidence)
        ));
    }

    #[test]
    fn authoritative_committee_rejects_registered_member_key_mismatch() {
        let (committee_sk, committee_pk) = test_bls_keypair(1);
        let (registered_sk, registered_pk) = test_bls_keypair(2);
        let node_id = test_node_id(1);
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3_000,
            bls_pubkeys: vec![committee_pk],
            bls_secret_key: None,
        };
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let mut state = StakingState::new();
        state.set_consensus_context(context);
        let registered_pop = registered_sk
            .create_proof_of_possession(&context.genesis_hash, &node_id)
            .to_bytes()
            .to_vec();
        state
            .register_validator(
                "mismatch".into(),
                node_id,
                registered_pk,
                registered_pop,
                context.genesis_hash,
                MIN_SELF_STAKE,
                0,
            )
            .unwrap();

        let error = state
            .bind_authoritative_committee(committee, context)
            .expect_err("a registered committee member with a different key must be rejected");
        assert!(matches!(
            error,
            StakingError::InvalidAuthoritativeCommittee(message)
                if message.contains("different BLS key")
        ));
        // Keep the authoritative key in use in this test so the compiler
        // cannot accidentally make the mismatch test depend on a reporter
        // supplied key.
        assert_ne!(
            committee_sk.public_key().to_bytes(),
            registered_sk.public_key().to_bytes()
        );
    }

    #[test]
    fn configured_committee_member_is_accepted_and_tombstoned() {
        let (mut state, sk, node_id, _) = configured_state();
        let evidence = create_valid_evidence(&state, &sk, node_id, 100);
        state.submit_evidence(evidence).unwrap();
        let results = state.process_pending_evidence();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].offender, node_id);
        assert_eq!(
            state.get_validator_by_node(&node_id).unwrap().status,
            ValidatorStatus::Tombstoned
        );
    }

    #[test]
    fn test_slash_for_equivocation() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Register validator with real BLS pubkey
        state
            .register_validator(
                "v1".into(),
                node_id,
                pk_bytes.clone(),
                test_bls_proof(&sk, node_id),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state.set_consensus_genesis_hash([9u8; 32]);

        // Add delegation
        state
            .delegate("delegator".into(), "v1".into(), 100_000_00)
            .unwrap();

        let initial_total = state.total_staked;
        let validator = state.get_validator(&"v1".into()).unwrap();
        let initial_self_stake = validator.self_stake;

        // Create valid evidence with real BLS signatures
        let evidence = create_valid_evidence(&state, &sk, node_id, 100);
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
        assert!(validator.reward_eligible_stake <= validator.self_stake);
        assert!(state
            .delegations
            .values()
            .all(|delegation| delegation.reward_eligible_stake <= delegation.amount));

        // Total staked should be reduced
        assert!(state.total_staked < initial_total);
    }

    #[test]
    fn test_invalid_evidence_rejected() {
        let mut state = StakingState::new();

        // Create BLS keypair for validator
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Register validator
        state
            .register_validator(
                "v1".into(),
                node_id,
                pk_bytes,
                test_bls_proof(&sk, node_id),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state.set_consensus_genesis_hash([9u8; 32]);

        // Submit evidence with invalid signatures (random bytes, not valid BLS)
        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            view: 100,
            timestamp: 1000,
            context: state.static_consensus_context().unwrap(),
            hash_a: [1u8; 32],
            app_hash_a: [0x11u8; 32],
            hash_b: [2u8; 32],
            app_hash_b: [0x22u8; 32],
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
        let (sk1, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);

        // Create different keypair for signing (attacker trying to frame validator)
        let (sk2, _) = test_bls_keypair(2);

        // Register validator
        state
            .register_validator(
                "v1".into(),
                node_id,
                pk_bytes,
                test_bls_proof(&sk1, node_id),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state.set_consensus_genesis_hash([9u8; 32]);

        // Create evidence signed by wrong key
        let evidence = create_valid_evidence(&state, &sk2, node_id, 100);

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
            .register_validator(
                "v1".into(),
                node_id,
                pk_bytes.clone(),
                test_bls_proof(&sk, node_id),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state.set_consensus_genesis_hash([9u8; 32]);

        // Create "evidence" with same hash (not actually equivocation)
        let same_hash = [1u8; 32];
        let app_hash = [0x11u8; 32];
        let context = state.static_consensus_context().unwrap();
        let msg = Certificate::build_signing_message(context, 100, &same_hash, &app_hash);
        let sig = sk.sign(&msg);

        let evidence = Evidence {
            evidence_type: EvidenceType::DoubleVote,
            offender: node_id,
            view: 100,
            timestamp: 1000,
            context,
            hash_a: same_hash,
            app_hash_a: app_hash,
            hash_b: same_hash, // Same hash - not equivocation!
            app_hash_b: app_hash,
            signature_a: sig.to_bytes().to_vec(),
            signature_b: sig.to_bytes().to_vec(),
        };

        // Evidence should be rejected - same hash isn't equivocation
        let result = state.submit_evidence(evidence);
        assert!(matches!(result, Err(StakingError::InvalidEvidence)));
    }

    #[test]
    fn test_evidence_binds_context_and_app_hashes() {
        let mut state = StakingState::new();
        let (sk, pk_bytes) = test_bls_keypair(1);
        let node_id = test_node_id(1);
        state
            .register_validator(
                "v1".into(),
                node_id,
                pk_bytes,
                test_bls_proof(&sk, node_id),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state.set_consensus_genesis_hash([9u8; 32]);

        let valid = create_valid_evidence(&state, &sk, node_id, 100);

        let mut wrong_app_hash = valid.clone();
        wrong_app_hash.app_hash_a[0] ^= 1;
        assert!(matches!(
            state.submit_evidence(wrong_app_hash),
            Err(StakingError::InvalidEvidence)
        ));

        let mut wrong_chain = valid.clone();
        wrong_chain.context.genesis_hash = [8u8; 32];
        assert!(matches!(
            state.submit_evidence(wrong_chain),
            Err(StakingError::InvalidEvidence)
        ));

        let mut zero_domain = valid.clone();
        zero_domain.context.genesis_hash = [0u8; 32];
        assert!(matches!(
            state.submit_evidence(zero_domain),
            Err(StakingError::InvalidEvidence)
        ));

        let mut historical = valid;
        historical.context.epoch = 1;
        assert!(matches!(
            state.submit_evidence(historical),
            Err(StakingError::InvalidEvidence)
        ));
    }
}
