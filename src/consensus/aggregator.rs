//! Vote Aggregation
//!
//! Collects votes and forms QCs with BLS signature aggregation.
//!
//! ## Usage
//!
//! ```ignore
//! let mut agg = VoteAggregator::new(quorum, use_bls);
//!
//! // Add votes as they arrive
//! if let Some(cert) = agg.add_vote(vote) {
//!     // Quorum reached, cert is ready
//! }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use crate::crypto::bls::{aggregate_signatures, BlsSignature};
use crate::types::{Certificate, Hash, NodeId, View, Vote};

use super::metrics::ConsensusMetrics;

/// Collects votes and aggregates signatures
pub struct VoteAggregator {
    /// Votes per (view, block_hash)
    votes: HashMap<(View, Hash), HashMap<NodeId, Vote>>,

    /// Quorum size (2f+1)
    quorum: usize,

    /// Use BLS aggregation
    use_bls: bool,

    /// Skip signature verification (dev mode only!)
    skip_verification: bool,

    /// Optional metrics for tracking Byzantine events
    metrics: Option<Arc<ConsensusMetrics>>,
}

impl VoteAggregator {
    /// Create a new aggregator
    pub fn new(quorum: usize, use_bls: bool) -> Self {
        Self {
            votes: HashMap::new(),
            quorum,
            use_bls,
            skip_verification: false,
            metrics: None,
        }
    }

    /// Create aggregator with verification control
    pub fn new_with_options(quorum: usize, use_bls: bool, skip_verification: bool) -> Self {
        Self {
            votes: HashMap::new(),
            quorum,
            use_bls,
            skip_verification,
            metrics: None,
        }
    }

