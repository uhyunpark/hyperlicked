//! Consensus Configuration
//!
//! Configuration for the HotStuff-2 consensus protocol.

use super::{NodeId, View};
use crate::crypto::bls::BlsSecretKey;

/// Consensus configuration
#[derive(Debug, Clone)]
pub struct ConsensusConfig {
    /// This node's ID
    pub node_id: NodeId,
    /// All validator node IDs (including self)
    pub validators: Vec<NodeId>,
    /// Timeout before view change (milliseconds)
    pub view_timeout_ms: u64,
    /// BLS public keys for each validator (same order as validators), 48 bytes each
    pub bls_pubkeys: Vec<Vec<u8>>,
    /// Our BLS secret key (32 bytes seed), None if BLS disabled
    pub bls_secret_key: Option<[u8; 32]>,
}

impl ConsensusConfig {
    /// Number of validators
    pub fn n(&self) -> usize {
        self.validators.len()
    }

    /// Number of validators with optional dynamic override
    pub fn n_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        dynamic.map(|d| d.len()).unwrap_or_else(|| self.validators.len())
    }

    /// Maximum Byzantine faults tolerated: f = (n-1)/3
    pub fn f(&self) -> usize {
        (self.n() - 1) / 3
    }

    /// Maximum Byzantine faults with optional dynamic override
    pub fn f_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        (self.n_with(dynamic) - 1) / 3
    }

    /// Quorum size: need majority for safety
    /// For BFT with n=3f+1: quorum = 2f+1
    /// But for n=3, we use simple majority (2) for testing
    pub fn quorum(&self) -> usize {
        let bft_quorum = 2 * self.f() + 1;
        let majority = self.n() / 2 + 1;
        // Use the larger of BFT quorum or simple majority
        bft_quorum.max(majority)
    }

    /// Quorum size with optional dynamic override
    pub fn quorum_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        let n = self.n_with(dynamic);
        let f = self.f_with(dynamic);
        let bft_quorum = 2 * f + 1;
        let majority = n / 2 + 1;
        bft_quorum.max(majority)
    }

    /// Check if we are the leader for a given view
    pub fn is_leader(&self, view: View) -> bool {
        self.leader_of(view) == self.node_id
    }

    /// Check if we are the leader with dynamic validator set
    pub fn is_leader_with(&self, view: View, dynamic: Option<&[NodeId]>) -> bool {
        self.leader_of_with(view, dynamic) == self.node_id
    }

    /// Get leader for a given view (round-robin)
    pub fn leader_of(&self, view: View) -> NodeId {
        let idx = (view as usize) % self.validators.len();
        self.validators[idx]
    }

    /// Get leader with dynamic validator set
    pub fn leader_of_with(&self, view: View, dynamic: Option<&[NodeId]>) -> NodeId {
        let validators = dynamic.unwrap_or(&self.validators);
        if validators.is_empty() {
            return self.node_id; // Single-node fallback
        }
        let idx = (view as usize) % validators.len();
        validators[idx]
    }

    /// Get leader using weighted selection based on stake
    ///
    /// Leaders are selected proportionally to stake. A validator with 2x stake
    /// is 2x more likely to be leader than one with 1x stake.
    ///
    /// # Algorithm
    ///
    /// Given stakes [100, 200, 300] (total 600):
    /// - view % 600 in [0, 100) -> validator 0
    /// - view % 600 in [100, 300) -> validator 1
    /// - view % 600 in [300, 600) -> validator 2
    pub fn leader_of_weighted(&self, view: View, stakes: &[(NodeId, u64)]) -> NodeId {
        if stakes.is_empty() {
            return self.node_id; // Single-node fallback
        }

        let total_stake: u64 = stakes.iter().map(|(_, s)| s).sum();
        if total_stake == 0 {
            // All stakes are zero, fall back to round-robin
            let idx = (view as usize) % stakes.len();
            return stakes[idx].0;
        }

        // Use view to select a slot in [0, total_stake)
        let slot = (view % total_stake) as u64;

        // Find the validator whose cumulative stake range contains this slot
        let mut cumulative: u64 = 0;
        for (node_id, stake) in stakes {
            cumulative += stake;
            if slot < cumulative {
                return *node_id;
            }
        }

        // Shouldn't reach here, but fallback to last validator
        stakes.last().map(|(id, _)| *id).unwrap_or(self.node_id)
    }

    /// Check if we are the leader using weighted selection
    pub fn is_leader_weighted(&self, view: View, stakes: &[(NodeId, u64)]) -> bool {
        self.leader_of_weighted(view, stakes) == self.node_id
    }

    /// Get active validators, preferring dynamic set if available
    pub fn active_validators<'a>(&'a self, dynamic: Option<&'a [NodeId]>) -> &'a [NodeId] {
        dynamic.unwrap_or(&self.validators)
    }

    /// Update the static validator list (used during epoch transitions)
    pub fn update_validators(&mut self, validators: Vec<NodeId>, bls_pubkeys: Vec<Vec<u8>>) {
        self.validators = validators;
        self.bls_pubkeys = bls_pubkeys;
    }

    /// Check if BLS is enabled
    pub fn bls_enabled(&self) -> bool {
        !self.bls_pubkeys.is_empty() && self.bls_secret_key.is_some()
    }

    /// Get BLS public key for a validator
    pub fn bls_pubkey(&self, node_id: &NodeId) -> Option<&[u8]> {
        self.validators
            .iter()
            .position(|v| v == node_id)
            .and_then(|i| self.bls_pubkeys.get(i))
            .map(|v| v.as_slice())
    }

    /// Create config for single-node testing with BLS enabled
    pub fn single_node() -> Self {
        let node_id = [1u8; 32];
        // Use deterministic seed for testing
        let bls_seed = [42u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&bls_seed);
        let bls_pk = bls_sk.public_key().to_bytes().to_vec();

        Self {
            node_id,
            validators: vec![node_id],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![bls_pk],
            bls_secret_key: Some(bls_seed),
        }
    }

    /// Get our BLS secret key
    pub fn bls_secret_key(&self) -> Option<BlsSecretKey> {
        self.bls_secret_key.map(|seed| BlsSecretKey::from_seed(&seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ConsensusConfig {
        ConsensusConfig {
            node_id: [1u8; 32],
            validators: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        }
    }

    #[test]
    fn test_round_robin_leader() {
        let config = test_config();

        // Round-robin should cycle through validators
        assert_eq!(config.leader_of(0), [1u8; 32]);
        assert_eq!(config.leader_of(1), [2u8; 32]);
        assert_eq!(config.leader_of(2), [3u8; 32]);
        assert_eq!(config.leader_of(3), [1u8; 32]); // Wraps around
    }

    #[test]
    fn test_weighted_leader_equal_stakes() {
        let config = test_config();

        // Equal stakes should behave like round-robin
        let stakes = vec![
            ([1u8; 32], 100),
            ([2u8; 32], 100),
            ([3u8; 32], 100),
        ];

        // Total 300, so slots 0-99 -> v1, 100-199 -> v2, 200-299 -> v3
        assert_eq!(config.leader_of_weighted(0, &stakes), [1u8; 32]);
        assert_eq!(config.leader_of_weighted(100, &stakes), [2u8; 32]);
        assert_eq!(config.leader_of_weighted(200, &stakes), [3u8; 32]);
        assert_eq!(config.leader_of_weighted(300, &stakes), [1u8; 32]); // Wraps
    }

    #[test]
    fn test_weighted_leader_unequal_stakes() {
        let config = test_config();

        // Validator 2 has 2x the stake of others
        let stakes = vec![
            ([1u8; 32], 100),
            ([2u8; 32], 200),
            ([3u8; 32], 100),
        ];

        // Total 400: 0-99 -> v1, 100-299 -> v2, 300-399 -> v3
        assert_eq!(config.leader_of_weighted(0, &stakes), [1u8; 32]);
        assert_eq!(config.leader_of_weighted(50, &stakes), [1u8; 32]);
        assert_eq!(config.leader_of_weighted(100, &stakes), [2u8; 32]);
        assert_eq!(config.leader_of_weighted(200, &stakes), [2u8; 32]);
        assert_eq!(config.leader_of_weighted(300, &stakes), [3u8; 32]);
        assert_eq!(config.leader_of_weighted(400, &stakes), [1u8; 32]); // Wraps
    }

    #[test]
    fn test_weighted_leader_distribution() {
        let config = test_config();

        // Validator 1 has 10%, validator 2 has 30%, validator 3 has 60%
        let stakes = vec![
            ([1u8; 32], 100),
            ([2u8; 32], 300),
            ([3u8; 32], 600),
        ];

        // Count how many times each validator is leader over 1000 views
        let mut counts = [0u32; 3];
        for view in 0..1000 {
            let leader = config.leader_of_weighted(view, &stakes);
            if leader == [1u8; 32] {
                counts[0] += 1;
            } else if leader == [2u8; 32] {
                counts[1] += 1;
            } else {
                counts[2] += 1;
            }
        }

        // Should be proportional to stake (10%, 30%, 60%)
        // With 1000 views, expect roughly 100, 300, 600
        assert_eq!(counts[0], 100);
        assert_eq!(counts[1], 300);
        assert_eq!(counts[2], 600);
    }

    #[test]
    fn test_weighted_leader_empty_stakes() {
        let config = test_config();

        // Empty stakes should return self
        let stakes: Vec<(NodeId, u64)> = vec![];
        assert_eq!(config.leader_of_weighted(0, &stakes), config.node_id);
    }

    #[test]
    fn test_weighted_leader_zero_stakes() {
        let config = test_config();

        // All zero stakes should fall back to round-robin
        let stakes = vec![
            ([1u8; 32], 0),
            ([2u8; 32], 0),
            ([3u8; 32], 0),
        ];

        // Falls back to round-robin
        assert_eq!(config.leader_of_weighted(0, &stakes), [1u8; 32]);
        assert_eq!(config.leader_of_weighted(1, &stakes), [2u8; 32]);
        assert_eq!(config.leader_of_weighted(2, &stakes), [3u8; 32]);
    }
}
