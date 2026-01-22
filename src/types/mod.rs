//! Core types for the Hyperlicked consensus and application layers.
//!
//! All types here are designed for determinism:
//! - Integer math only (no floats)
//! - Explicit serialization
//! - Clear ownership semantics

mod block;
mod certificate;
mod config;
mod messages;

pub use block::Block;
pub use certificate::{Certificate, Vote};
pub use config::ConsensusConfig;
pub use messages::{
    Message, NewView, Prepare, Propose, SnapshotRequest, SnapshotResponse, SyncRequest,
    SyncResponse, Timeout, TimeoutCertificate, ViewChange, ViewChangeCertificate,
};

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
            bls_pubkeys: vec![],
            bls_secret_key: None,
        };
        assert_eq!(cfg4.f(), 1);
        assert_eq!(cfg4.quorum(), 3);
    }

    #[test]
    fn test_vote_bls_signing() {
        use crate::crypto::bls::BlsSecretKey;

        // Get BLS key from single_node config
        let cfg = ConsensusConfig::single_node();
        let bls_sk = cfg.bls_secret_key().expect("BLS should be enabled");

        // Create a BLS-signed vote
        let vote = Vote::new_bls(1, [1u8; 32], [2u8; 32], cfg.node_id, &bls_sk);

        // Verify it's a BLS vote
        assert!(vote.is_bls(), "Vote should use BLS signature");
        assert_eq!(vote.signature.len(), 96, "BLS signature should be 96 bytes");
        assert!(vote.bls_pubkey.is_some(), "Vote should have BLS public key");
        assert_eq!(
            vote.bls_pubkey.as_ref().unwrap().len(),
            48,
            "BLS pubkey should be 48 bytes"
        );

        // Verify the signature
        assert!(vote.verify_bls(), "BLS signature should verify");

        // Tampered vote should fail verification
        let mut tampered_vote = vote.clone();
        tampered_vote.block_hash[0] ^= 1;
        assert!(
            !tampered_vote.verify_bls(),
            "Tampered vote should fail verification"
        );
    }

    #[test]
    fn test_single_node_has_bls_keys() {
        let cfg = ConsensusConfig::single_node();
        assert!(cfg.bls_enabled(), "single_node config should have BLS enabled");
        assert!(cfg.bls_secret_key().is_some(), "should have BLS secret key");
        assert_eq!(cfg.bls_pubkeys.len(), 1, "should have one BLS pubkey");
    }
}
