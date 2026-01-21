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
    Vote(super::Vote),
    Prepare(Prepare),
    ViewChange(ViewChange),
    NewView(NewView),
}
