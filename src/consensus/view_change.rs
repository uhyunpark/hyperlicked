//! View Change Protocol
//!
//! Handles coordinated view changes when leader fails.
//! Implements HotStuff-2 Case-2 pacemaker pattern.
//!
//! ## Protocol Flow
//!
//! 1. Validator times out waiting for leader's proposal
//! 2. Validator broadcasts ViewChange with their high_qc
//! 3. New leader collects 2f+1 ViewChanges
//! 4. New leader broadcasts NewView with highest QC among ViewChanges
//! 5. All validators advance to new view

use std::collections::HashMap;

/// Maximum views ahead of current view to accept ViewChanges.
/// Prevents memory exhaustion attacks from far-future ViewChanges.
pub const MAX_FUTURE_VIEWS: u64 = 10;

use crate::crypto::bls::{BlsPublicKey, BlsSecretKey, BlsSignature};
use crate::types::{
    Certificate, Committee, ConsensusContext, NodeId, View, ViewChange, ViewChangeCertificate,
};

use super::verify_certificate;

/// Collects ViewChange messages and forms ViewChangeCertificates
pub struct ViewChangeCollector {
    /// ViewChange messages per target view: view -> (sender -> ViewChange)
    view_changes: HashMap<View, HashMap<NodeId, ViewChange>>,

    /// Quorum size needed (2f+1)
    quorum: usize,

    /// Validator public keys for signature verification (None = skip verification)
    validator_pubkeys: Option<HashMap<NodeId, BlsPublicKey>>,

    /// When present, quorum is stake-weighted over this canonical committee.
    committee: Option<Committee>,

    /// Consensus context all collected view changes must authenticate.
    context: Option<ConsensusContext>,
}

impl ViewChangeCollector {
    /// Create a new collector with the given quorum size
    pub fn new(quorum: usize) -> Self {
        Self {
            view_changes: HashMap::new(),
            quorum,
            validator_pubkeys: None,
            committee: None,
            context: None,
        }
    }

    /// Create a new collector with signature verification enabled
    pub fn with_validators(
        quorum: usize,
        validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    ) -> Self {
        Self {
            view_changes: HashMap::new(),
            quorum,
            validator_pubkeys: Some(validator_pubkeys),
            committee: None,
            context: None,
        }
    }

    /// Create a collector bound to a fully keyed committee.
    pub fn with_committee(committee: Committee) -> Result<Self, String> {
        let validator_pubkeys = committee_pubkeys(&committee)?;
        let context = committee.initial_context();
        Ok(Self {
            view_changes: HashMap::new(),
            quorum: 0,
            validator_pubkeys: Some(validator_pubkeys),
            committee: Some(committee),
            context: Some(context),
        })
    }

    /// Create a committee-bound collector with the validated genesis domain
    /// from the node's consensus configuration.
    pub fn with_committee_and_context(
        committee: Committee,
        context: ConsensusContext,
    ) -> Result<Self, String> {
        let collector = Self::with_committee(committee)?;
        if collector.context.is_some_and(|committee_context| {
            committee_context.epoch != context.epoch
                || committee_context.committee_hash != context.committee_hash
        }) {
            return Err("view-change committee does not match consensus context".to_string());
        }
        Ok(collector.with_context(context))
    }

    /// Bind this collector to a static consensus context.
    pub fn with_context(mut self, context: ConsensusContext) -> Self {
        self.context = Some(context);
        self
    }

    /// Return the context enforced by this collector, if configured.
    pub fn context(&self) -> Option<ConsensusContext> {
        self.context
    }

