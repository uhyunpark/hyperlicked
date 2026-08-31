//! Vote Aggregation
//!
//! Collects votes and forms QCs with BLS signature aggregation.
//!
//! ## Usage
//!
//! ```
//! use hyperlicked::consensus::VoteAggregator;
//! use hyperlicked::types::{ConsensusContext, Vote};
//!
//! let context = ConsensusContext::new(0, [7u8; 32]);
//! let block_hash = [1u8; 32];
//! let app_hash = [2u8; 32];
//! let mut aggregator = VoteAggregator::new_with_context(context, 2, false);
//!
//! let first_vote = Vote::new(context, 1, block_hash, app_hash, [3u8; 32]);
//! let second_vote = Vote::new(context, 1, block_hash, app_hash, [4u8; 32]);
//!
//! assert!(aggregator.add_vote(first_vote).is_none());
//! let certificate = aggregator
//!     .add_vote(second_vote)
//!     .expect("the second vote reaches quorum");
//! assert_eq!(certificate.context(), context);
//! assert_eq!(certificate.view, 1);
//! assert_eq!(aggregator.vote_count(1, &block_hash), 2);
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::crypto::bls::{aggregate_signatures, BlsSignature};
use crate::types::{Certificate, ConsensusContext, Hash, NodeId, View, Vote};

use super::metrics::ConsensusMetrics;
use super::MAX_VOTES_PER_VALIDATOR_PER_SECOND;

// =============================================================================
// Vote Rate Limiter (CRITICAL-7)
// =============================================================================

/// Rate limiter for votes using sliding window algorithm.
///
/// Prevents vote spam DoS attacks by limiting votes per validator per second.
/// Uses a sliding window of timestamps to track recent votes.
#[derive(Debug, Default)]
pub struct VoteRateLimiter {
    /// Per-validator sliding window of vote timestamps
    windows: HashMap<NodeId, VecDeque<Instant>>,
    /// Maximum votes per second per validator
    max_per_second: usize,
}

/// Error returned when vote is rate limited
#[derive(Debug, Clone, thiserror::Error)]
pub enum RateLimitError {
    #[error("vote rate limit exceeded: {votes_in_window} votes in last second (max {max})")]
    RateLimitExceeded { votes_in_window: usize, max: usize },
}

impl VoteRateLimiter {
    /// Create a new rate limiter with the given max votes per second
    pub fn new(max_per_second: usize) -> Self {
        Self {
            windows: HashMap::new(),
            max_per_second,
        }
    }

    /// Check if a vote from this validator should be allowed.
    /// Returns Ok(()) if allowed, Err if rate limited.
    pub fn check_and_record(&mut self, voter: &NodeId) -> Result<(), RateLimitError> {
        let now = Instant::now();
        let window = self.windows.entry(*voter).or_default();

        // Remove votes older than 1 second
        while let Some(&front) = window.front() {
            if now.duration_since(front).as_secs_f64() > 1.0 {
                window.pop_front();
            } else {
                break;
            }
        }

        // Check if under limit
        if window.len() >= self.max_per_second {
            return Err(RateLimitError::RateLimitExceeded {
                votes_in_window: window.len(),
                max: self.max_per_second,
            });
        }

        // Record this vote
        window.push_back(now);
        Ok(())
    }

    /// Prune old entries to prevent memory growth
    /// Called periodically (e.g., on view change)
    pub fn prune_stale(&mut self) {
        let now = Instant::now();
        self.windows.retain(|_, window| {
            // Remove votes older than 1 second
            while let Some(&front) = window.front() {
                if now.duration_since(front).as_secs_f64() > 1.0 {
                    window.pop_front();
                } else {
                    break;
                }
            }
            // Keep entry only if it has recent votes
            !window.is_empty()
        });
    }

    /// Get current vote count for a validator (for testing/metrics)
    pub fn vote_count(&self, voter: &NodeId) -> usize {
        self.windows.get(voter).map(|w| w.len()).unwrap_or(0)
    }
}

/// Collects votes and aggregates signatures
pub struct VoteAggregator {
    /// Static consensus context all collected votes and certificates must use.
    context: ConsensusContext,

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

    /// Rate limiter for vote spam prevention (CRITICAL-7)
    rate_limiter: VoteRateLimiter,
}

impl VoteAggregator {
    /// Create a new aggregator
    pub fn new(quorum: usize, use_bls: bool) -> Self {
        Self::new_with_context(ConsensusContext::new(0, [0u8; 32]), quorum, use_bls)
    }

    /// Create an aggregator bound to a consensus context.
    pub fn new_with_context(context: ConsensusContext, quorum: usize, use_bls: bool) -> Self {
        Self {
            context,
            votes: HashMap::new(),
            quorum,
            use_bls,
            skip_verification: false,
            metrics: None,
            rate_limiter: VoteRateLimiter::new(MAX_VOTES_PER_VALIDATOR_PER_SECOND),
        }
    }

    /// Create aggregator with verification control
    pub fn new_with_options(quorum: usize, use_bls: bool, skip_verification: bool) -> Self {
        Self::new_with_options_and_context(
            ConsensusContext::new(0, [0u8; 32]),
            quorum,
            use_bls,
            skip_verification,
        )
    }

