//! Core types for the Hyperlicked consensus and application layers.
//!
//! All types here are designed for determinism:
//! - Integer math only (no floats)
//! - Explicit serialization
//! - Clear ownership semantics

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::crypto::bls::BlsSecretKey;

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
/// - `bls_pubkey`: Optional BLS public key (48 bytes) for BLS signature aggregation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    pub view: View,
    pub block_hash: Hash,
    pub app_hash: Hash, // For Byzantine detection: validators must agree on execution
    pub voter: NodeId,
    /// BLS signature (96 bytes) or legacy ECDSA placeholder (64 bytes)
    pub signature: Signature,
    /// BLS public key of voter (48 bytes), None for legacy ECDSA
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bls_pubkey: Option<Vec<u8>>,
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
            bls_pubkey: None,
        }
    }

    /// Create a BLS-signed vote
    pub fn new_bls(
        view: View,
        block_hash: Hash,
        app_hash: Hash,
        voter: NodeId,
        bls_sk: &BlsSecretKey,
    ) -> Self {
        let mut vote = Self {
            view,
            block_hash,
            app_hash,
            voter,
            signature: vec![],
            bls_pubkey: None,
        };

        // Sign the vote data
        let signing_data = vote.signing_data();
        let sig = bls_sk.sign(&signing_data);
        let pubkey = bls_sk.public_key();

        vote.signature = sig.to_bytes().to_vec();
        vote.bls_pubkey = Some(pubkey.to_bytes().to_vec());

        vote
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

    /// Check if this vote uses BLS signature
    pub fn is_bls(&self) -> bool {
        self.signature.len() == 96 && self.bls_pubkey.is_some()
    }

    /// Verify this vote's BLS signature
    pub fn verify_bls(&self) -> bool {
        if !self.is_bls() {
            return false;
        }

        use crate::crypto::bls::{BlsPublicKey, BlsSignature};

        // Parse pubkey
        let pubkey_bytes = match self.bls_pubkey.as_ref() {
            Some(pk) if pk.len() == 48 => pk,
            _ => return false,
        };
        let mut pk_arr = [0u8; 48];
        pk_arr.copy_from_slice(pubkey_bytes);
        let pubkey = match BlsPublicKey::from_bytes(&pk_arr) {
            Ok(pk) => pk,
            Err(_) => return false,
        };

        // Parse signature
        let sig = match BlsSignature::from_slice(&self.signature) {
            Ok(s) => s,
            Err(_) => return false,
        };

        // Verify
        pubkey.verify(&self.signing_data(), &sig)
    }
}

/// Quorum Certificate: proof that 2f+1 validators voted for a block.
///
/// A QC proves consensus was reached. In HotStuff-2:
/// - QC on block N allows proposing block N+1
/// - QC on block N+1 commits block N (2-chain rule)
///
/// Supports two modes:
/// - Legacy: stores individual votes, concatenates signatures
/// - BLS: stores voter list + pubkeys, single 96-byte aggregated signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
    pub view: View,
    pub block_hash: Hash,
    /// Individual votes (legacy mode, empty when using BLS aggregation)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub votes: Vec<Vote>,
    /// Voters who contributed to this QC (NodeIds)
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub voters: Vec<NodeId>,
    /// BLS public keys of voters (for verification), 48 bytes each
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub bls_pubkeys: Vec<Vec<u8>>,
    /// Aggregated signature (96 bytes for BLS, or concatenated for legacy)
    pub agg_signature: Vec<u8>,
}

impl Certificate {
    /// Create a certificate from collected votes (legacy mode)
    pub fn new(view: View, block_hash: Hash, votes: Vec<Vote>) -> Self {
        let voters = votes.iter().map(|v| v.voter).collect();
        let agg_signature = votes
            .iter()
            .flat_map(|v| v.signature.iter().copied())
            .collect();

        Self {
            view,
            block_hash,
            votes,
            voters,
            bls_pubkeys: vec![],
            agg_signature,
        }
    }

    /// Create a certificate with BLS aggregation
    pub fn new_bls(
        view: View,
        block_hash: Hash,
        votes: Vec<Vote>,
        agg_signature: Vec<u8>,
    ) -> Self {
        let voters = votes.iter().map(|v| v.voter).collect();
        let bls_pubkeys = votes
            .iter()
            .filter_map(|v| v.bls_pubkey.clone())
            .collect();

        Self {
            view,
            block_hash,
            votes: vec![], // Don't store individual votes when using BLS
            voters,
            bls_pubkeys,
            agg_signature,
        }
    }