    /// Add a ViewChange message.
    ///
    /// Returns ViewChangeCertificate if quorum reached for the target view.
    pub fn add(&mut self, vc: ViewChange) -> Option<ViewChangeCertificate> {
        let target_view = vc.to_view;
        let sender = vc.sender;

        // Legacy/test collectors adopt the first message's context. Live
        // collectors are bound to the canonical committee before receiving
        // network messages.
        if let Some(expected) = self.context {
            if vc.context() != expected {
                tracing::warn!(
                    expected_epoch = expected.epoch,
                    got_epoch = vc.epoch,
                    "Rejecting ViewChange with mismatched consensus context"
                );
                return None;
            }
        } else {
            self.context = Some(vc.context());
        }

        // Validate the view change (with or without signature verification)
        let validation_result = if let Some(ref pubkeys) = self.validator_pubkeys {
            validate_view_change_with_sig(&vc, pubkeys)
        } else {
            validate_view_change(&vc)
        };

        if let Err(e) = validation_result {
            tracing::warn!(error = %e, "Invalid ViewChange received");
            return None;
        }

        let view_map = self.view_changes.entry(target_view).or_default();

        // Don't accept duplicate from same sender
        if view_map.contains_key(&sender) {
            return None;
        }

        view_map.insert(sender, vc);

        // Check if we've reached quorum.  The live collector is bound to the
        // canonical committee and therefore cannot fall back to count-based
        // quorum.
        let reached = if let Some(committee) = &self.committee {
            committee
                .has_weighted_quorum(view_map.keys().copied())
                .unwrap_or(false)
        } else {
            view_map.len() >= self.quorum
        };
        if reached {
            let vcs: Vec<ViewChange> = view_map.values().cloned().collect();
            ViewChangeCertificate::new(
                self.context
                    .unwrap_or_else(|| ConsensusContext::new(0, [0u8; 32])),
                target_view,
                vcs,
            )
            .ok()
        } else {
            None
        }
    }

    /// Get the highest QC from collected ViewChange messages for a view
    pub fn highest_qc(&self, view: View) -> Option<Certificate> {
        self.view_changes
            .get(&view)?
            .values()
            .filter_map(|vc| vc.high_qc.clone())
            .max_by_key(|qc| qc.view)
    }

    /// Get current count of ViewChanges for a target view
    pub fn count(&self, view: View) -> usize {
        self.view_changes.get(&view).map(|m| m.len()).unwrap_or(0)
    }

    /// Prune old view change data below a certain view
    pub fn prune_below(&mut self, view: View) {
        self.view_changes.retain(|v, _| *v >= view);
    }

    /// Clear all collected view changes
    pub fn clear(&mut self) {
        self.view_changes.clear();
    }
}

/// Validates a ViewChange message (basic structure only)
pub fn validate_view_change(vc: &ViewChange) -> Result<(), ViewChangeError> {
    // Must be advancing to a higher view
    if vc.to_view <= vc.from_view {
        return Err(ViewChangeError::InvalidViewAdvance {
            from: vc.from_view,
            to: vc.to_view,
        });
    }

    // to_view should be from_view + 1 (normal case)
    // Allow larger jumps for catch-up scenarios
    if vc.to_view > vc.from_view + 100 {
        return Err(ViewChangeError::ViewJumpTooLarge {
            from: vc.from_view,
            to: vc.to_view,
        });
    }

    Ok(())
}

/// Validates a ViewChange message with BLS signature verification
///
/// Parameters:
/// - `vc`: The ViewChange message to validate
/// - `validator_pubkeys`: Map of NodeId -> BLS public key for signature verification
pub fn validate_view_change_with_sig(
    vc: &ViewChange,
    validator_pubkeys: &HashMap<NodeId, BlsPublicKey>,
) -> Result<(), ViewChangeError> {
    // First do basic validation
    validate_view_change(vc)?;

    // Verify sender is a known validator
    let pubkey = validator_pubkeys
        .get(&vc.sender)
        .ok_or(ViewChangeError::UnknownValidator(vc.sender))?;

    // Verify BLS signature
    if vc.signature.len() != 96 {
        return Err(ViewChangeError::InvalidSignature);
    }

    let sig =
        BlsSignature::from_slice(&vc.signature).map_err(|_| ViewChangeError::InvalidSignature)?;

    let signing_data = vc.signing_data();
    if !pubkey.verify(&signing_data, &sig) {
        return Err(ViewChangeError::InvalidSignature);
    }

    Ok(())
}

