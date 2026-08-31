//! Vote and Certificate Types
//!
//! Votes and Quorum Certificates for BFT consensus.

use serde::{Deserialize, Serialize};

use super::{ConsensusContext, Hash, NodeId, Signature, View};
use crate::crypto::bls::BlsSecretKey;

/// A vote for a block.
///
/// Validators vote for blocks they consider valid. Votes include:
/// - `app_hash`: The state hash after executing the block (for Byzantine detection)
/// - `signature`: Proof that this validator approved the block
/// - `bls_pubkey`: Optional BLS public key (48 bytes) for BLS signature aggregation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vote {
    /// Consensus epoch that authenticated this vote.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    pub block_hash: Hash,
    pub app_hash: Hash, // For Byzantine detection: validators must agree on execution
    pub voter: NodeId,
    /// BLS signature (96 bytes) or legacy ECDSA placeholder (64 bytes)
    pub signature: Signature,
    /// BLS public key of voter (48 bytes), None for legacy ECDSA
    #[serde(default)]
    pub bls_pubkey: Option<Vec<u8>>,
}

impl Vote {
    /// Create an unsigned vote (signature will be added later)
    pub fn new(
        context: ConsensusContext,
        view: View,
        block_hash: Hash,
        app_hash: Hash,
        voter: NodeId,
    ) -> Self {
        Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            block_hash,
            app_hash,
            voter,
            signature: vec![0u8; 64], // Placeholder, will be signed
            bls_pubkey: None,
        }
    }

    /// Create a BLS-signed vote using the common signing format
    ///
    /// Signs: view || block_hash || app_hash (voter NOT included)
    /// This enables efficient BLS aggregate verification.
    pub fn new_bls(
        context: ConsensusContext,
        view: View,
        block_hash: Hash,
        app_hash: Hash,
        voter: NodeId,
        bls_sk: &BlsSecretKey,
    ) -> Self {
        let mut vote = Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            block_hash,
            app_hash,
            voter,
            signature: vec![],
            bls_pubkey: None,
        };

        // Sign the COMMON data (without voter) for aggregate verification
        let signing_data = vote.signing_data_common();
        let sig = bls_sk.sign(&signing_data);
        let pubkey = bls_sk.public_key();

        vote.signature = sig.to_bytes().to_vec();
        vote.bls_pubkey = Some(pubkey.to_bytes().to_vec());

        vote
    }

    /// Data to be signed (legacy format - includes voter ID)
    ///
    /// **DEPRECATED for BLS**: This format includes the voter ID, which makes
    /// each message unique and prevents efficient BLS aggregate verification.
    /// Use `signing_data_common()` for BLS signatures.
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"HYPERLICKED_VOTE_V3");
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.view.to_le_bytes());
        data.extend_from_slice(&self.block_hash);
        data.extend_from_slice(&self.app_hash);
        data.extend_from_slice(&self.voter);
        data
    }

    /// Common signing data for BLS aggregate verification
    ///
    /// All voters sign the same message: view || block_hash || app_hash
    /// This enables O(1) aggregate signature verification.
    pub fn signing_data_common(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(20 + 8 + 32 + 8 + 32 + 32);
        data.extend_from_slice(b"HYPERLICKED_VOTE_V3");
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.view.to_le_bytes());
        data.extend_from_slice(&self.block_hash);
        data.extend_from_slice(&self.app_hash);
        data
    }

    /// Return the authentication context carried by this vote.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this vote belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err("vote context does not match expected consensus context".to_string());
        }
        Ok(())
    }

    /// Check if this vote uses BLS signature
    pub fn is_bls(&self) -> bool {
        self.signature.len() == 96 && self.bls_pubkey.is_some()
    }

    /// Verify this vote's BLS signature using the common signing format
    ///
    /// Uses: view || block_hash || app_hash (voter NOT included)
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

        // Verify using common signing data (for aggregate compatibility)
        pubkey.verify(&self.signing_data_common(), &sig)
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
///
/// For BLS verification, all voters must agree on the same (view, block_hash, app_hash).
/// The app_hash is stored in the certificate to enable aggregate verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Certificate {
    /// Consensus epoch that authenticated this certificate.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    pub block_hash: Hash,
    /// App state hash that all voters agreed on (required for BLS verification)
    /// All voters in a valid QC must have the same app_hash.
    #[serde(default)]
    pub app_hash: Option<Hash>,
    /// Individual votes (legacy mode, empty when using BLS aggregation)
    #[serde(default)]
    pub votes: Vec<Vote>,
    /// Voters who contributed to this QC (NodeIds)
    #[serde(default)]
    pub voters: Vec<NodeId>,
    /// BLS public keys of voters (for verification), 48 bytes each
    #[serde(default)]
    pub bls_pubkeys: Vec<Vec<u8>>,
    /// Aggregated signature (96 bytes for BLS, or concatenated for legacy)
    pub agg_signature: Vec<u8>,
}

