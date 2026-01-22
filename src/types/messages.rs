//! Network Messages
//!
//! Protocol messages for HotStuff-2 consensus.

use serde::{Deserialize, Serialize};

use super::{Block, Certificate, NodeId, Signature, View};

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

/// Timeout message: sent when a validator times out waiting for leader.
///
/// This is a simplified message specifically for timeout aggregation.
/// Contains only the essential data needed for BLS signature aggregation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeout {
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
        data.extend_from_slice(b"TIMEOUT"); // Domain separator
        data.extend_from_slice(&self.view.to_le_bytes());
        data
    }
}

/// TimeoutCertificate: cryptographic proof that 2f+1 validators timed out.
///
/// This provides Byzantine safety for view changes - proves that enough
/// honest validators agreed the leader failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeoutCertificate {
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
        data.extend_from_slice(b"TIMEOUT");
        data.extend_from_slice(&self.view.to_le_bytes());
        data
    }
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
}