fn committee_pubkeys(committee: &Committee) -> Result<HashMap<NodeId, BlsPublicKey>, String> {
    if !committee.bls_enabled() {
        return Err("view-change committee must have a BLS key for every member".to_string());
    }

    let mut pubkeys = HashMap::new();
    for member in committee.members() {
        let bytes = member
            .bls_pubkey
            .as_ref()
            .ok_or_else(|| "view-change committee is missing a BLS key".to_string())?;
        if bytes.len() != 48 {
            return Err(format!(
                "invalid BLS public key length for validator {}",
                hex::encode(member.node_id)
            ));
        }
        let mut key_bytes = [0u8; 48];
        key_bytes.copy_from_slice(bytes);
        let key = BlsPublicKey::from_bytes(&key_bytes).map_err(|_| {
            format!(
                "invalid BLS public key for validator {}",
                hex::encode(member.node_id)
            )
        })?;
        pubkeys.insert(member.node_id, key);
    }
    Ok(pubkeys)
}

/// Validate a signed ViewChange against the active committee and current view.
pub fn validate_view_change_with_committee(
    vc: &ViewChange,
    committee: &Committee,
    current_view: View,
) -> Result<(), ViewChangeError> {
    validate_view_change_with_committee_and_context(
        vc,
        committee,
        committee.initial_context(),
        current_view,
    )
}

/// Validate a signed ViewChange against the canonical committee and an
/// explicitly configured genesis-bound context.
pub fn validate_view_change_with_committee_and_context(
    vc: &ViewChange,
    committee: &Committee,
    expected_context: ConsensusContext,
    current_view: View,
) -> Result<(), ViewChangeError> {
    vc.validate_context(expected_context)
        .map_err(|_| ViewChangeError::ContextMismatch)?;
    validate_view_change_bounds(vc, current_view)?;
    let pubkeys = committee_pubkeys(committee).map_err(|_| ViewChangeError::InvalidSignature)?;
    validate_view_change_with_sig(vc, &pubkeys)?;

    if committee.member(&vc.sender).is_none() {
        return Err(ViewChangeError::UnknownValidator(vc.sender));
    }

    if let Some(qc) = &vc.high_qc {
        qc.validate_context(expected_context)
            .map_err(|_| ViewChangeError::InvalidHighQc)?;
        let app_hash = qc.app_hash.as_ref().ok_or(ViewChangeError::InvalidHighQc)?;
        verify_certificate(
            committee,
            qc,
            expected_context,
            qc.view,
            &qc.block_hash,
            Some(app_hash),
            true,
        )
        .map_err(|_| ViewChangeError::InvalidHighQc)?;
    }

    Ok(())
}

/// Validate a VCC before allowing a NewView to mutate safety or pacemaker
/// state.  Every embedded ViewChange and high QC is authenticated, and the
/// signer set must satisfy strict weighted quorum.
pub fn validate_view_change_certificate_with_committee(
    vcc: &ViewChangeCertificate,
    committee: &Committee,
    current_view: View,
) -> Result<(), String> {
    validate_view_change_certificate_with_committee_and_context(
        vcc,
        committee,
        committee.initial_context(),
        current_view,
    )
}

