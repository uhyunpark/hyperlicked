//! Equivocation Detection Tests
//!
//! Tests for Byzantine fault detection: equivocation (double voting).

use std::collections::HashMap;

use hyperlicked::consensus::{
    EquivocationDetector, EquivocationProof, ConsensusMetrics, VoteCheckResult,
};
use hyperlicked::types::{NodeId, Vote};

fn test_node_id(n: u8) -> NodeId {
    let mut id = [0u8; 32];
    id[0] = n;
    id
}

fn test_vote(view: u64, voter_id: u8, block_hash_byte: u8) -> Vote {
    let mut voter = [0u8; 32];
    voter[0] = voter_id;

    let mut hash = [0u8; 32];
    hash[0] = block_hash_byte;

    Vote {
        view,
        block_hash: hash,
        app_hash: [0u8; 32],
        voter,
        signature: vec![voter_id, block_hash_byte], // Simplified signature for testing
        bls_pubkey: None,
    }
}

// =============================================================================
// Equivocation Detection Tests
// =============================================================================

/// Test that first vote is accepted as valid
#[test]
fn test_first_vote_valid() {
    let mut detector = EquivocationDetector::new();
    let vote = test_vote(1, 1, 10);

    match detector.check_vote(&vote) {
        VoteCheckResult::Valid => {}
        _ => panic!("First vote should be Valid"),
    }
}

/// Test that duplicate vote (same hash) is allowed but flagged
#[test]
fn test_duplicate_vote_allowed() {
    let mut detector = EquivocationDetector::new();
    let vote = test_vote(1, 1, 10);

    detector.check_vote(&vote);
    match detector.check_vote(&vote) {
        VoteCheckResult::Duplicate => {}
        _ => panic!("Duplicate vote should be Duplicate"),
    }
}

/// Test that equivocation is detected
#[test]
fn test_equivocation_detected() {
    let mut detector = EquivocationDetector::new();

    // Vote for block hash 10
    let vote1 = test_vote(1, 1, 10);
    detector.check_vote(&vote1);

    // Vote for different block hash 20 in SAME view = EQUIVOCATION
    let vote2 = test_vote(1, 1, 20);
    match detector.check_vote(&vote2) {
        VoteCheckResult::Equivocation(proof) => {
            assert_eq!(proof.view, 1);
            assert_eq!(proof.offender, vote1.voter);
            assert_eq!(proof.hash_a[0], 10);
            assert_eq!(proof.hash_b[0], 20);
        }
        _ => panic!("Should detect equivocation"),
    }

    // Verify offender is tracked
    assert!(detector.has_equivocated(&vote1.voter));
}

/// Test that different views don't trigger equivocation
#[test]
fn test_different_views_not_equivocation() {
    let mut detector = EquivocationDetector::new();

    // Vote for hash 10 in view 1
    let vote1 = test_vote(1, 1, 10);
    detector.check_vote(&vote1);

    // Vote for different hash 20 in view 2 (different view = OK)
    let vote2 = test_vote(2, 1, 20);
    match detector.check_vote(&vote2) {
        VoteCheckResult::Valid => {}
        _ => panic!("Different view should be Valid, not equivocation"),
    }

    assert!(!detector.has_equivocated(&vote1.voter));
}

/// Test that different voters don't trigger equivocation
#[test]
fn test_different_voters_not_equivocation() {
    let mut detector = EquivocationDetector::new();

    // Voter 1 votes for hash 10
    let vote1 = test_vote(1, 1, 10);
    detector.check_vote(&vote1);

    // Voter 2 votes for different hash 20 (different voter = OK)
    let vote2 = test_vote(1, 2, 20);
    match detector.check_vote(&vote2) {
        VoteCheckResult::Valid => {}
        _ => panic!("Different voter should be Valid"),
    }
}

/// Test equivocation statistics
#[test]
fn test_equivocation_stats() {
    let mut detector = EquivocationDetector::new();

    // Add some votes
    detector.check_vote(&test_vote(1, 1, 10));
    detector.check_vote(&test_vote(1, 2, 10));
    detector.check_vote(&test_vote(1, 3, 10));

    let stats = detector.stats();
    assert_eq!(stats.tracked_votes, 3);
    assert_eq!(stats.detected_equivocations, 0);

    // Cause equivocation
    detector.check_vote(&test_vote(1, 1, 20)); // Voter 1 votes again

    let stats = detector.stats();
    assert_eq!(stats.detected_equivocations, 1);
    assert_eq!(stats.unique_offenders, 1);
}

