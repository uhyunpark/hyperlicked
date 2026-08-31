//! Network Messages
//!
//! Protocol messages for HotStuff-2 consensus.

use serde::{Deserialize, Serialize};

use super::{Block, Certificate, Committee, ConsensusContext, Hash, NodeId, Signature, View};
use crate::app::SignedEnvelope;

/// Propose message: leader broadcasts a new block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Propose {
    /// Consensus epoch that authenticated this proposal.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub block: Block,
    /// QC that justifies this proposal (proves parent is certified)
    pub justify: Option<Certificate>,
    /// BLS signature by the scheduled proposer over the finalized block.
    ///
    /// The field is kept as bytes for wire compatibility with the other BLS
    /// messages.  A live network admission gate rejects an absent or invalid
    /// signature; an empty value is only useful to legacy in-process fixtures.
    #[serde(default)]
    pub proposer_signature: Signature,
}

impl Propose {
    /// Return the authentication context carried by this proposal.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this proposal belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err("proposal context does not match expected consensus context".to_string());
        }
        Ok(())
    }

    /// Fixed, versioned bytes authenticated by the proposer.
    ///
    /// The block hash is computed from the finalized block, including its
    /// application hash.  The explicit context fields prevent a signature
    /// from being replayed between chains or committees even if a block hash
    /// is otherwise reused.
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(32 + 8 + 32 + 32 + 32);
        data.extend_from_slice(b"HYPERLICKED_PROPOSER_V1");
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.block.hash());
        data
    }

    /// Verify the proposer signature against the configured committee key.
    pub fn verify_signature(&self, committee: &Committee) -> Result<(), String> {
        use crate::crypto::bls::{BlsPublicKey, BlsSignature};

        let key_bytes = committee
            .bls_pubkey(&self.block.proposer)
            .ok_or_else(|| "proposer has no configured BLS public key".to_string())?;
        if self.proposer_signature.len() != 96 {
            return Err("proposal is missing a 96-byte proposer signature".to_string());
        }
        if key_bytes.len() != 48 {
            return Err("configured proposer BLS public key has invalid length".to_string());
        }
        let mut key_array = [0u8; 48];
        key_array.copy_from_slice(key_bytes);
        let public_key = BlsPublicKey::from_bytes(&key_array)
            .map_err(|_| "configured proposer BLS public key is invalid".to_string())?;
        let signature = BlsSignature::from_slice(&self.proposer_signature)
            .map_err(|_| "proposal proposer signature is invalid".to_string())?;
        if !public_key.verify(&self.signing_data(), &signature) {
            return Err("proposal proposer signature verification failed".to_string());
        }
        Ok(())
    }
}

/// Prepare message: leader broadcasts QC after collecting votes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Prepare {
    /// Consensus epoch that authenticated this prepare.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    pub qc: Certificate,
}

impl Prepare {
    /// Return the authentication context carried by this prepare.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this prepare belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err("prepare context does not match expected consensus context".to_string());
        }
        Ok(())
    }
}

/// Timeout message: sent when a validator times out waiting for leader.
///
/// This is a simplified message specifically for timeout aggregation.
/// Contains only the essential data needed for BLS signature aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeout {
    /// Consensus epoch that authenticated this timeout.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    /// The view that timed out
    pub view: View,
    /// The sender's highest QC view (for leader election tie-breaking)
    pub high_qc_view: View,
    /// The sender's node ID
    pub sender: NodeId,
    /// BLS signature over (view, high_qc_view)
    pub signature: Signature,
}

impl Timeout {
    /// Data to be signed for this timeout
    ///
    /// Only the view is signed - high_qc_view is self-reported for tie-breaking
    /// but not cryptographically bound (validators could lie about it regardless).
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"HYPERLICKED_TIMEOUT_V3"); // Domain/version separator
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.view.to_le_bytes());
        data
    }

    /// Return the authentication context carried by this timeout.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this timeout belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err("timeout context does not match expected consensus context".to_string());
        }
        Ok(())
    }
}

/// TimeoutCertificate: cryptographic proof that 2f+1 validators timed out.
///
/// This provides Byzantine safety for view changes - proves that enough
/// honest validators agreed the leader failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutCertificate {
    /// Consensus epoch that authenticated this timeout certificate.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    /// The view that timed out
    pub view: View,
    /// The highest high_qc_view among all timeouts (for leader election)
    pub high_qc_view: View,
    /// Node IDs that contributed to this TC
    pub signers: Vec<NodeId>,
    /// Aggregated BLS signature over the view number
    pub agg_signature: Signature,
}

impl TimeoutCertificate {
    /// Get the signing data that was aggregated
    ///
    /// Only the view is signed (same as individual Timeout messages).
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"HYPERLICKED_TIMEOUT_V3");
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.view.to_le_bytes());
        data
    }

    /// Return the authentication context carried by this timeout certificate.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this timeout certificate belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err(
                "timeout certificate context does not match expected consensus context".to_string(),
            );
        }
        Ok(())
    }
}

