//! Equivocation Detection
//!
//! Detects when validators equivocate (vote for conflicting blocks in same view).
//! Generates evidence that can be submitted to the staking module for slashing.
//!
//! ## Safety Guarantees
//!
//! This module provides Byzantine fault detection - when a validator sends conflicting
//! votes or proposals for the same view, we generate cryptographic evidence that can
//! be used to slash their stake.
//!
//! ## Integration
//!
//! The detector should be called whenever a vote is received. If equivocation is
//! detected, the evidence should be submitted to StakingState::submit_evidence().

use std::collections::HashMap;

use tracing::warn;

use crate::types::{Hash, NodeId, View, Vote};

/// Tracks votes and detects equivocation
pub struct EquivocationDetector {
    /// Votes per (view, voter) -> (block_hash, signature)
    /// If we see a different block_hash for the same (view, voter), that's equivocation
    votes: HashMap<(View, NodeId), (Hash, Vec<u8>)>,

    /// Detected equivocations (view, offender) -> (hash_a, hash_b, sig_a, sig_b)
    equivocations: HashMap<(View, NodeId), EquivocationProof>,

    /// Maximum view to keep (for garbage collection)
    prune_below: View,
}

/// Proof of equivocation (double voting)
#[derive(Debug, Clone)]
pub struct EquivocationProof {
    /// The validator who equivocated
    pub offender: NodeId,
    /// The view where equivocation occurred
    pub view: View,
    /// First block hash voted for
    pub hash_a: Hash,
    /// Second block hash voted for
    pub hash_b: Hash,
    /// Signature on first vote
    pub signature_a: Vec<u8>,
    /// Signature on second vote
    pub signature_b: Vec<u8>,
}

/// Result of checking a vote for equivocation
pub enum VoteCheckResult {
    /// Vote is valid (first vote for this view from this voter)
    Valid,
    /// Duplicate vote (same block hash) - allowed
    Duplicate,
    /// Equivocation detected!
    Equivocation(EquivocationProof),
}

impl EquivocationDetector {
    pub fn new() -> Self {
        Self {
            votes: HashMap::new(),
            equivocations: HashMap::new(),
            prune_below: 0,
        }
    }

    /// Check a vote for equivocation.
    ///
    /// Returns `VoteCheckResult::Equivocation` if this voter already voted for
    /// a different block in this view.
    pub fn check_vote(&mut self, vote: &Vote) -> VoteCheckResult {
        let key = (vote.view, vote.voter);

        if let Some((existing_hash, existing_sig)) = self.votes.get(&key) {
            if *existing_hash == vote.block_hash {
                // Same vote, not equivocation (just duplicate)
                return VoteCheckResult::Duplicate;
            }

            // Different block hash for same (view, voter) = EQUIVOCATION
            let proof = EquivocationProof {
                offender: vote.voter,
                view: vote.view,
                hash_a: *existing_hash,
                hash_b: vote.block_hash,
                signature_a: existing_sig.clone(),
                signature_b: vote.signature.clone(),
            };

            warn!(
                view = vote.view,
                offender = %hex::encode(&vote.voter[..4]),
                hash_a = %hex::encode(&existing_hash[..4]),
                hash_b = %hex::encode(&vote.block_hash[..4]),
                "EQUIVOCATION DETECTED: Double vote!"
            );

            // Record the equivocation
            self.equivocations.insert(key, proof.clone());

            return VoteCheckResult::Equivocation(proof);
        }

        // First vote from this voter for this view - record it
        self.votes
            .insert(key, (vote.block_hash, vote.signature.clone()));

        VoteCheckResult::Valid
    }

    /// Get all detected equivocations
    pub fn get_equivocations(&self) -> Vec<EquivocationProof> {
        self.equivocations.values().cloned().collect()
    }

    /// Take and clear detected equivocations (for processing)
    pub fn take_equivocations(&mut self) -> Vec<EquivocationProof> {
        std::mem::take(&mut self.equivocations)
            .into_values()
            .collect()
    }

    /// Check if a specific validator has equivocated
    pub fn has_equivocated(&self, voter: &NodeId) -> bool {
        self.equivocations.keys().any(|(_, v)| v == voter)
    }

    /// Get equivocation proof for a specific (view, voter) if it exists
    pub fn get_equivocation(&self, view: View, voter: &NodeId) -> Option<&EquivocationProof> {
        self.equivocations.get(&(view, *voter))
    }

