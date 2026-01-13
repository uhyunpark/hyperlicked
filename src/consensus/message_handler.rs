//! Message Handler - Common message processing logic

use tracing::debug;

use super::{Pacemaker, Safety};
use crate::types::{hash_short, Hash, Message, NodeId, ViewChangeCertificate, Vote};

/// Process view-related messages (ViewChange, NewView)
pub fn handle_view_message(
    msg: &Message,
    from: &NodeId,
    pacemaker: &mut Pacemaker,
    safety: &mut Safety,
) -> Option<ViewChangeCertificate> {
    match msg {
        Message::ViewChange(vc) => {
            debug!(from = %hash_short(from), to_view = vc.to_view, "Received ViewChange");
            pacemaker.on_view_change(vc.clone())
        }
        Message::NewView(nv) => {
            debug!(view = nv.view, "Received NewView");
            pacemaker.on_new_view(nv);
            if let Some(qc) = &nv.high_qc {
                safety.update_high_qc(qc.clone());
            }
            None
        }
        _ => None,
    }
}

/// Store a vote in the votes map
pub fn store_vote(votes: &mut std::collections::HashMap<Hash, Vec<Vote>>, vote: Vote) {
    votes.entry(vote.block_hash).or_default().push(vote);
}
