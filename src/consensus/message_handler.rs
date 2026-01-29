//! Message Handler - Common message processing logic

use tracing::{debug, warn};

use super::view_change::{validate_view_change_bounds, MAX_FUTURE_VIEWS};
use super::{EquivocationDetector, EquivocationProof, Pacemaker, Safety, VoteCheckResult};
use crate::types::{hash_short, Hash, Message, NewView, NodeId, ViewChangeCertificate, Vote};

/// Validate a NewView message before processing.
///
/// Checks:
/// 1. VCC view matches NewView view
/// 2. high_qc is actually the highest QC among the VCC's view changes
///
/// Returns an error string if validation fails.
fn validate_new_view(nv: &NewView) -> Result<(), &'static str> {
    // Rule 1: VCC view must match NewView view
    if nv.view_change_cert.view != nv.view {
        return Err("VCC view does not match NewView view");
    }

    // Rule 2: high_qc must be the highest among VCC's view changes
    let vcc_highest_qc = nv.view_change_cert.highest_qc();
    let vcc_highest_view = vcc_highest_qc.map(|qc| qc.view).unwrap_or(0);

    let claimed_qc_view = nv.high_qc.as_ref().map(|qc| qc.view).unwrap_or(0);

    // The claimed high_qc must not be lower than any QC in the VCC
    if claimed_qc_view < vcc_highest_view {
        return Err("NewView high_qc is lower than highest QC in VCC (leader hiding higher QC)");
    }

    // If claimed QC is higher, it must match one of the VCC's QCs
    // (leader can't just make up a higher QC)
    if claimed_qc_view > vcc_highest_view {
        // Check if the claimed QC matches any QC in the VCC
        let qc_found = nv.view_change_cert.view_changes.iter().any(|vc| {
            vc.high_qc
                .as_ref()
                .map(|qc| qc.view == claimed_qc_view)
                .unwrap_or(false)
        });
        if !qc_found && nv.high_qc.is_some() {
            return Err("NewView high_qc is not from any ViewChange in VCC");
        }
    }

    Ok(())
}

/// Process view-related messages (ViewChange, NewView)
pub fn handle_view_message(
    msg: &Message,
    from: &NodeId,
    pacemaker: &mut Pacemaker,
    safety: &mut Safety,
) -> Option<ViewChangeCertificate> {
    match msg {
        Message::ViewChange(vc) => {
            let current_view = pacemaker.current_view();
            // Pre-check bounds before passing to pacemaker
            if let Err(e) = validate_view_change_bounds(vc, current_view) {
                warn!(
                    from = %hash_short(from),
                    to_view = vc.to_view,
                    current_view,
                    max_future = MAX_FUTURE_VIEWS,
                    error = %e,
                    "Rejecting ViewChange: too far ahead"
                );
                return None;
            }
            debug!(from = %hash_short(from), to_view = vc.to_view, "Received ViewChange");
            pacemaker.on_view_change(vc.clone())
        }
        Message::NewView(nv) => {
            debug!(view = nv.view, "Received NewView");

            // Validate VCC before processing
            if let Err(reason) = validate_new_view(nv) {
                warn!(
                    view = nv.view,
                    from = %hash_short(from),
                    reason,
                    "Rejecting invalid NewView message"
                );
                return None;
            }

            pacemaker.on_new_view(nv);
            if let Some(qc) = &nv.high_qc {
                safety.update_high_qc(qc.clone());
            }
            None
        }
        _ => None,
    }
}

/// Store a vote in the votes map and check for equivocation.
///
/// Returns `Some(EquivocationProof)` if this vote constitutes equivocation
/// (the same voter voted for a different block in the same view).
pub fn store_vote_with_equivocation_check(
    votes: &mut std::collections::HashMap<Hash, Vec<Vote>>,
    vote: Vote,
    equivocation_detector: &mut EquivocationDetector,
) -> Option<EquivocationProof> {
    // Check for equivocation BEFORE storing
    let result = equivocation_detector.check_vote(&vote);

    // Store the vote regardless (we need to track it for quorum)
    votes.entry(vote.block_hash).or_default().push(vote);

    // Return equivocation proof if detected
    match result {
        VoteCheckResult::Equivocation(proof) => Some(proof),
        _ => None,
    }
}

/// Store a vote in the votes map (legacy function without equivocation check)
#[allow(dead_code)]
pub fn store_vote(votes: &mut std::collections::HashMap<Hash, Vec<Vote>>, vote: Vote) {
    votes.entry(vote.block_hash).or_default().push(vote);
}
