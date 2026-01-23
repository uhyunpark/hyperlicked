//! Timeout Certificate Collection and Aggregation
//!
//! Implements BLS signature aggregation for timeout messages to form
//! TimeoutCertificates (TCs) - cryptographic proof of leader failure.
//!
//! ## Why TCs Matter
//!
//! Without TCs, a malicious validator could claim a timeout occurred when it didn't.
//! TCs provide Byzantine safety by requiring 2f+1 validators to sign the timeout.
//!
//! ## Protocol
//!
//! 1. Validator times out → signs Timeout message with BLS
//! 2. Broadcasts Timeout to network
//! 3. Collector aggregates 2f+1 signatures into TC
//! 4. TC proves to all validators that view change is legitimate

use std::collections::HashMap;
use std::sync::Arc;

use crate::crypto::bls::{aggregate_signatures, BlsPublicKey, BlsSecretKey, BlsSignature};
use crate::types::{NodeId, Timeout, TimeoutCertificate, View};

use super::metrics::ConsensusMetrics;

/// Errors that can occur during timeout collection
#[derive(Debug, Clone, thiserror::Error)]
pub enum TimeoutError {
    #[error("invalid view: expected {expected}, got {got}")]
    InvalidView { expected: View, got: View },

    #[error("unknown validator: {0:?}")]
    UnknownValidator(NodeId),

    #[error("invalid signature")]
    InvalidSignature,

    #[error("duplicate timeout from {0:?}")]
    DuplicateTimeout(NodeId),
}

/// Collects Timeout messages and forms TimeoutCertificates
pub struct TimeoutCollector {
    /// Timeouts per view: view -> (sender -> Timeout)
    timeouts: HashMap<View, HashMap<NodeId, Timeout>>,

    /// Quorum size needed (2f+1)
    quorum: usize,

    /// Validator public keys for signature verification
    validator_pubkeys: HashMap<NodeId, BlsPublicKey>,

    /// Optional metrics for tracking Byzantine events
    metrics: Option<Arc<ConsensusMetrics>>,
}

impl TimeoutCollector {
    /// Create a new collector with validator public keys
    pub fn new(quorum: usize, validator_pubkeys: HashMap<NodeId, BlsPublicKey>) -> Self {
        Self {
            timeouts: HashMap::new(),
            quorum,
            validator_pubkeys,
            metrics: None,
        }
    }

    /// Create a new collector without signature verification (for testing)
    #[cfg(test)]
    pub fn new_without_verification(quorum: usize) -> Self {
        Self {
            timeouts: HashMap::new(),
            quorum,
            validator_pubkeys: HashMap::new(),
            metrics: None,
        }
    }