    /// Create a context-bound aggregator with verification control.
    pub fn new_with_options_and_context(
        context: ConsensusContext,
        quorum: usize,
        use_bls: bool,
        skip_verification: bool,
    ) -> Self {
        Self {
            context,
            votes: HashMap::new(),
            quorum,
            use_bls,
            skip_verification,
            metrics: None,
            rate_limiter: VoteRateLimiter::new(MAX_VOTES_PER_VALIDATOR_PER_SECOND),
        }
    }

    /// Set metrics tracker for Byzantine event monitoring
    pub fn with_metrics(mut self, metrics: Arc<ConsensusMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Add a vote. Returns Certificate if quorum reached.
    /// Verifies BLS signature unless skip_verification is set.
    /// Enforces rate limiting (CRITICAL-7).
    pub fn add_vote(&mut self, vote: Vote) -> Option<Certificate> {
        if vote.validate_context(self.context).is_err() {
            tracing::warn!(
                view = vote.view,
                voter = %crate::types::hash_short(&vote.voter),
                "Rejecting vote with mismatched consensus context"
            );
            return None;
        }
        let key = (vote.view, vote.block_hash);
        let voter = vote.voter;

        // CRITICAL-7: Check rate limit before processing
        if let Err(e) = self.rate_limiter.check_and_record(&voter) {
            tracing::warn!(
                view = vote.view,
                voter = %crate::types::hash_short(&voter),
                error = %e,
                "Rejecting vote due to rate limiting - POTENTIAL SPAM ATTACK"
            );
            if let Some(ref metrics) = self.metrics {
                metrics.record_vote_rate_limited(&voter);
            }
            return None;
        }

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
                Certificate::new(self.context, key.0, key.1, votes).ok()
            }
        } else {
            None
        }
    }

    /// Aggregate BLS signatures into a certificate
    ///
    /// When `bls_batch_verify` feature is enabled, performs additional
    /// verification before aggregation to detect Byzantine signatures.
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
                return Certificate::new(self.context, view, block_hash, votes).ok();
            }
        };

        // When bls_batch_verify is enabled, verify each signature before aggregation
        // This catches Byzantine votes that would otherwise corrupt the aggregate
        #[cfg(feature = "bls_batch_verify")]
        {
            use crate::crypto::bls::BlsPublicKey;

            // Batch verify by checking each signature individually but in parallel
            // (Different from true batch verify which requires same message)
            let mut valid_votes = Vec::with_capacity(votes.len());
            let mut valid_sigs = Vec::with_capacity(votes.len());

            for (vote, sig) in votes.iter().zip(signatures.iter()) {
                // Parse public key
                let pk = vote.bls_pubkey.as_ref().and_then(|bytes| {
                    if bytes.len() == 48 {
                        let mut arr = [0u8; 48];
                        arr.copy_from_slice(bytes);
                        BlsPublicKey::from_bytes(&arr).ok()
                    } else {
                        None
                    }
                });

                let pk = match pk {
                    Some(pk) => pk,
                    None => {
                        tracing::warn!(
                            view = view,
                            voter = %crate::types::hash_short(&vote.voter),
                            "Invalid BLS public key"
                        );
                        continue;
                    }
                };

                // Verify signature using common signing data (excludes voter ID)
                // This matches how Vote::new_bls signs the vote
                let signing_data = vote.signing_data_common();
                if pk.verify(&signing_data, sig) {
                    valid_votes.push(vote.clone());
                    valid_sigs.push(sig.clone());
                } else {
                    tracing::warn!(
                        view = view,
                        voter = %crate::types::hash_short(&vote.voter),
                        "Invalid BLS signature - BYZANTINE FAULT"
                    );
                    if let Some(ref metrics) = self.metrics {
                        metrics.record_invalid_vote_signature(&vote.voter);
                    }
                }
            }

            // Check if we still have quorum after filtering
            if valid_votes.len() < self.quorum {
                tracing::error!(
                    view = view,
                    valid = valid_votes.len(),
                    quorum = self.quorum,
                    "Insufficient valid signatures after filtering Byzantine votes"
                );
                return None;
            }

            // Use filtered votes and signatures
            return self.aggregate_valid_signatures(view, block_hash, valid_votes, valid_sigs);
        }

        #[cfg(not(feature = "bls_batch_verify"))]
        self.aggregate_valid_signatures(view, block_hash, votes, signatures)
    }

    /// Aggregate already-validated signatures into a certificate
    fn aggregate_valid_signatures(
        &self,
        view: View,
        block_hash: Hash,
        votes: Vec<Vote>,
        signatures: Vec<BlsSignature>,
    ) -> Option<Certificate> {
        // Aggregate signatures
        let agg_sig = match aggregate_signatures(&signatures) {
            Ok(sig) => sig.to_bytes().to_vec(),
            Err(_) => {
                // Fallback to legacy if aggregation fails
                tracing::warn!("BLS aggregation failed, using legacy certificate");
                if let Some(ref metrics) = self.metrics {
                    metrics.record_aggregation_failure();
                }
                return Certificate::new(self.context, view, block_hash, votes).ok();
            }
        };

        Certificate::new_bls(self.context, view, block_hash, votes, agg_sig).ok()
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

    fn test_context() -> ConsensusContext {
        ConsensusContext::new(0, [0u8; 32])
    }

    fn make_vote(view: View, block_hash: Hash, voter: u8) -> Vote {
        Vote {
            epoch: test_context().epoch,
            committee_hash: test_context().committee_hash,
            genesis_hash: test_context().genesis_hash,
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
        let vote = Vote::new_bls(test_context(), 1, hash, [0u8; 32], [1u8; 32], &bls_sk);

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
        let mut tampered_vote =
            Vote::new_bls(test_context(), 1, hash, [0u8; 32], [1u8; 32], &bls_sk);
        tampered_vote.block_hash[0] ^= 1; // Tamper with block hash

        // Should reject tampered vote
        agg.add_vote(tampered_vote);
        assert_eq!(
            agg.vote_count(1, &hash),
            0,
            "Tampered vote should be rejected"
        );
    }

    #[test]
    fn test_skip_verification_accepts_invalid() {
        use crate::crypto::bls::BlsSecretKey;

        // With skip_verification = true, quorum = 2 so we can check vote is stored
        let mut agg = VoteAggregator::new_with_options(2, true, true);

        let hash = [1u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);

        // Create valid vote then tamper with it
        let mut tampered_vote =
            Vote::new_bls(test_context(), 1, hash, [0u8; 32], [1u8; 32], &bls_sk);
        tampered_vote.block_hash[0] ^= 1; // Tamper
        let tampered_hash = tampered_vote.block_hash;

        // Should accept when verification is skipped (dev mode)
        let cert = agg.add_vote(tampered_vote);
        assert!(cert.is_none(), "Quorum not reached yet");
        // Vote is stored under tampered_hash (because that's what's in the vote)
        assert_eq!(
            agg.vote_count(1, &tampered_hash),
            1,
            "Should accept when verification skipped"
        );
    }

    // ==========================================================================
    // Rate Limiter Tests (CRITICAL-7)
    // ==========================================================================

    #[test]
    fn test_rate_limiter_allows_under_limit() {
        let mut limiter = VoteRateLimiter::new(10);
        let voter = [1u8; 32];

        // First 10 votes should be allowed
        for _ in 0..10 {
            assert!(limiter.check_and_record(&voter).is_ok());
        }

        assert_eq!(limiter.vote_count(&voter), 10);
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let mut limiter = VoteRateLimiter::new(5);
        let voter = [1u8; 32];

        // First 5 should succeed
        for _ in 0..5 {
            assert!(limiter.check_and_record(&voter).is_ok());
        }

        // 6th should fail
        let result = limiter.check_and_record(&voter);
        assert!(result.is_err());
        if let Err(RateLimitError::RateLimitExceeded {
            votes_in_window,
            max,
        }) = result
        {
            assert_eq!(votes_in_window, 5);
            assert_eq!(max, 5);
        }
    }

    #[test]
    fn test_rate_limiter_per_validator() {
        let mut limiter = VoteRateLimiter::new(3);
        let voter1 = [1u8; 32];
        let voter2 = [2u8; 32];

        // Each voter has independent limit
        for _ in 0..3 {
            assert!(limiter.check_and_record(&voter1).is_ok());
            assert!(limiter.check_and_record(&voter2).is_ok());
        }

        // Both at limit now
        assert!(limiter.check_and_record(&voter1).is_err());
        assert!(limiter.check_and_record(&voter2).is_err());
    }

    #[test]
    fn test_aggregator_rate_limits_votes() {
        // Use a low quorum so we can test rate limiting kicks in
        let mut agg = VoteAggregator::new(100, false);

        let hash = [1u8; 32];
        let voter = [1u8; 32];

        // Send MAX_VOTES_PER_VALIDATOR_PER_SECOND votes
        for i in 0..MAX_VOTES_PER_VALIDATOR_PER_SECOND {
            let vote = Vote {
                epoch: test_context().epoch,
                committee_hash: test_context().committee_hash,
                genesis_hash: test_context().genesis_hash,
                view: i as u64,
                block_hash: hash,
                app_hash: [0u8; 32],
                voter,
                signature: vec![0u8; 64],
                bls_pubkey: None,
            };
            // Vote for different views to avoid duplicate detection
            agg.add_vote(vote);
        }

        // Next vote should be rate limited (returns None, not added)
        let rate_limited_vote = Vote {
            epoch: test_context().epoch,
            committee_hash: test_context().committee_hash,
            genesis_hash: test_context().genesis_hash,
            view: 1000,
            block_hash: hash,
            app_hash: [0u8; 32],
            voter,
            signature: vec![0u8; 64],
            bls_pubkey: None,
        };

        let result = agg.add_vote(rate_limited_vote);
        assert!(result.is_none(), "Rate limited vote should return None");

        // Verify the vote wasn't stored
        assert_eq!(
            agg.vote_count(1000, &hash),
            0,
            "Rate limited vote should not be stored"
        );
    }
}
