//! Consensus Metrics
//!
//! Tracks Byzantine events and signature failures for monitoring and alerting.
//!
//! ## Usage
//!
//! ```ignore
//! use consensus::metrics::ConsensusMetrics;
//!
//! let metrics = ConsensusMetrics::new();
//!
//! // Record events as they occur
//! metrics.record_invalid_vote_signature(voter);
//! metrics.record_invalid_timeout_signature(sender);
//!
//! // Get summary for monitoring
//! let summary = metrics.summary();
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::types::NodeId;

/// Tracks consensus metrics and Byzantine events
#[derive(Default)]
pub struct ConsensusMetrics {
    /// Count of votes with invalid BLS signatures
    invalid_vote_signatures: AtomicU64,

    /// Count of timeouts with invalid signatures
    invalid_timeout_signatures: AtomicU64,

    /// Count of unknown validators (votes from non-validators)
    unknown_validators: AtomicU64,

    /// Count of signature parse failures
    signature_parse_failures: AtomicU64,

    /// Count of aggregation failures
    aggregation_failures: AtomicU64,

    /// Count of rate-limited votes (CRITICAL-7)
    rate_limited_votes: AtomicU64,

    /// Invalid signatures per validator (for identifying Byzantine nodes)
    /// Note: This is a simple counter map, not thread-safe for concurrent access
    /// In production, use proper metrics library like prometheus
    byzantine_suspicions: std::sync::RwLock<HashMap<NodeId, u64>>,
}

impl ConsensusMetrics {
    /// Create new metrics tracker
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an invalid vote signature
    pub fn record_invalid_vote_signature(&self, voter: &NodeId) {
        self.invalid_vote_signatures.fetch_add(1, Ordering::Relaxed);
        self.increment_byzantine_suspicion(voter);
    }

    /// Record an invalid timeout signature
    pub fn record_invalid_timeout_signature(&self, sender: &NodeId) {
        self.invalid_timeout_signatures.fetch_add(1, Ordering::Relaxed);
        self.increment_byzantine_suspicion(sender);
    }

    /// Record a vote/timeout from unknown validator
    pub fn record_unknown_validator(&self, node_id: &NodeId) {
        self.unknown_validators.fetch_add(1, Ordering::Relaxed);
        // Don't track as Byzantine - could be configuration issue
        tracing::warn!(
            node = %hex::encode(&node_id[..4]),
            "Message from unknown validator"
        );
    }