    /// Set metrics tracker for Byzantine event monitoring
    pub fn with_metrics(mut self, metrics: Arc<ConsensusMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Add a Timeout message.
    ///
    /// Returns TimeoutCertificate if quorum reached for the view.
    pub fn add(&mut self, timeout: Timeout) -> Result<Option<TimeoutCertificate>, TimeoutError> {
        let view = timeout.view;
        let sender = timeout.sender;

        // Verify signature if we have validator pubkeys
        if !self.validator_pubkeys.is_empty() {
            self.verify_timeout(&timeout)?;
        }

        let view_map = self.timeouts.entry(view).or_default();

        // Don't accept duplicate from same sender
        if view_map.contains_key(&sender) {
            return Err(TimeoutError::DuplicateTimeout(sender));
        }

        view_map.insert(sender, timeout);

        // Check if we've reached quorum
        if view_map.len() >= self.quorum {
            Ok(Some(self.create_tc(view)))
        } else {
            Ok(None)
        }
    }

    /// Verify a timeout message signature
    fn verify_timeout(&self, timeout: &Timeout) -> Result<(), TimeoutError> {
        let pubkey = self.validator_pubkeys.get(&timeout.sender).ok_or_else(|| {
            // Record metric for unknown validator
            if let Some(ref metrics) = self.metrics {
                metrics.record_unknown_validator(&timeout.sender);
            }
            TimeoutError::UnknownValidator(timeout.sender)
        })?;

        if timeout.signature.len() != 96 {
            // Record metric for parse failure
            if let Some(ref metrics) = self.metrics {
                metrics.record_signature_parse_failure();
            }
            return Err(TimeoutError::InvalidSignature);
        }

        let sig = BlsSignature::from_slice(&timeout.signature).map_err(|_| {
            if let Some(ref metrics) = self.metrics {
                metrics.record_signature_parse_failure();
            }
            TimeoutError::InvalidSignature
        })?;

        let signing_data = timeout.signing_data();
        if !pubkey.verify(&signing_data, &sig) {
            // Record metric for invalid signature - POTENTIAL BYZANTINE FAULT
            tracing::warn!(
                view = timeout.view,
                sender = %hex::encode(&timeout.sender[..4]),
                "Rejecting timeout with invalid BLS signature - POTENTIAL BYZANTINE FAULT"
            );
            if let Some(ref metrics) = self.metrics {
                metrics.record_invalid_timeout_signature(&timeout.sender);
            }
            return Err(TimeoutError::InvalidSignature);
        }

        Ok(())
    }

    /// Create a TimeoutCertificate from collected timeouts
    fn create_tc(&self, view: View) -> TimeoutCertificate {
        let timeouts = self.timeouts.get(&view).unwrap();

        // Find highest high_qc_view
        let high_qc_view = timeouts.values().map(|t| t.high_qc_view).max().unwrap_or(0);

        // Collect signers and signatures
        let mut signers = Vec::new();
        let mut signatures = Vec::new();

        for timeout in timeouts.values() {
            signers.push(timeout.sender);
            if timeout.signature.len() == 96 {
                if let Ok(sig) = BlsSignature::from_slice(&timeout.signature) {
                    signatures.push(sig);
                }
            }
        }

        // Aggregate signatures if we have valid ones
        let agg_signature = if signatures.len() >= self.quorum {
            match aggregate_signatures(&signatures) {
                Ok(agg) => agg.to_bytes().to_vec(),
                Err(_) => {
                    // Fallback: concatenate individual signatures
                    timeouts
                        .values()
                        .flat_map(|t| t.signature.iter().copied())
                        .collect()
                }
            }
        } else {
            // Fallback: concatenate individual signatures
            timeouts
                .values()
                .flat_map(|t| t.signature.iter().copied())
                .collect()
        };

        TimeoutCertificate {
            view,
            high_qc_view,
            signers,
            agg_signature,
        }
    }

    /// Get the count of timeouts for a view
    pub fn count(&self, view: View) -> usize {
        self.timeouts.get(&view).map(|m| m.len()).unwrap_or(0)
    }

    /// Clear all collected timeouts
    pub fn clear(&mut self) {
        self.timeouts.clear();
    }

    /// Remove timeouts for views below the given view
    pub fn prune_below(&mut self, view: View) {
        self.timeouts.retain(|&v, _| v >= view);
    }
}

/// Create a signed Timeout message
pub fn create_signed_timeout(
    view: View,
    high_qc_view: View,
    sender: NodeId,
    bls_sk: &BlsSecretKey,
) -> Timeout {
    let mut timeout = Timeout {
        view,
        high_qc_view,
        sender,
        signature: vec![0u8; 96],
    };

    let signing_data = timeout.signing_data();
    let sig = bls_sk.sign(&signing_data);
    timeout.signature = sig.to_bytes().to_vec();

    timeout
}

/// Verify a TimeoutCertificate
///
/// Returns true if the aggregated signature is valid for the given validators.
pub fn verify_timeout_certificate(
    tc: &TimeoutCertificate,
    validator_pubkeys: &HashMap<NodeId, BlsPublicKey>,
) -> bool {
    // Need quorum of signers
    if tc.signers.is_empty() {
        return false;
    }

    // Collect public keys (cloned for verify_aggregate)
    let pubkeys: Vec<BlsPublicKey> = tc
        .signers
        .iter()
        .filter_map(|id| validator_pubkeys.get(id).cloned())
        .collect();

    if pubkeys.len() != tc.signers.len() {
        return false; // Some signers unknown
    }

    // Parse aggregated signature
    if tc.agg_signature.len() != 96 {
        return false;
    }

    let agg_sig = match BlsSignature::from_slice(&tc.agg_signature) {
        Ok(sig) => sig,
        Err(_) => return false,
    };

    // Verify aggregated signature (order: message, signature, public_keys)
    let signing_data = tc.signing_data();
    crate::crypto::bls::verify_aggregate(&signing_data, &agg_sig, &pubkeys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timeout_collection() {
        let mut collector = TimeoutCollector::new_without_verification(2);

        // First timeout - no quorum yet
        let t1 = Timeout {
            view: 5,
            high_qc_view: 3,
            sender: [1u8; 32],
            signature: vec![0u8; 64],
        };
        assert!(collector.add(t1).unwrap().is_none());
        assert_eq!(collector.count(5), 1);

        // Second timeout - reaches quorum
        let t2 = Timeout {
            view: 5,
            high_qc_view: 4,
            sender: [2u8; 32],
            signature: vec![0u8; 64],
        };
        let tc = collector.add(t2).unwrap();
        assert!(tc.is_some());

        let tc = tc.unwrap();
        assert_eq!(tc.view, 5);
        assert_eq!(tc.high_qc_view, 4); // Highest among timeouts
        assert_eq!(tc.signers.len(), 2);
    }

    #[test]
    fn test_duplicate_timeout_rejected() {
        let mut collector = TimeoutCollector::new_without_verification(2);

        let t1 = Timeout {
            view: 5,
            high_qc_view: 3,
            sender: [1u8; 32],
            signature: vec![0u8; 64],
        };
        collector.add(t1.clone()).unwrap();

        // Duplicate from same sender
        let result = collector.add(t1);
        assert!(matches!(result, Err(TimeoutError::DuplicateTimeout(_))));
    }

    #[test]
    fn test_signed_timeout() {
        let bls_sk = BlsSecretKey::from_seed(&[1u8; 32]);
        let bls_pk = bls_sk.public_key();
        let sender: NodeId = [1u8; 32];

        // Create signed timeout
        let timeout = create_signed_timeout(5, 3, sender, &bls_sk);

        // Verify signature manually
        let sig = BlsSignature::from_slice(&timeout.signature).unwrap();
        let signing_data = timeout.signing_data();
        assert!(bls_pk.verify(&signing_data, &sig));
    }

    #[test]
    fn test_timeout_certificate_with_bls() {
        let bls_sk1 = BlsSecretKey::from_seed(&[1u8; 32]);
        let bls_sk2 = BlsSecretKey::from_seed(&[2u8; 32]);
        let sender1: NodeId = [1u8; 32];
        let sender2: NodeId = [2u8; 32];

        let mut pubkeys = HashMap::new();
        pubkeys.insert(sender1, bls_sk1.public_key());
        pubkeys.insert(sender2, bls_sk2.public_key());

        let mut collector = TimeoutCollector::new(2, pubkeys.clone());

        // Add signed timeouts
        let t1 = create_signed_timeout(5, 3, sender1, &bls_sk1);
        assert!(collector.add(t1).unwrap().is_none());

        let t2 = create_signed_timeout(5, 4, sender2, &bls_sk2);
        let tc = collector.add(t2).unwrap();
        assert!(tc.is_some());

        let tc = tc.unwrap();
        assert_eq!(tc.view, 5);
        assert_eq!(tc.high_qc_view, 4);

        // Verify the TC
        assert!(verify_timeout_certificate(&tc, &pubkeys));
    }

    #[test]
    fn test_prune_below() {
        let mut collector = TimeoutCollector::new_without_verification(2);

        let t1 = Timeout {
            view: 5,
            high_qc_view: 3,
            sender: [1u8; 32],
            signature: vec![0u8; 64],
        };
        let t2 = Timeout {
            view: 10,
            high_qc_view: 8,
            sender: [2u8; 32],
            signature: vec![0u8; 64],
        };

        collector.add(t1).unwrap();
        collector.add(t2).unwrap();

        collector.prune_below(8);

        assert_eq!(collector.count(5), 0);
        assert_eq!(collector.count(10), 1);
    }
}