    /// Number of voters in this certificate
    pub fn vote_count(&self) -> usize {
        if !self.voters.is_empty() {
            self.voters.len()
        } else {
            self.votes.len()
        }
    }

    /// Check if this is a BLS-aggregated certificate
    pub fn is_bls(&self) -> bool {
        !self.bls_pubkeys.is_empty() && self.agg_signature.len() == 96
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

/// ViewChange message: sent when a validator times out waiting for leader.
///
/// In HotStuff-2, when a leader fails:
/// 1. Validators timeout and broadcast ViewChange
/// 2. New leader collects 2f+1 ViewChanges to form ViewChangeCertificate
/// 3. New leader broadcasts NewView to start the new view
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewChange {
    /// The view this validator is moving FROM
    pub from_view: View,
    /// The view this validator wants to move TO
    pub to_view: View,
    /// The sender's highest QC (proof of chain progress)
    pub high_qc: Option<Certificate>,
    /// The sender's node ID
    pub sender: NodeId,
    /// Signature over (from_view, to_view, high_qc.block_hash)
    pub signature: Signature,
}

impl ViewChange {
    /// Data to be signed for this view change
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&self.from_view.to_le_bytes());
        data.extend_from_slice(&self.to_view.to_le_bytes());
        if let Some(ref qc) = self.high_qc {
            data.extend_from_slice(&qc.block_hash);
        }
        data
    }
}

/// ViewChangeCertificate: proof that 2f+1 validators agreed to change views.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewChangeCertificate {
    pub view: View,
    pub view_changes: Vec<ViewChange>,
    /// Aggregated signature (BLS when implemented)
    pub agg_signature: Vec<u8>,
}

impl ViewChangeCertificate {
    /// Create from collected ViewChange messages
    pub fn new(view: View, view_changes: Vec<ViewChange>) -> Self {
        let agg_signature = view_changes
            .iter()
            .flat_map(|vc| vc.signature.iter().copied())
            .collect();
        Self {
            view,
            view_changes,
            agg_signature,
        }
    }

    /// Get the highest QC among all ViewChange messages
    pub fn highest_qc(&self) -> Option<&Certificate> {
        self.view_changes
            .iter()
            .filter_map(|vc| vc.high_qc.as_ref())
            .max_by_key(|qc| qc.view)
    }
}

/// NewView message: sent by new leader after collecting ViewChange quorum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewView {
    pub view: View,
    /// The highest QC among all ViewChange messages
    pub high_qc: Option<Certificate>,
    /// Proof that 2f+1 validators agreed to this view change
    pub view_change_cert: ViewChangeCertificate,
}

/// All network message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    Propose(Propose),
    Vote(Vote),
    Prepare(Prepare),
    ViewChange(ViewChange),
    NewView(NewView),
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
        // Get BLS key from single_node config
        let cfg = ConsensusConfig::single_node();
        let bls_sk = cfg.bls_secret_key().expect("BLS should be enabled");

        // Create a BLS-signed vote
        let vote = Vote::new_bls(
            1,
            [1u8; 32],
            [2u8; 32],
            cfg.node_id,
            &bls_sk,
        );

        // Verify it's a BLS vote
        assert!(vote.is_bls(), "Vote should use BLS signature");
        assert_eq!(vote.signature.len(), 96, "BLS signature should be 96 bytes");
        assert!(vote.bls_pubkey.is_some(), "Vote should have BLS public key");
        assert_eq!(vote.bls_pubkey.as_ref().unwrap().len(), 48, "BLS pubkey should be 48 bytes");

        // Verify the signature
        assert!(vote.verify_bls(), "BLS signature should verify");

        // Tampered vote should fail verification
        let mut tampered_vote = vote.clone();
        tampered_vote.block_hash[0] ^= 1;
        assert!(!tampered_vote.verify_bls(), "Tampered vote should fail verification");
    }

    #[test]
    fn test_single_node_has_bls_keys() {
        let cfg = ConsensusConfig::single_node();
        assert!(cfg.bls_enabled(), "single_node config should have BLS enabled");
        assert!(cfg.bls_secret_key().is_some(), "should have BLS secret key");
        assert_eq!(cfg.bls_pubkeys.len(), 1, "should have one BLS pubkey");
    }
}
