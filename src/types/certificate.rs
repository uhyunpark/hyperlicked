//! Vote and Certificate Types
//!
//! Votes and Quorum Certificates for BFT consensus.

use serde::{Deserialize, Serialize};

use super::{Hash, NodeId, Signature, View};
use crate::crypto::bls::BlsSecretKey;

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