/// Validate a ViewChangeCertificate against the canonical committee and an
/// explicitly configured genesis-bound context.
pub fn validate_view_change_certificate_with_committee_and_context(
    vcc: &ViewChangeCertificate,
    committee: &Committee,
    expected_context: ConsensusContext,
    current_view: View,
) -> Result<(), String> {
    vcc.validate_context(expected_context)?;
    if vcc.view <= current_view {
        return Err(format!(
            "view-change certificate {} is not ahead of current view {}",
            vcc.view, current_view
        ));
    }
    if vcc.view > current_view.saturating_add(MAX_FUTURE_VIEWS) {
        return Err("view-change certificate is too far ahead".to_string());
    }
    if vcc.view_changes.is_empty() {
        return Err("view-change certificate has no signers".to_string());
    }

    let mut signers = std::collections::HashSet::new();
    for vc in &vcc.view_changes {
        vc.validate_context(expected_context)?;
        if vc.to_view != vcc.view {
            return Err("ViewChange target does not match VCC view".to_string());
        }
        if !signers.insert(vc.sender) {
            return Err(format!(
                "duplicate ViewChange signer {}",
                hex::encode(vc.sender)
            ));
        }
        validate_view_change_with_committee_and_context(
            vc,
            committee,
            expected_context,
            current_view,
        )
        .map_err(|error| error.to_string())?;
    }

    if !committee
        .has_weighted_quorum(signers)
        .map_err(|error| error.to_string())?
    {
        return Err("ViewChange certificate lacks strict weighted quorum".to_string());
    }

    Ok(())
}

/// Create a signed ViewChange message
pub fn create_signed_view_change(
    from_view: View,
    to_view: View,
    high_qc: Option<Certificate>,
    sender: NodeId,
    bls_sk: &BlsSecretKey,
) -> ViewChange {
    create_signed_view_change_with_context(
        ConsensusContext::new(0, [0u8; 32]),
        from_view,
        to_view,
        high_qc,
        sender,
        bls_sk,
    )
}

/// Create a BLS-signed ViewChange bound to the active consensus context.
pub fn create_signed_view_change_with_context(
    context: ConsensusContext,
    from_view: View,
    to_view: View,
    high_qc: Option<Certificate>,
    sender: NodeId,
    bls_sk: &BlsSecretKey,
) -> ViewChange {
    let mut vc = ViewChange {
        epoch: context.epoch,
        committee_hash: context.committee_hash,
        genesis_hash: context.genesis_hash,
        from_view,
        to_view,
        high_qc,
        sender,
        signature: vec![0u8; 96], // Placeholder
    };

    // Sign the view change
    let signing_data = vc.signing_data();
    let sig = bls_sk.sign(&signing_data);
    vc.signature = sig.to_bytes().to_vec();

    vc
}

/// Errors that can occur during view change validation
#[derive(Debug, Clone, thiserror::Error)]
pub enum ViewChangeError {
    #[error("invalid view advance: from {from} to {to} (must increase)")]
    InvalidViewAdvance { from: View, to: View },

    #[error("view jump too large: from {from} to {to} (max 100)")]
    ViewJumpTooLarge { from: View, to: View },

    #[error("view too far ahead: vc_view {vc_view} > current_view {current_view} + {max_future}")]
    ViewTooFarAhead {
        vc_view: View,
        current_view: View,
        max_future: u64,
    },

    #[error("invalid signature")]
    InvalidSignature,

    #[error("invalid high QC")]
    InvalidHighQc,

    #[error("consensus context mismatch")]
    ContextMismatch,

    #[error("sender mismatch")]
    SenderMismatch,

    #[error("unknown validator: {0:?}")]
    UnknownValidator(NodeId),
}