impl Certificate {
    /// Create a certificate from collected votes (legacy mode)
    pub fn new(
        context: ConsensusContext,
        view: View,
        block_hash: Hash,
        votes: Vec<Vote>,
    ) -> Result<Self, String> {
        let app_hash = Self::validate_votes(context, view, block_hash, &votes)?;
        let voters = votes.iter().map(|v| v.voter).collect();
        let agg_signature = votes
            .iter()
            .flat_map(|v| v.signature.iter().copied())
            .collect();

        Ok(Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            block_hash,
            app_hash: Some(app_hash),
            votes,
            voters,
            bls_pubkeys: vec![],
            agg_signature,
        })
    }

    /// Create a certificate with BLS aggregation
    ///
    /// All votes must have the same app_hash for the aggregate signature to verify.
    pub fn new_bls(
        context: ConsensusContext,
        view: View,
        block_hash: Hash,
        votes: Vec<Vote>,
        agg_signature: Vec<u8>,
    ) -> Result<Self, String> {
        let app_hash = Self::validate_votes(context, view, block_hash, &votes)?;
        if votes.iter().any(|vote| !vote.is_bls()) {
            return Err("BLS certificate contains a non-BLS vote".to_string());
        }
        let voters = votes.iter().map(|v| v.voter).collect();
        let bls_pubkeys = votes
            .iter()
            .map(|v| {
                v.bls_pubkey
                    .clone()
                    .ok_or_else(|| "BLS vote is missing its public key".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            block_hash,
            app_hash: Some(app_hash),
            votes: vec![], // Don't store individual votes when using BLS
            voters,
            bls_pubkeys,
            agg_signature,
        })
    }

    fn validate_votes(
        context: ConsensusContext,
        view: View,
        block_hash: Hash,
        votes: &[Vote],
    ) -> Result<Hash, String> {
        let first = votes
            .first()
            .ok_or_else(|| "cannot form a certificate without votes".to_string())?;
        if first.context() != context {
            return Err("certificate context does not match its votes".to_string());
        }
        if first.view != view || first.block_hash != block_hash {
            return Err("certificate subject does not match its first vote".to_string());
        }

        let app_hash = first.app_hash;
        let mut voters = std::collections::HashSet::new();
        for vote in votes {
            if vote.context() != context {
                return Err("certificate contains votes from different contexts".to_string());
            }
            if vote.view != view || vote.block_hash != block_hash {
                return Err("certificate contains a vote for a different subject".to_string());
            }
            if vote.app_hash != app_hash {
                return Err("certificate contains votes with different app hashes".to_string());
            }
            if !voters.insert(vote.voter) {
                return Err("certificate contains duplicate voters".to_string());
            }
        }
        Ok(app_hash)
    }

    /// Number of voters in this certificate
    pub fn vote_count(&self) -> usize {
        if !self.voters.is_empty() {
            self.voters.len()
        } else {
            self.votes.len()
        }
    }

    /// Return the authentication context carried by this certificate.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this certificate belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err(
                "certificate context does not match expected consensus context".to_string(),
            );
        }
        Ok(())
    }

    /// Check if this is a BLS-aggregated certificate
    pub fn is_bls(&self) -> bool {
        !self.bls_pubkeys.is_empty() && self.agg_signature.len() == 96
    }

    /// Verify BLS aggregate signature
    ///
    /// Uses the common signing message (view, block_hash, app_hash) that all
    /// voters sign. This enables efficient O(1) aggregate verification.
    ///
    /// Returns Ok(()) if verification passes, Err with reason otherwise.
    pub fn verify_bls(&self) -> Result<(), String> {
        use crate::crypto::bls::{verify_aggregate_same_message, BlsPublicKey, BlsSignature};

        if !self.is_bls() {
            return Err("Not a BLS certificate".to_string());
        }

        // Must have app_hash for verification
        let app_hash = self
            .app_hash
            .ok_or("Certificate missing app_hash for BLS verification")?;

        // Must have voters and matching pubkeys
        if self.voters.is_empty() {
            return Err("Certificate has no voters".to_string());
        }
        if self.voters.len() != self.bls_pubkeys.len() {
            return Err(format!(
                "Voter/pubkey count mismatch: {} vs {}",
                self.voters.len(),
                self.bls_pubkeys.len()
            ));
        }

        // Parse aggregate signature
        let agg_sig = BlsSignature::from_slice(&self.agg_signature)
            .map_err(|_| "Invalid aggregate signature")?;

        // Parse public keys
        let mut pubkeys = Vec::with_capacity(self.bls_pubkeys.len());
        for (i, pk_bytes) in self.bls_pubkeys.iter().enumerate() {
            if pk_bytes.len() != 48 {
                return Err(format!("Invalid pubkey length at index {}", i));
            }
            let mut pk_arr = [0u8; 48];
            pk_arr.copy_from_slice(pk_bytes);
            let pk = BlsPublicKey::from_bytes(&pk_arr)
                .map_err(|_| format!("Failed to parse pubkey at index {}", i))?;
            pubkeys.push(pk);
        }

        // Build the common signing message (what all voters signed)
        let signing_message =
            Self::build_signing_message(self.context(), self.view, &self.block_hash, &app_hash);

        // Verify aggregate signature against aggregate public key
        if verify_aggregate_same_message(&signing_message, &agg_sig, &pubkeys) {
            Ok(())
        } else {
            Err("BLS aggregate signature verification failed".to_string())
        }
    }

    /// Build the common signing message for BLS votes
    ///
    /// All voters sign: domain || epoch || committee_hash || genesis_hash || view || block_hash || app_hash
    /// The voter's identity is NOT included - it's established by which pubkey verifies.
    pub fn build_signing_message(
        context: ConsensusContext,
        view: View,
        block_hash: &Hash,
        app_hash: &Hash,
    ) -> Vec<u8> {
        let mut data = Vec::with_capacity(20 + 8 + 32 + 8 + 32 + 32);
        data.extend_from_slice(b"HYPERLICKED_VOTE_V3");
        data.extend_from_slice(&context.epoch.to_le_bytes());
        data.extend_from_slice(&context.committee_hash);
        data.extend_from_slice(&context.genesis_hash);
        data.extend_from_slice(&view.to_le_bytes());
        data.extend_from_slice(block_hash);
        data.extend_from_slice(app_hash);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ConsensusContext {
        ConsensusContext::new(0, [7u8; 32])
    }

    #[test]
    fn vote_signature_binds_context() {
        let secret = BlsSecretKey::from_seed(&[9u8; 32]);
        let vote = Vote::new_bls(context(), 3, [1u8; 32], [2u8; 32], [3u8; 32], &secret);
        assert!(vote.verify_bls());

        let mut different_epoch = vote.clone();
        different_epoch.epoch = 1;
        assert!(!different_epoch.verify_bls());

        let mut different_committee = vote;
        different_committee.committee_hash[0] ^= 1;
        assert!(!different_committee.verify_bls());

        let different_genesis = Vote::new_bls(
            ConsensusContext::with_genesis(0, context().committee_hash, [8u8; 32]),
            3,
            [1u8; 32],
            [2u8; 32],
            [3u8; 32],
            &secret,
        );
        assert_ne!(
            different_genesis.signing_data_common(),
            Vote::new_bls(context(), 3, [1u8; 32], [2u8; 32], [3u8; 32], &secret)
                .signing_data_common()
        );
    }

    #[test]
    fn certificate_rejects_mixed_context_votes() {
        let secret_a = BlsSecretKey::from_seed(&[10u8; 32]);
        let secret_b = BlsSecretKey::from_seed(&[11u8; 32]);
        let vote_a = Vote::new_bls(context(), 3, [1u8; 32], [2u8; 32], [3u8; 32], &secret_a);
        let vote_b = Vote::new_bls(
            ConsensusContext::new(1, context().committee_hash),
            3,
            [1u8; 32],
            [2u8; 32],
            [4u8; 32],
            &secret_b,
        );

        assert!(Certificate::new(context(), 3, [1u8; 32], vec![vote_a, vote_b]).is_err());
    }

    #[test]
    fn certificate_rejects_votes_from_another_genesis_domain() {
        let secret_a = BlsSecretKey::from_seed(&[14u8; 32]);
        let secret_b = BlsSecretKey::from_seed(&[15u8; 32]);
        let context_a = ConsensusContext::with_genesis(0, [7u8; 32], [1u8; 32]);
        let context_b = ConsensusContext::with_genesis(0, [7u8; 32], [2u8; 32]);
        let vote_a = Vote::new_bls(context_a, 3, [1u8; 32], [2u8; 32], [3u8; 32], &secret_a);
        let vote_b = Vote::new_bls(context_b, 3, [1u8; 32], [2u8; 32], [4u8; 32], &secret_b);

        assert!(Certificate::new(context_a, 3, [1u8; 32], vec![vote_a, vote_b]).is_err());
    }
}
