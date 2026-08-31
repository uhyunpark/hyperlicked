//! Message Handler - Common message processing logic

use super::{EquivocationDetector, EquivocationProof, VoteCheckResult};
use crate::types::{ConsensusContext, Hash, Vote};

/// Store a vote only when it belongs to the runner's static consensus context.
///
/// Context validation happens before the vote reaches either the equivocation
/// detector or the per-block collection, so a stale/future committee vote
/// cannot mutate consensus bookkeeping.
pub fn store_vote_with_context(
    votes: &mut std::collections::HashMap<Hash, Vec<Vote>>,
    vote: Vote,
    expected_context: ConsensusContext,
    equivocation_detector: &mut EquivocationDetector,
) -> Option<EquivocationProof> {
    if vote.validate_context(expected_context).is_err() {
        return None;
    }

    store_vote_with_equivocation_check(votes, vote, equivocation_detector)
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
