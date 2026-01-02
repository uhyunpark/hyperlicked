//! Core types for the Hyperlicked consensus and application layers.
//!
//! All types here are designed for determinism:
//! - Integer math only (no floats)
//! - Explicit serialization
//! - Clear ownership semantics

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// =============================================================================
// Type Aliases
// =============================================================================

/// View number (round in consensus)
pub type View = u64;

/// Block height (number of committed blocks, starting from 0 for genesis)
pub type Height = u64;

/// 32-byte hash (SHA-256)
pub type Hash = [u8; 32];

/// Validator identity (32 bytes, could be public key hash)
pub type NodeId = [u8; 32];

/// Cryptographic signature (variable length, typically 64-65 bytes)
/// Using Vec<u8> for serde compatibility with arrays > 32 bytes
pub type Signature = Vec<u8>;

/// Price in cents (1 USD = 100). Integer math for determinism.
pub type Price = i64;

/// Size in satoshis (1 unit = 100_000_000). Integer math for determinism.
pub type Size = i64;

// =============================================================================
// Consensus Types
// =============================================================================

/// A block in the chain.
///
/// Blocks form a chain via `parent` hash. Each block has:
/// - `view`: The consensus round it was proposed in
/// - `height`: Position in committed chain (0 = genesis)
/// - `payload`: Serialized transactions
/// - `app_hash`: State root after executing this block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub view: View,
    pub height: Height,
    pub parent: Hash,
    pub payload: Vec<u8>,
    pub proposer: NodeId,
    pub app_hash: Hash,
    pub timestamp: u64,
}

impl Block {
    /// Compute the hash of this block
    pub fn hash(&self) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.parent);
        hasher.update(&self.payload);
        hasher.update(self.proposer);
        hasher.update(self.app_hash);
        hasher.update(self.timestamp.to_le_bytes());
        hasher.finalize().into()
    }

    /// Create genesis block (height 0, no parent)
    pub fn genesis() -> Self {
        Self {
            view: 0,
            height: 0,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
        }
    }
}

/// A vote for a block.
///
/// Validators vote for blocks they consider valid. Votes include:
/// - `app_hash`: The state hash after executing the block (for Byzantine detection)
/// - `signature`: Proof that this validator approved the block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub view: View,
    pub block_hash: Hash,
    pub app_hash: Hash, // For Byzantine detection: validators must agree on execution
    pub voter: NodeId,
    pub signature: Signature,
}

impl Vote {
    /// Create an unsigned vote (signature will be added later)
    pub fn new(view: View, block_hash: Hash, app_hash: Hash, voter: NodeId) -> Self {
        Self {
            view,
            block_hash,
            app_hash,
            voter,
            signature: vec![0u8; 64], // Placeholder, will be signed
        }
    }

    /// Data to be signed
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.view.to_le_bytes());
        data.extend_from_slice(&self.block_hash);
        data.extend_from_slice(&self.app_hash);
        data.extend_from_slice(&self.voter);
        data
    }
}

/// Quorum Certificate: proof that 2f+1 validators voted for a block.
///
/// A QC proves consensus was reached. In HotStuff-2:
/// - QC on block N allows proposing block N+1
/// - QC on block N+1 commits block N (2-chain rule)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub view: View,
    pub block_hash: Hash,
    pub votes: Vec<Vote>,
    /// Aggregated signature (BLS). For now, just concatenate signatures.
    pub agg_signature: Vec<u8>,
}

impl Certificate {
    /// Create a certificate from collected votes
    pub fn new(view: View, block_hash: Hash, votes: Vec<Vote>) -> Self {
        // For now, aggregate signature is just a placeholder
        // Real implementation would use BLS aggregation
        let agg_signature = votes
            .iter()
            .flat_map(|v| v.signature.iter().copied())
            .collect();

        Self {
            view,
            block_hash,
            votes,
            agg_signature,
        }
    }

    /// Number of votes in this certificate
    pub fn vote_count(&self) -> usize {
        self.votes.len()
    }
}

// =============================================================================
// Network Messages
// =============================================================================

/// Propose message: leader broadcasts a new block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Propose {
    pub block: Block,
    /// QC that justifies this proposal (proves parent is certified)
    pub justify: Option<Certificate>,
}

/// Prepare message: leader broadcasts QC after collecting votes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepare {
    pub view: View,
    pub qc: Certificate,
}

/// All network message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    Propose(Propose),
    Vote(Vote),
    Prepare(Prepare),
}

// =============================================================================
// Configuration
// =============================================================================

/// Consensus configuration
#[derive(Debug, Clone)]
pub struct ConsensusConfig {
    /// This node's ID
    pub node_id: NodeId,
    /// All validator node IDs (including self)
    pub validators: Vec<NodeId>,
    /// Timeout before view change (milliseconds)
    pub view_timeout_ms: u64,
}

impl ConsensusConfig {
    /// Number of validators
    pub fn n(&self) -> usize {
        self.validators.len()
    }

    /// Maximum Byzantine faults tolerated: f = (n-1)/3
    pub fn f(&self) -> usize {
        (self.n() - 1) / 3
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

    /// Check if we are the leader for a given view
    pub fn is_leader(&self, view: View) -> bool {
        self.leader_of(view) == self.node_id
    }

    /// Get leader for a given view (round-robin)
    pub fn leader_of(&self, view: View) -> NodeId {
        let idx = (view as usize) % self.validators.len();
        self.validators[idx]
    }

    /// Create config for single-node testing
    pub fn single_node() -> Self {
        let node_id = [1u8; 32];
        Self {
            node_id,
            validators: vec![node_id],
            view_timeout_ms: 3000,
        }
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Compute SHA-256 hash of arbitrary data
pub fn hash(data: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// Format hash as hex string (for logging)
pub fn hash_to_hex(h: &Hash) -> String {
    hex::encode(h)
}

/// Short hash for display (first 8 chars)
pub fn hash_short(h: &Hash) -> String {
    hex::encode(&h[..4])
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = Block::genesis();
        assert_eq!(genesis.height, 0);
        assert_eq!(genesis.view, 0);
        assert_eq!(genesis.parent, [0u8; 32]);
    }

    #[test]
    fn test_block_hash_deterministic() {
        let block = Block::genesis();
        let hash1 = block.hash();
        let hash2 = block.hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_config_quorum() {
        // n=1: f=0, quorum=1
        let cfg = ConsensusConfig::single_node();
        assert_eq!(cfg.f(), 0);
        assert_eq!(cfg.quorum(), 1);

        // n=4: f=1, quorum=3
        let cfg4 = ConsensusConfig {
            node_id: [1u8; 32],
            validators: vec![[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]],
            view_timeout_ms: 3000,
        };
        assert_eq!(cfg4.f(), 1);
        assert_eq!(cfg4.quorum(), 3);
    }
}