/// ViewChange message: sent when a validator times out waiting for leader.
///
/// In HotStuff-2, when a leader fails:
/// 1. Validators timeout and broadcast ViewChange
/// 2. New leader collects 2f+1 ViewChanges to form ViewChangeCertificate
/// 3. New leader broadcasts NewView to start the new view
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewChange {
    /// Consensus epoch that authenticated this view change.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
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
        data.extend_from_slice(b"HYPERLICKED_VIEW_CHANGE_V3");
        data.extend_from_slice(&self.epoch.to_le_bytes());
        data.extend_from_slice(&self.committee_hash);
        data.extend_from_slice(&self.genesis_hash);
        data.extend_from_slice(&self.from_view.to_le_bytes());
        data.extend_from_slice(&self.to_view.to_le_bytes());
        if let Some(ref qc) = self.high_qc {
            data.push(1);
            data.extend_from_slice(&qc.epoch.to_le_bytes());
            data.extend_from_slice(&qc.committee_hash);
            data.extend_from_slice(&qc.genesis_hash);
            data.extend_from_slice(&qc.view.to_le_bytes());
            data.extend_from_slice(&qc.block_hash);
            match qc.app_hash {
                Some(app_hash) => {
                    data.push(1);
                    data.extend_from_slice(&app_hash);
                }
                None => data.push(0),
            }
        } else {
            data.push(0);
        }
        data
    }

    /// Return the authentication context carried by this view change.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this view change belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err(
                "view-change context does not match expected consensus context".to_string(),
            );
        }
        Ok(())
    }
}

/// ViewChangeCertificate: proof that 2f+1 validators agreed to change views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewChangeCertificate {
    /// Consensus epoch that authenticated this view-change certificate.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    pub view_changes: Vec<ViewChange>,
    /// Aggregated signature (BLS when implemented)
    pub agg_signature: Vec<u8>,
}

impl ViewChangeCertificate {
    /// Create from collected ViewChange messages.
    ///
    /// If all ViewChanges have valid BLS signatures (96 bytes), aggregates them.
    /// Otherwise falls back to concatenating signatures.
    pub fn new(
        context: ConsensusContext,
        view: View,
        view_changes: Vec<ViewChange>,
    ) -> Result<Self, String> {
        use crate::crypto::bls::{aggregate_signatures, BlsSignature};

        if view_changes.is_empty() {
            return Err("cannot form a view-change certificate without messages".to_string());
        }
        if view_changes
            .iter()
            .any(|view_change| view_change.context() != context)
        {
            return Err("view-change certificate contains mixed contexts".to_string());
        }
        if view_changes
            .iter()
            .any(|view_change| view_change.to_view != view)
        {
            return Err("view-change certificate contains a different target view".to_string());
        }

        // Try to collect BLS signatures from view changes
        let bls_sigs: Vec<BlsSignature> = view_changes
            .iter()
            .filter(|vc| vc.signature.len() == 96)
            .filter_map(|vc| BlsSignature::from_slice(&vc.signature).ok())
            .collect();

        // Use BLS aggregation if we have enough valid BLS signatures for quorum
        // (at least half+1 of the view changes should have valid BLS sigs)
        let quorum_threshold = view_changes.len() / 2 + 1;
        let agg_signature = if bls_sigs.len() >= quorum_threshold {
            aggregate_signatures(&bls_sigs)
                .map(|a| a.to_bytes().to_vec())
                .unwrap_or_else(|_| Self::concat_signatures(&view_changes))
        } else {
            Self::concat_signatures(&view_changes)
        };

        Ok(Self {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view,
            view_changes,
            agg_signature,
        })
    }

    /// Concatenate signatures (fallback when BLS aggregation not available)
    fn concat_signatures(view_changes: &[ViewChange]) -> Vec<u8> {
        view_changes
            .iter()
            .flat_map(|vc| vc.signature.iter().copied())
            .collect()
    }

    /// Get the highest QC among all ViewChange messages
    pub fn highest_qc(&self) -> Option<&Certificate> {
        self.view_changes
            .iter()
            .filter_map(|vc| vc.high_qc.as_ref())
            .max_by_key(|qc| (qc.view, qc.block_hash))
    }

    /// Return the authentication context carried by this certificate.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this certificate belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err(
                "view-change certificate context does not match expected consensus context"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// NewView message: sent by new leader after collecting ViewChange quorum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewView {
    /// Consensus epoch that authenticated this new-view message.
    pub epoch: u64,
    /// Canonical validator committee hash for `epoch`.
    pub committee_hash: Hash,
    /// Cryptographic genesis domain for this chain.
    pub genesis_hash: Hash,
    pub view: View,
    /// The highest QC among all ViewChange messages
    pub high_qc: Option<Certificate>,
    /// Proof that 2f+1 validators agreed to this view change
    pub view_change_cert: ViewChangeCertificate,
}

