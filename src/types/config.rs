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