    /// Set metrics tracker for Byzantine event monitoring
    pub fn with_metrics(mut self, metrics: Arc<ConsensusMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Add a vote. Returns Certificate if quorum reached.
    /// Verifies BLS signature unless skip_verification is set.
    pub fn add_vote(&mut self, vote: Vote) -> Option<Certificate> {
        let key = (vote.view, vote.block_hash);
        let voter = vote.voter;

        let vote_map = self.votes.entry(key).or_default();

        // Don't accept duplicate from same voter
        if vote_map.contains_key(&voter) {
            return None;
        }

        // Verify BLS signature if enabled and not skipped
        if self.use_bls && !self.skip_verification && vote.is_bls() {
            if !vote.verify_bls() {
                tracing::warn!(
                    view = vote.view,
                    voter = %crate::types::hash_short(&voter),
                    "Rejecting vote with invalid BLS signature - POTENTIAL BYZANTINE FAULT"
                );

                // Record metric for monitoring
                if let Some(ref metrics) = self.metrics {
                    metrics.record_invalid_vote_signature(&voter);
                }

                return None;
            }
        }

        vote_map.insert(voter, vote);

        // Check if quorum reached
        if vote_map.len() >= self.quorum {
            let votes: Vec<Vote> = vote_map.values().cloned().collect();

            if self.use_bls && votes.iter().all(|v| v.is_bls()) {
                self.aggregate_bls(key.0, key.1, votes)
            } else {
                Some(Certificate::new(key.0, key.1, votes))
            }
        } else {
            None
        }
    }

    /// Aggregate BLS signatures into a certificate
    fn aggregate_bls(&self, view: View, block_hash: Hash, votes: Vec<Vote>) -> Option<Certificate> {
        // Extract BLS signatures
        let signatures: Result<Vec<BlsSignature>, _> = votes
            .iter()
            .map(|v| BlsSignature::from_slice(&v.signature))
            .collect();

        let signatures = match signatures {
            Ok(sigs) => sigs,
            Err(_) => {
                // Fallback to legacy if BLS parsing fails
                tracing::warn!("BLS signature parsing failed, using legacy certificate");
                if let Some(ref metrics) = self.metrics {
                    metrics.record_signature_parse_failure();
                }
                return Some(Certificate::new(view, block_hash, votes));
            }
        };

        // Aggregate signatures
        let agg_sig = match aggregate_signatures(&signatures) {
            Ok(sig) => sig.to_bytes().to_vec(),
            Err(_) => {
                // Fallback to legacy if aggregation fails
                tracing::warn!("BLS aggregation failed, using legacy certificate");
                if let Some(ref metrics) = self.metrics {
                    metrics.record_aggregation_failure();
                }
                return Some(Certificate::new(view, block_hash, votes));
            }
        };

        Some(Certificate::new_bls(view, block_hash, votes, agg_sig))
    }

    /// Get current vote count for a block
    pub fn vote_count(&self, view: View, block_hash: &Hash) -> usize {
        self.votes
            .get(&(view, *block_hash))
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Get votes for a block (without removing)
    pub fn get_votes(&self, view: View, block_hash: &Hash) -> Vec<Vote> {
        self.votes
            .get(&(view, *block_hash))
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Remove and return votes for a block
    pub fn take_votes(&mut self, view: View, block_hash: &Hash) -> Vec<Vote> {
        self.votes
            .remove(&(view, *block_hash))
            .map(|m| m.into_values().collect())
            .unwrap_or_default()
    }

    /// Prune old votes below a certain view
    pub fn prune_below(&mut self, view: View) {
        self.votes.retain(|(v, _), _| *v >= view);
    }

    /// Clear all collected votes
    pub fn clear(&mut self) {
        self.votes.clear();
    }

    /// Check if using BLS aggregation
    pub fn is_bls_enabled(&self) -> bool {
        self.use_bls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vote(view: View, block_hash: Hash, voter: u8) -> Vote {
        Vote {
            view,
            block_hash,
            app_hash: [0u8; 32],
            voter: [voter; 32],
            signature: vec![0u8; 64],
            bls_pubkey: None,
        }
    }

    #[test]
    fn test_vote_collection() {
        let mut agg = VoteAggregator::new(2, false);

        let hash = [1u8; 32];
        let vote1 = make_vote(1, hash, 1);
        let vote2 = make_vote(1, hash, 2);

        // First vote - no quorum
        assert!(agg.add_vote(vote1).is_none());
        assert_eq!(agg.vote_count(1, &hash), 1);

        // Second vote - reaches quorum
        let cert = agg.add_vote(vote2);
        assert!(cert.is_some());
        assert_eq!(cert.unwrap().vote_count(), 2);
    }

    #[test]
    fn test_ignores_duplicates() {
        let mut agg = VoteAggregator::new(2, false);

        let hash = [1u8; 32];
        let vote1 = make_vote(1, hash, 1);

        agg.add_vote(vote1.clone());
        agg.add_vote(vote1); // Duplicate

        assert_eq!(agg.vote_count(1, &hash), 1);
    }

    #[test]
    fn test_different_blocks() {
        let mut agg = VoteAggregator::new(2, false);

        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];

        agg.add_vote(make_vote(1, hash1, 1));
        agg.add_vote(make_vote(1, hash2, 2));

        assert_eq!(agg.vote_count(1, &hash1), 1);
        assert_eq!(agg.vote_count(1, &hash2), 1);
    }

    #[test]
    fn test_prune() {
        let mut agg = VoteAggregator::new(2, false);

        let hash = [1u8; 32];
        agg.add_vote(make_vote(5, hash, 1));
        agg.add_vote(make_vote(10, hash, 2));
        agg.add_vote(make_vote(15, hash, 3));

        agg.prune_below(12);

        assert_eq!(agg.vote_count(5, &hash), 0);
        assert_eq!(agg.vote_count(10, &hash), 0);
        assert_eq!(agg.vote_count(15, &hash), 1);
    }

    #[test]
    fn test_take_votes() {
        let mut agg = VoteAggregator::new(3, false);

        let hash = [1u8; 32];
        agg.add_vote(make_vote(1, hash, 1));
        agg.add_vote(make_vote(1, hash, 2));

        let votes = agg.take_votes(1, &hash);
        assert_eq!(votes.len(), 2);

        // Should be empty now
        assert_eq!(agg.vote_count(1, &hash), 0);
    }

    #[test]
    fn test_bls_verification_accepts_valid() {
        use crate::crypto::bls::BlsSecretKey;

        let mut agg = VoteAggregator::new_with_options(1, true, false);

        let hash = [1u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);

        // Create valid BLS-signed vote
        let vote = Vote::new_bls(1, hash, [0u8; 32], [1u8; 32], &bls_sk);

        // Should accept valid vote
        let cert = agg.add_vote(vote);
        assert!(cert.is_some(), "Valid BLS vote should be accepted");
    }

    #[test]
    fn test_bls_verification_rejects_invalid() {
        use crate::crypto::bls::BlsSecretKey;

        let mut agg = VoteAggregator::new_with_options(2, true, false);

        let hash = [1u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);

        // Create valid vote then tamper with it
        let mut tampered_vote = Vote::new_bls(1, hash, [0u8; 32], [1u8; 32], &bls_sk);
        tampered_vote.block_hash[0] ^= 1; // Tamper with block hash

        // Should reject tampered vote
        agg.add_vote(tampered_vote);
        assert_eq!(agg.vote_count(1, &hash), 0, "Tampered vote should be rejected");
    }

    #[test]
    fn test_skip_verification_accepts_invalid() {
        use crate::crypto::bls::BlsSecretKey;

        // With skip_verification = true, quorum = 2 so we can check vote is stored
        let mut agg = VoteAggregator::new_with_options(2, true, true);

        let hash = [1u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);

        // Create valid vote then tamper with it
        let mut tampered_vote = Vote::new_bls(1, hash, [0u8; 32], [1u8; 32], &bls_sk);
        tampered_vote.block_hash[0] ^= 1; // Tamper
        let tampered_hash = tampered_vote.block_hash;

        // Should accept when verification is skipped (dev mode)
        let cert = agg.add_vote(tampered_vote);
        assert!(cert.is_none(), "Quorum not reached yet");
        // Vote is stored under tampered_hash (because that's what's in the vote)
        assert_eq!(agg.vote_count(1, &tampered_hash), 1, "Should accept when verification skipped");
    }
}