    /// Prune old votes to save memory.
    ///
    /// Should be called after commits to remove votes for views that can't
    /// generate new equivocations (already committed).
    pub fn prune_below(&mut self, view: View) {
        if view <= self.prune_below {
            return;
        }
        self.prune_below = view;
        self.votes.retain(|(v, _), _| *v >= view);
        // Keep equivocations longer (they're evidence for slashing)
        // Only prune very old ones
        if view > 1000 {
            self.equivocations
                .retain(|(v, _), _| *v >= view.saturating_sub(1000));
        }
    }

    /// Get statistics for monitoring
    pub fn stats(&self) -> EquivocationStats {
        EquivocationStats {
            tracked_votes: self.votes.len(),
            detected_equivocations: self.equivocations.len(),
            unique_offenders: self
                .equivocations
                .values()
                .map(|e| e.offender)
                .collect::<std::collections::HashSet<_>>()
                .len(),
        }
    }
}

impl Default for EquivocationDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about equivocation detection
#[derive(Debug, Clone)]
pub struct EquivocationStats {
    /// Number of votes being tracked
    pub tracked_votes: usize,
    /// Number of equivocations detected
    pub detected_equivocations: usize,
    /// Number of unique offenders
    pub unique_offenders: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vote(view: View, voter_id: u8, block_hash: u8) -> Vote {
        let mut voter = [0u8; 32];
        voter[0] = voter_id;

        let mut hash = [0u8; 32];
        hash[0] = block_hash;

        Vote {
            view,
            block_hash: hash,
            app_hash: [0u8; 32],
            voter,
            signature: vec![voter_id, block_hash],
            bls_pubkey: None,
        }
    }

    #[test]
    fn test_first_vote_valid() {
        let mut detector = EquivocationDetector::new();
        let vote = test_vote(1, 1, 10);

        match detector.check_vote(&vote) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result"),
        }
    }

    #[test]
    fn test_duplicate_vote_allowed() {
        let mut detector = EquivocationDetector::new();
        let vote = test_vote(1, 1, 10);

        detector.check_vote(&vote);
        match detector.check_vote(&vote) {
            VoteCheckResult::Duplicate => {}
            _ => panic!("Expected Duplicate result"),
        }
    }

    #[test]
    fn test_equivocation_detected() {
        let mut detector = EquivocationDetector::new();

        // First vote for block 10
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Second vote for different block 20 in same view
        let vote2 = test_vote(1, 1, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Equivocation(proof) => {
                assert_eq!(proof.view, 1);
                assert_eq!(proof.hash_a[0], 10);
                assert_eq!(proof.hash_b[0], 20);
            }
            _ => panic!("Expected Equivocation result"),
        }

        assert!(detector.has_equivocated(&vote1.voter));
    }

    #[test]
    fn test_different_views_not_equivocation() {
        let mut detector = EquivocationDetector::new();

        // Vote for block 10 in view 1
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Vote for different block 20 in view 2 (different view, OK)
        let vote2 = test_vote(2, 1, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result for different view"),
        }
    }

    #[test]
    fn test_different_voters_not_equivocation() {
        let mut detector = EquivocationDetector::new();

        // Voter 1 votes for block 10
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Voter 2 votes for different block 20 (different voter, OK)
        let vote2 = test_vote(1, 2, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result for different voter"),
        }
    }

    #[test]
    fn test_prune_old_votes() {
        let mut detector = EquivocationDetector::new();

        // Add votes for views 1, 2, 3
        for view in 1..=3 {
            let vote = test_vote(view, 1, view as u8);
            detector.check_vote(&vote);
        }

        assert_eq!(detector.votes.len(), 3);

        // Prune below view 3
        detector.prune_below(3);
        assert_eq!(detector.votes.len(), 1);
    }

    #[test]
    fn test_take_equivocations() {
        let mut detector = EquivocationDetector::new();

        // Create equivocation
        detector.check_vote(&test_vote(1, 1, 10));
        detector.check_vote(&test_vote(1, 1, 20));

        let equivocations = detector.take_equivocations();
        assert_eq!(equivocations.len(), 1);

        // Should be empty after taking
        assert!(detector.take_equivocations().is_empty());
    }
}