/// Test take_equivocations clears the list
#[test]
fn test_take_equivocations() {
    let mut detector = EquivocationDetector::new();

    // Create equivocation
    detector.check_vote(&test_vote(1, 1, 10));
    detector.check_vote(&test_vote(1, 1, 20));

    let equivocations = detector.take_equivocations();
    assert_eq!(equivocations.len(), 1);

    // Should be empty after taking
    let equivocations2 = detector.take_equivocations();
    assert!(equivocations2.is_empty());
}

/// Test pruning old votes
#[test]
fn test_prune_old_votes() {
    let mut detector = EquivocationDetector::new();

    // Add votes in views 1, 5, 10
    detector.check_vote(&test_vote(1, 1, 10));
    detector.check_vote(&test_vote(5, 1, 20));
    detector.check_vote(&test_vote(10, 1, 30));

    assert_eq!(detector.stats().tracked_votes, 3);

    // Prune below view 5
    detector.prune_below(5);

    // Only views >= 5 should remain
    let stats = detector.stats();
    assert_eq!(stats.tracked_votes, 2);
}

/// Test multiple equivocations from different validators
#[test]
fn test_multiple_equivocators() {
    let mut detector = EquivocationDetector::new();

    // Validator 1 equivocates
    detector.check_vote(&test_vote(1, 1, 10));
    detector.check_vote(&test_vote(1, 1, 11));

    // Validator 2 equivocates
    detector.check_vote(&test_vote(2, 2, 20));
    detector.check_vote(&test_vote(2, 2, 21));

    // Validator 3 does NOT equivocate
    detector.check_vote(&test_vote(3, 3, 30));

    let stats = detector.stats();
    assert_eq!(stats.detected_equivocations, 2);
    assert_eq!(stats.unique_offenders, 2);

    assert!(detector.has_equivocated(&test_node_id(1)));
    assert!(detector.has_equivocated(&test_node_id(2)));
    assert!(!detector.has_equivocated(&test_node_id(3)));
}

// =============================================================================
// Consensus Metrics Tests
// =============================================================================

/// Test metrics tracking for invalid signatures
#[test]
fn test_metrics_invalid_signatures() {
    let metrics = ConsensusMetrics::new();

    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_invalid_vote_signature(&test_node_id(2));

    let summary = metrics.summary();
    assert_eq!(summary.invalid_vote_signatures, 3);
    assert_eq!(summary.suspicious_validators, 2);
}

/// Test metrics tracking for timeout signatures
#[test]
fn test_metrics_invalid_timeout_signatures() {
    let metrics = ConsensusMetrics::new();

    metrics.record_invalid_timeout_signature(&test_node_id(1));
    metrics.record_invalid_timeout_signature(&test_node_id(2));

    let summary = metrics.summary();
    assert_eq!(summary.invalid_timeout_signatures, 2);
    assert!(summary.has_byzantine_activity());
}

/// Test highly suspicious validator detection
#[test]
fn test_highly_suspicious_threshold() {
    let metrics = ConsensusMetrics::new();
    let node = test_node_id(1);

    // Record 4 invalid signatures
    for _ in 0..4 {
        metrics.record_invalid_vote_signature(&node);
    }

    assert!(!metrics.is_highly_suspicious(&node, 5));

    metrics.record_invalid_vote_signature(&node);
    assert!(metrics.is_highly_suspicious(&node, 5));
}

/// Test suspicious validators list
#[test]
fn test_get_suspicious_validators() {
    let metrics = ConsensusMetrics::new();

    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_invalid_vote_signature(&test_node_id(2));

    let suspicious = metrics.get_suspicious_validators();
    assert_eq!(suspicious.len(), 2);

    // Find node 1's count
    let node1_count = suspicious
        .iter()
        .find(|(id, _)| id[0] == 1)
        .map(|(_, c)| *c);
    assert_eq!(node1_count, Some(2));
}

/// Test metrics reset
#[test]
fn test_metrics_reset() {
    let metrics = ConsensusMetrics::new();

    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_aggregation_failure();

    assert!(metrics.summary().invalid_vote_signatures > 0);

    metrics.reset();

    let summary = metrics.summary();
    assert_eq!(summary.invalid_vote_signatures, 0);
    assert_eq!(summary.aggregation_failures, 0);
    assert!(!summary.has_byzantine_activity());
}

/// Test total signature failures calculation
#[test]
fn test_total_signature_failures() {
    let metrics = ConsensusMetrics::new();

    metrics.record_invalid_vote_signature(&test_node_id(1));
    metrics.record_invalid_vote_signature(&test_node_id(2));
    metrics.record_invalid_timeout_signature(&test_node_id(3));
    metrics.record_signature_parse_failure();

    let summary = metrics.summary();
    assert_eq!(summary.total_signature_failures(), 4);
}