    /// Record a signature parse failure
    pub fn record_signature_parse_failure(&self) {
        self.signature_parse_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record an aggregation failure
    pub fn record_aggregation_failure(&self) {
        self.aggregation_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a rate-limited vote (CRITICAL-7: vote spam prevention)
    pub fn record_vote_rate_limited(&self, voter: &NodeId) {
        self.rate_limited_votes.fetch_add(1, Ordering::Relaxed);
        // Rate limiting could indicate spam attack from Byzantine node
        self.increment_byzantine_suspicion(voter);
    }

    /// Increment Byzantine suspicion counter for a node
    fn increment_byzantine_suspicion(&self, node_id: &NodeId) {
        if let Ok(mut map) = self.byzantine_suspicions.write() {
            *map.entry(*node_id).or_insert(0) += 1;
        }
    }

    /// Get summary of metrics
    pub fn summary(&self) -> MetricsSummary {
        let byzantine_nodes: Vec<NodeId> = self
            .byzantine_suspicions
            .read()
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();

        MetricsSummary {
            invalid_vote_signatures: self.invalid_vote_signatures.load(Ordering::Relaxed),
            invalid_timeout_signatures: self.invalid_timeout_signatures.load(Ordering::Relaxed),
            unknown_validators: self.unknown_validators.load(Ordering::Relaxed),
            signature_parse_failures: self.signature_parse_failures.load(Ordering::Relaxed),
            aggregation_failures: self.aggregation_failures.load(Ordering::Relaxed),
            rate_limited_votes: self.rate_limited_votes.load(Ordering::Relaxed),
            suspicious_validators: byzantine_nodes.len(),
        }
    }

    /// Get nodes with Byzantine suspicions (potential slashing candidates)
    pub fn get_suspicious_validators(&self) -> Vec<(NodeId, u64)> {
        self.byzantine_suspicions
            .read()
            .map(|m| m.iter().map(|(k, v)| (*k, *v)).collect())
            .unwrap_or_default()
    }

    /// Check if a node has accumulated too many suspicions
    pub fn is_highly_suspicious(&self, node_id: &NodeId, threshold: u64) -> bool {
        self.byzantine_suspicions
            .read()
            .map(|m| m.get(node_id).copied().unwrap_or(0) >= threshold)
            .unwrap_or(false)
    }

    /// Reset all counters (for testing)
    pub fn reset(&self) {
        self.invalid_vote_signatures.store(0, Ordering::Relaxed);
        self.invalid_timeout_signatures.store(0, Ordering::Relaxed);
        self.unknown_validators.store(0, Ordering::Relaxed);
        self.signature_parse_failures.store(0, Ordering::Relaxed);
        self.aggregation_failures.store(0, Ordering::Relaxed);
        self.rate_limited_votes.store(0, Ordering::Relaxed);
        if let Ok(mut map) = self.byzantine_suspicions.write() {
            map.clear();
        }
    }
}

/// Summary of consensus metrics
#[derive(Debug, Clone, Default)]
pub struct MetricsSummary {
    /// Total invalid vote signatures
    pub invalid_vote_signatures: u64,
    /// Total invalid timeout signatures
    pub invalid_timeout_signatures: u64,
    /// Total messages from unknown validators
    pub unknown_validators: u64,
    /// Total signature parse failures
    pub signature_parse_failures: u64,
    /// Total aggregation failures
    pub aggregation_failures: u64,
    /// Total rate-limited votes (CRITICAL-7)
    pub rate_limited_votes: u64,
    /// Number of validators with Byzantine suspicions
    pub suspicious_validators: usize,
}

impl MetricsSummary {
    /// Check if any Byzantine activity detected
    pub fn has_byzantine_activity(&self) -> bool {
        self.invalid_vote_signatures > 0
            || self.invalid_timeout_signatures > 0
            || self.suspicious_validators > 0
    }

    /// Total signature failures
    pub fn total_signature_failures(&self) -> u64 {
        self.invalid_vote_signatures
            + self.invalid_timeout_signatures
            + self.signature_parse_failures
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_invalid_vote_tracking() {
        let metrics = ConsensusMetrics::new();

        metrics.record_invalid_vote_signature(&test_node(1));
        metrics.record_invalid_vote_signature(&test_node(1));
        metrics.record_invalid_vote_signature(&test_node(2));

        let summary = metrics.summary();
        assert_eq!(summary.invalid_vote_signatures, 3);
        assert_eq!(summary.suspicious_validators, 2);
    }

    #[test]
    fn test_byzantine_suspicion_threshold() {
        let metrics = ConsensusMetrics::new();
        let node = test_node(1);

        // Record 4 invalid signatures
        for _ in 0..4 {
            metrics.record_invalid_vote_signature(&node);
        }

        assert!(!metrics.is_highly_suspicious(&node, 5));
        metrics.record_invalid_vote_signature(&node);
        assert!(metrics.is_highly_suspicious(&node, 5));
    }

    #[test]
    fn test_summary_has_byzantine_activity() {
        let metrics = ConsensusMetrics::new();

        let summary = metrics.summary();
        assert!(!summary.has_byzantine_activity());

        metrics.record_invalid_timeout_signature(&test_node(1));

        let summary = metrics.summary();
        assert!(summary.has_byzantine_activity());
    }

    #[test]
    fn test_reset() {
        let metrics = ConsensusMetrics::new();

        metrics.record_invalid_vote_signature(&test_node(1));
        metrics.record_aggregation_failure();

        assert!(metrics.summary().invalid_vote_signatures > 0);

        metrics.reset();

        let summary = metrics.summary();
        assert_eq!(summary.invalid_vote_signatures, 0);
        assert_eq!(summary.aggregation_failures, 0);
    }
}