/// Validate ViewChange bounds against current view.
///
/// Rejects ViewChanges that are too far in the future to prevent
/// memory exhaustion attacks from accumulating far-future ViewChanges.
pub fn validate_view_change_bounds(
    vc: &ViewChange,
    current_view: View,
) -> Result<(), ViewChangeError> {
    if vc.to_view > current_view.saturating_add(MAX_FUTURE_VIEWS) {
        return Err(ViewChangeError::ViewTooFarAhead {
            vc_view: vc.to_view,
            current_view,
            max_future: MAX_FUTURE_VIEWS,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_view_change(from: View, to: View, sender: u8) -> ViewChange {
        ViewChange {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            from_view: from,
            to_view: to,
            high_qc: None,
            sender: [sender; 32],
            signature: vec![0u8; 64],
        }
    }

    fn dummy_certificate(view: View, block_hash: [u8; 32]) -> Certificate {
        Certificate {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            view,
            block_hash,
            app_hash: Some([0u8; 32]),
            votes: vec![],
            voters: vec![],
            bls_pubkeys: vec![],
            agg_signature: vec![],
        }
    }

    #[test]
    fn test_collector_reaches_quorum() {
        let mut collector = ViewChangeCollector::new(2);

        // First ViewChange - no quorum yet
        let vc1 = make_view_change(5, 6, 1);
        assert!(collector.add(vc1).is_none());
        assert_eq!(collector.count(6), 1);

        // Second ViewChange - reaches quorum
        let vc2 = make_view_change(5, 6, 2);
        let cert = collector.add(vc2);
        assert!(cert.is_some());

        let cert = cert.unwrap();
        assert_eq!(cert.view, 6);
        assert_eq!(cert.view_changes.len(), 2);
    }

    #[test]
    fn test_collector_ignores_duplicates() {
        let mut collector = ViewChangeCollector::new(2);

        let vc1 = make_view_change(5, 6, 1);
        collector.add(vc1.clone());
        collector.add(vc1); // Duplicate

        assert_eq!(collector.count(6), 1);
    }

    #[test]
    fn test_collector_different_views() {
        let mut collector = ViewChangeCollector::new(2);

        // ViewChanges for different target views
        let vc1 = make_view_change(5, 6, 1);
        let vc2 = make_view_change(6, 7, 2);

        collector.add(vc1);
        collector.add(vc2);

        assert_eq!(collector.count(6), 1);
        assert_eq!(collector.count(7), 1);
    }

    #[test]
    fn test_highest_qc_extraction() {
        let mut collector = ViewChangeCollector::new(3);

        // ViewChange with QC at view 10
        let mut vc1 = make_view_change(15, 16, 1);
        vc1.high_qc = Some(dummy_certificate(10, [1u8; 32]));

        // ViewChange with QC at view 12 (higher)
        let mut vc2 = make_view_change(15, 16, 2);
        vc2.high_qc = Some(dummy_certificate(12, [2u8; 32]));

        // ViewChange with no QC
        let vc3 = make_view_change(15, 16, 3);

        collector.add(vc1);
        collector.add(vc2);
        collector.add(vc3);

        let highest = collector.highest_qc(16);
        assert!(highest.is_some());
        assert_eq!(highest.unwrap().view, 12);
    }

    #[test]
    fn test_validate_view_change() {
        // Valid view change
        let valid = make_view_change(5, 6, 1);
        assert!(validate_view_change(&valid).is_ok());

        // Invalid: to_view <= from_view
        let invalid = make_view_change(6, 5, 1);
        assert!(matches!(
            validate_view_change(&invalid),
            Err(ViewChangeError::InvalidViewAdvance { .. })
        ));

        // Invalid: jump too large
        let big_jump = make_view_change(5, 200, 1);
        assert!(matches!(
            validate_view_change(&big_jump),
            Err(ViewChangeError::ViewJumpTooLarge { .. })
        ));
    }

    #[test]
    fn test_prune_below() {
        let mut collector = ViewChangeCollector::new(2);

        collector.add(make_view_change(5, 6, 1));
        collector.add(make_view_change(10, 11, 2));
        collector.add(make_view_change(15, 16, 3));

        // Prune views below 12
        collector.prune_below(12);

        assert_eq!(collector.count(6), 0);
        assert_eq!(collector.count(11), 0);
        assert_eq!(collector.count(16), 1);
    }

    #[test]
    fn test_signed_view_change() {
        use crate::crypto::bls::BlsSecretKey;

        // Generate validator key
        let bls_sk = BlsSecretKey::from_seed(&[1u8; 32]);
        let bls_pk = bls_sk.public_key();
        let sender: NodeId = [1u8; 32];

        // Create signed ViewChange
        let vc = create_signed_view_change(5, 6, None, sender, &bls_sk);

        // Build validator pubkeys map
        let mut pubkeys = HashMap::new();
        pubkeys.insert(sender, bls_pk);

        // Should validate successfully
        assert!(validate_view_change_with_sig(&vc, &pubkeys).is_ok());

        // Tampered view change should fail - change to_view (still valid range but different)
        let mut tampered = vc.clone();
        tampered.to_view = 7; // Change to_view (still passes basic validation: 5 < 7 < 105)
        assert!(matches!(
            validate_view_change_with_sig(&tampered, &pubkeys),
            Err(ViewChangeError::InvalidSignature)
        ));
    }

    #[test]
    fn test_unknown_validator_rejected() {
        use crate::crypto::bls::BlsSecretKey;

        let bls_sk = BlsSecretKey::from_seed(&[1u8; 32]);
        let sender: NodeId = [1u8; 32];
        let vc = create_signed_view_change(5, 6, None, sender, &bls_sk);

        // Empty validator pubkeys map - sender not known
        let pubkeys: HashMap<NodeId, BlsPublicKey> = HashMap::new();

        assert!(matches!(
            validate_view_change_with_sig(&vc, &pubkeys),
            Err(ViewChangeError::UnknownValidator(_))
        ));
    }

    #[test]
    fn test_collector_with_sig_verification() {
        use crate::crypto::bls::BlsSecretKey;

        let bls_sk1 = BlsSecretKey::from_seed(&[1u8; 32]);
        let bls_sk2 = BlsSecretKey::from_seed(&[2u8; 32]);
        let sender1: NodeId = [1u8; 32];
        let sender2: NodeId = [2u8; 32];

        let mut pubkeys = HashMap::new();
        pubkeys.insert(sender1, bls_sk1.public_key());
        pubkeys.insert(sender2, bls_sk2.public_key());

        let mut collector = ViewChangeCollector::with_validators(2, pubkeys);

        // First signed ViewChange
        let vc1 = create_signed_view_change(5, 6, None, sender1, &bls_sk1);
        assert!(collector.add(vc1).is_none());
        assert_eq!(collector.count(6), 1);

        // Second signed ViewChange - reaches quorum
        let vc2 = create_signed_view_change(5, 6, None, sender2, &bls_sk2);
        let cert = collector.add(vc2);
        assert!(cert.is_some());
        assert_eq!(cert.unwrap().view_changes.len(), 2);
    }

    #[test]
    fn committee_view_change_quorum_uses_strict_unequal_power() {
        let secrets: Vec<_> = (1u8..=3)
            .map(|id| {
                let mut seed = [0u8; 32];
                seed[0] = id;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();
        let validators: Vec<NodeId> = (1u8..=3).map(|id| [id; 32]).collect();
        let config = crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: validators[0],
            validators: validators.clone(),
            voting_powers: vec![4, 1, 1],
            view_timeout_ms: 1000,
            bls_pubkeys: secrets
                .iter()
                .map(|secret| secret.public_key().to_bytes().to_vec())
                .collect(),
            bls_secret_key: Some(secrets[0].to_bytes()),
        };
        let context = config.context().unwrap();
        let mut collector =
            ViewChangeCollector::with_committee(config.committee().unwrap()).unwrap();

        // 4/6 is exactly two thirds and must not form a VCC.
        let high_power =
            create_signed_view_change_with_context(context, 5, 6, None, validators[0], &secrets[0]);
        assert!(collector.add(high_power).is_none());

        // 5/6 is strictly above two thirds and must form a VCC.
        let low_power =
            create_signed_view_change_with_context(context, 5, 6, None, validators[1], &secrets[1]);
        let certificate = collector.add(low_power).unwrap();
        assert_eq!(certificate.view, 6);
        assert_eq!(certificate.view_changes.len(), 2);
    }

    #[test]
    fn committee_view_change_rejects_unknown_and_duplicate_signers() {
        let secret1 = BlsSecretKey::from_seed(&[1u8; 32]);
        let secret2 = BlsSecretKey::from_seed(&[2u8; 32]);
        let secret3 = BlsSecretKey::from_seed(&[3u8; 32]);
        let validators = vec![[1u8; 32], [2u8; 32], [3u8; 32]];
        let config = crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: validators[0],
            validators: validators.clone(),
            voting_powers: vec![1, 1, 1],
            view_timeout_ms: 1000,
            bls_pubkeys: vec![
                secret1.public_key().to_bytes().to_vec(),
                secret2.public_key().to_bytes().to_vec(),
                secret3.public_key().to_bytes().to_vec(),
            ],
            bls_secret_key: Some(secret1.to_bytes()),
        };
        let context = config.context().unwrap();
        let mut collector =
            ViewChangeCollector::with_committee(config.committee().unwrap()).unwrap();
        let unknown =
            create_signed_view_change_with_context(context, 5, 6, None, [99u8; 32], &secret1);
        assert!(collector.add(unknown).is_none());

        let valid =
            create_signed_view_change_with_context(context, 5, 6, None, validators[0], &secret1);
        collector.add(valid.clone());
        assert!(collector.add(valid).is_none());
    }

    #[test]
    fn test_collector_rejects_bad_signature() {
        use crate::crypto::bls::BlsSecretKey;

        let bls_sk = BlsSecretKey::from_seed(&[1u8; 32]);
        let sender: NodeId = [1u8; 32];

        let mut pubkeys = HashMap::new();
        pubkeys.insert(sender, bls_sk.public_key());

        let mut collector = ViewChangeCollector::with_validators(1, pubkeys);

        // ViewChange with invalid signature (not matching sender)
        let mut bad_vc = create_signed_view_change(5, 6, None, sender, &bls_sk);
        bad_vc.signature = vec![0u8; 96]; // Invalid signature

        // Should be rejected
        assert!(collector.add(bad_vc).is_none());
        assert_eq!(collector.count(6), 0);
    }

    #[test]
    fn test_validate_view_change_bounds() {
        let current_view = 100;

        // Within bounds (current + 5)
        let valid_vc = make_view_change(104, 105, 1);
        assert!(validate_view_change_bounds(&valid_vc, current_view).is_ok());

        // At the limit (current + MAX_FUTURE_VIEWS)
        let limit_vc = make_view_change(109, 110, 1);
        assert!(validate_view_change_bounds(&limit_vc, current_view).is_ok());

        // Beyond limit (current + MAX_FUTURE_VIEWS + 1)
        let beyond_vc = make_view_change(110, 111, 1);
        assert!(matches!(
            validate_view_change_bounds(&beyond_vc, current_view),
            Err(ViewChangeError::ViewTooFarAhead { .. })
        ));

        // Way beyond limit
        let far_vc = make_view_change(200, 201, 1);
        assert!(matches!(
            validate_view_change_bounds(&far_vc, current_view),
            Err(ViewChangeError::ViewTooFarAhead { .. })
        ));
    }

    #[test]
    fn view_change_from_another_genesis_is_rejected() {
        let secret = BlsSecretKey::from_seed(&[21u8; 32]);
        let local = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let remote = ConsensusContext::with_genesis(0, [7u8; 32], [2u8; 32]);
        let view_change =
            create_signed_view_change_with_context(remote, 1, 2, None, [1u8; 32], &secret);
        let committee = Committee::from_config(&crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: [1u8; 32],
            validators: vec![[1u8; 32]],
            voting_powers: vec![1],
            view_timeout_ms: 1000,
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            bls_secret_key: Some(secret.to_bytes()),
        })
        .expect("one-validator committee");

        assert!(validate_view_change_with_committee_and_context(
            &view_change,
            &committee,
            local,
            1,
        )
        .is_err());
    }
}