impl NewView {
    /// Return the authentication context carried by this new-view message.
    pub const fn context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(self.epoch, self.committee_hash, self.genesis_hash)
    }

    /// Check that this new-view message belongs to the expected context.
    pub fn validate_context(&self, expected: ConsensusContext) -> Result<(), String> {
        if self.context() != expected {
            return Err("new-view context does not match expected consensus context".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::bls::BlsSecretKey;

    fn context() -> ConsensusContext {
        ConsensusContext::new(0, [7u8; 32])
    }

    #[test]
    fn timeout_signature_binds_context() {
        let secret = BlsSecretKey::from_seed(&[12u8; 32]);
        let mut timeout = Timeout {
            epoch: context().epoch,
            committee_hash: context().committee_hash,
            genesis_hash: context().genesis_hash,
            view: 3,
            high_qc_view: 2,
            sender: [1u8; 32],
            signature: vec![],
        };
        timeout.signature = secret.sign(&timeout.signing_data()).to_bytes().to_vec();

        let signature = crate::crypto::bls::BlsSignature::from_slice(&timeout.signature).unwrap();
        assert!(secret
            .public_key()
            .verify(&timeout.signing_data(), &signature));

        timeout.epoch = 1;
        assert!(!secret
            .public_key()
            .verify(&timeout.signing_data(), &signature));

        timeout.epoch = context().epoch;
        timeout.genesis_hash[0] ^= 1;
        assert!(!secret
            .public_key()
            .verify(&timeout.signing_data(), &signature));
    }

    #[test]
    fn view_change_signature_binds_context() {
        let secret = BlsSecretKey::from_seed(&[13u8; 32]);
        let mut view_change = ViewChange {
            epoch: context().epoch,
            committee_hash: context().committee_hash,
            genesis_hash: context().genesis_hash,
            from_view: 3,
            to_view: 4,
            high_qc: None,
            sender: [1u8; 32],
            signature: vec![],
        };
        view_change.signature = secret.sign(&view_change.signing_data()).to_bytes().to_vec();

        let signature =
            crate::crypto::bls::BlsSignature::from_slice(&view_change.signature).unwrap();
        assert!(secret
            .public_key()
            .verify(&view_change.signing_data(), &signature));

        view_change.committee_hash[0] ^= 1;
        assert!(!secret
            .public_key()
            .verify(&view_change.signing_data(), &signature));

        view_change.committee_hash = context().committee_hash;
        view_change.genesis_hash[0] ^= 1;
        assert!(!secret
            .public_key()
            .verify(&view_change.signing_data(), &signature));
    }
}

// =============================================================================
// Sync Protocol Messages
// =============================================================================

/// Request blocks for catchup from a peer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRequest {
    /// Starting height (inclusive)
    pub from_height: u64,
    /// Ending height (optional, defaults to peer's latest)
    pub to_height: Option<u64>,
    /// Maximum blocks to return
    pub max_blocks: u64,
    /// Request ID for correlation
    pub request_id: u64,
}

/// Response with blocks for catchup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResponse {
    /// Request ID for correlation
    pub request_id: u64,
    /// Blocks in ascending height order
    pub blocks: Vec<Block>,
    /// Peer's current height (for progress tracking)
    pub peer_height: u64,
    /// True if there are more blocks after this batch
    pub has_more: bool,
}

/// Request a snapshot for fast sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotRequest {
    /// Specific height (None = latest)
    pub height: Option<u64>,
    /// Request ID for correlation
    pub request_id: u64,
}

/// Response with snapshot data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResponse {
    /// Request ID for correlation
    pub request_id: u64,
    /// Snapshot height (None if not found)
    pub height: Option<u64>,
    /// Snapshot data (compressed, None if not found)
    pub data: Option<Vec<u8>>,
    /// Whether data is compressed
    pub compressed: bool,
}

/// All network message types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    // Consensus messages
    Propose(Propose),
    Vote(super::Vote),
    Prepare(Prepare),
    ViewChange(ViewChange),
    NewView(NewView),
    Timeout(Timeout),
    // Sync messages
    SyncRequest(SyncRequest),
    SyncResponse(SyncResponse),
    SnapshotRequest(SnapshotRequest),
    SnapshotResponse(SnapshotResponse),
    // Gossip-wrapped message for epidemic propagation
    Gossip(Box<crate::network::GossipMessage>),
    // Durable, committee-authenticated equivocation evidence.  Keep this
    // appended so existing bincode variant indices remain unchanged.
    EquivocationEvidence(crate::consensus::EquivocationProof),
    // Canonical signed user transaction.  Keep every new wire variant
    // appended: bincode encodes enum discriminants positionally.
    UserTransaction(SignedEnvelope),
}
