//! Equivocation Detection
//!
//! Detects when validators equivocate (vote for conflicting blocks in same view).
//! Generates evidence that can be submitted to the staking module for slashing.
//!
//! ## Safety Guarantees
//!
//! This module provides Byzantine fault detection - when a validator sends conflicting
//! votes or proposals for the same view, we generate cryptographic evidence that can
//! be used to slash their stake.
//!
//! ## Integration
//!
//! The detector should be called whenever a vote is received. If equivocation is
//! detected, the evidence should be submitted to StakingState::submit_evidence().

use std::collections::HashMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tracing::warn;

use crate::types::{ConsensusContext, Hash, NodeId, View, Vote};

/// BLS signatures carried by an equivocation proof are fixed-width.
pub const EQUIVOCATION_SIGNATURE_BYTES: usize = 96;
/// Version byte for the durable proof-journal key.
pub const EQUIVOCATION_JOURNAL_KEY_VERSION: u8 = 1;

/// Tracks votes and detects equivocation
pub struct EquivocationDetector {
    /// Votes per (view, voter) -> (block_hash, app_hash, signature)
    /// If we see a different block_hash for the same (view, voter), that's equivocation
    votes: HashMap<(u64, Hash, Hash, View, NodeId), (Hash, Hash, Vec<u8>)>,

    /// Detected equivocations (view, offender) -> (hash_a, hash_b, sig_a, sig_b)
    equivocations: HashMap<(u64, Hash, Hash, View, NodeId), EquivocationProof>,

    /// Maximum view to keep (for garbage collection)
    prune_below: View,

    /// Optional static context. Votes from another epoch or committee are
    /// rejected before they enter the detector.
    context: Option<ConsensusContext>,
}

/// Proof of equivocation (double voting)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquivocationProof {
    /// Consensus context in which the conflicting votes were signed.
    pub context: ConsensusContext,
    /// The validator who equivocated
    pub offender: NodeId,
    /// The view where equivocation occurred
    pub view: View,
    /// First block hash voted for
    pub hash_a: Hash,
    /// Application state hash committed by the first vote.
    pub app_hash_a: Hash,
    /// Second block hash voted for
    pub hash_b: Hash,
    /// Application state hash committed by the second vote.
    pub app_hash_b: Hash,
    /// Signature on first vote
    pub signature_a: Vec<u8>,
    /// Signature on second vote
    pub signature_b: Vec<u8>,
}

/// Wire representation kept separate so proof deserialization can reject
/// malformed or non-canonical records before they enter a journal.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EquivocationProofWire {
    context: ConsensusContext,
    offender: NodeId,
    view: View,
    hash_a: Hash,
    app_hash_a: Hash,
    hash_b: Hash,
    app_hash_b: Hash,
    #[serde(with = "fixed_signature")]
    signature_a: [u8; EQUIVOCATION_SIGNATURE_BYTES],
    #[serde(with = "fixed_signature")]
    signature_b: [u8; EQUIVOCATION_SIGNATURE_BYTES],
}

mod fixed_signature {
    use serde::de::{Error as DeError, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S>(
        value: &[u8; super::EQUIVOCATION_SIGNATURE_BYTES],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tuple = serializer.serialize_tuple(super::EQUIVOCATION_SIGNATURE_BYTES)?;
        for byte in value {
            tuple.serialize_element(byte)?;
        }
        tuple.end()
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<[u8; super::EQUIVOCATION_SIGNATURE_BYTES], D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SignatureVisitor;

        impl<'de> Visitor<'de> for SignatureVisitor {
            type Value = [u8; super::EQUIVOCATION_SIGNATURE_BYTES];

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("exactly 96 signature bytes")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut value = [0u8; super::EQUIVOCATION_SIGNATURE_BYTES];
                for (index, byte) in value.iter_mut().enumerate() {
                    *byte = sequence
                        .next_element()?
                        .ok_or_else(|| A::Error::invalid_length(index, &self))?;
                }
                if sequence.next_element::<u8>()?.is_some() {
                    return Err(A::Error::invalid_length(
                        super::EQUIVOCATION_SIGNATURE_BYTES + 1,
                        &self,
                    ));
                }
                Ok(value)
            }
        }

        deserializer.deserialize_tuple(super::EQUIVOCATION_SIGNATURE_BYTES, SignatureVisitor)
    }
}

impl From<&EquivocationProof> for EquivocationProofWire {
    fn from(proof: &EquivocationProof) -> Self {
        Self {
            context: proof.context,
            offender: proof.offender,
            view: proof.view,
            hash_a: proof.hash_a,
            app_hash_a: proof.app_hash_a,
            hash_b: proof.hash_b,
            app_hash_b: proof.app_hash_b,
            signature_a: proof
                .signature_a
                .as_slice()
                .try_into()
                .expect("canonical proof signature length was checked"),
            signature_b: proof
                .signature_b
                .as_slice()
                .try_into()
                .expect("canonical proof signature length was checked"),
        }
    }
}

impl Serialize for EquivocationProof {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::Error as _;

        self.validate_canonical().map_err(S::Error::custom)?;
        EquivocationProofWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EquivocationProof {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        let wire = EquivocationProofWire::deserialize(deserializer)?;
        let proof = Self {
            context: wire.context,
            offender: wire.offender,
            view: wire.view,
            hash_a: wire.hash_a,
            app_hash_a: wire.app_hash_a,
            hash_b: wire.hash_b,
            app_hash_b: wire.app_hash_b,
            signature_a: wire.signature_a.to_vec(),
            signature_b: wire.signature_b.to_vec(),
        };
        proof.validate_canonical().map_err(D::Error::custom)?;
        Ok(proof)
    }
}

impl EquivocationProof {
    /// Return a proof with its two signed vote tuples in canonical order.
    /// Hashes are distinct for an actionable double-vote proof, so ordering by
    /// block hash also orders the complete `(block, app, signature)` tuple.
    pub fn canonicalized(&self) -> Result<Self, String> {
        let mut proof = self.clone();
        if proof.hash_a > proof.hash_b {
            std::mem::swap(&mut proof.hash_a, &mut proof.hash_b);
            std::mem::swap(&mut proof.app_hash_a, &mut proof.app_hash_b);
            std::mem::swap(&mut proof.signature_a, &mut proof.signature_b);
        }
        proof.validate_canonical()?;
        Ok(proof)
    }

    /// Validate the strict canonical journal representation.
    pub fn validate_canonical(&self) -> Result<(), String> {
        if !self.context.has_genesis_domain() {
            return Err("equivocation proof has a zero genesis context".to_string());
        }
        if self.signature_a.len() != EQUIVOCATION_SIGNATURE_BYTES
            || self.signature_b.len() != EQUIVOCATION_SIGNATURE_BYTES
        {
            return Err(format!(
                "equivocation proof signatures must be exactly {EQUIVOCATION_SIGNATURE_BYTES} bytes"
            ));
        }
        if self.hash_a == self.hash_b {
            return Err("equivocation proof must contain different block hashes".to_string());
        }
        if self.hash_a > self.hash_b {
            return Err("equivocation proof vote tuple is not canonical".to_string());
        }
        Ok(())
    }

    /// Stable key for one actionable proof per authenticated context/offender.
    /// The view and conflicting hashes intentionally do not participate: the
    /// first valid proof for a validator in a context wins.
    pub fn journal_key(&self) -> Vec<u8> {
        let mut key = Vec::with_capacity(1 + 8 + 32 + 32 + 32);
        key.push(EQUIVOCATION_JOURNAL_KEY_VERSION);
        key.extend_from_slice(&self.context.epoch.to_be_bytes());
        key.extend_from_slice(&self.context.committee_hash);
        key.extend_from_slice(&self.context.genesis_hash);
        key.extend_from_slice(&self.offender);
        key
    }
}

/// Result of checking a vote for equivocation
pub enum VoteCheckResult {
    /// Vote is valid (first vote for this view from this voter)
    Valid,
    /// Duplicate vote (same block hash) - allowed
    Duplicate,
    /// Equivocation detected!
    Equivocation(EquivocationProof),
    /// Vote belongs to a different configured consensus context.
    ContextMismatch,
}

impl EquivocationDetector {
    pub fn new() -> Self {
        Self {
            votes: HashMap::new(),
            equivocations: HashMap::new(),
            prune_below: 0,
            context: None,
        }
    }

    /// Create a detector bound to the static committee context.
    pub fn new_with_context(context: ConsensusContext) -> Self {
        let mut detector = Self::new();
        detector.context = Some(context);
        detector
    }

    /// Bind this detector to a static consensus context.
    pub fn set_context(&mut self, context: ConsensusContext) -> Result<(), String> {
        if let Some(existing) = self.context {
            if existing != context {
                return Err("cannot change equivocation detector context".to_string());
            }
        }
        self.context = Some(context);
        Ok(())
    }

    /// Return the context enforced by this detector, if configured.
    pub fn context(&self) -> Option<ConsensusContext> {
        self.context
    }

    /// Check a vote for equivocation.
    ///
    /// Returns `VoteCheckResult::Equivocation` if this voter already voted for
    /// a different block in this view.
    pub fn check_vote(&mut self, vote: &Vote) -> VoteCheckResult {
        if let Some(expected) = self.context {
            if vote.context() != expected {
                return VoteCheckResult::ContextMismatch;
            }
        } else {
            self.context = Some(vote.context());
        }

        let context = vote.context();
        let key = (
            context.epoch,
            context.committee_hash,
            context.genesis_hash,
            vote.view,
            vote.voter,
        );

        if let Some((existing_hash, existing_app_hash, existing_sig)) = self.votes.get(&key) {
            if *existing_hash == vote.block_hash {
                // Same vote, not equivocation (just duplicate)
                return VoteCheckResult::Duplicate;
            }

            // Different block hash for same (view, voter) = EQUIVOCATION
            let proof = EquivocationProof {
                context,
                offender: vote.voter,
                view: vote.view,
                hash_a: *existing_hash,
                app_hash_a: *existing_app_hash,
                hash_b: vote.block_hash,
                app_hash_b: vote.app_hash,
                signature_a: existing_sig.clone(),
                signature_b: vote.signature.clone(),
            };

            warn!(
                view = vote.view,
                offender = %hex::encode(&vote.voter[..4]),
                hash_a = %hex::encode(&existing_hash[..4]),
                hash_b = %hex::encode(&vote.block_hash[..4]),
                "EQUIVOCATION DETECTED: Double vote!"
            );

            // Record the equivocation
            self.equivocations.insert(key, proof.clone());

            return VoteCheckResult::Equivocation(proof);
        }

        // First vote from this voter for this view - record it
        self.votes.insert(
            key,
            (vote.block_hash, vote.app_hash, vote.signature.clone()),
        );

        VoteCheckResult::Valid
    }

    /// Get all detected equivocations
    pub fn get_equivocations(&self) -> Vec<EquivocationProof> {
        self.equivocations.values().cloned().collect()
    }

    /// Take and clear detected equivocations (for processing)
    pub fn take_equivocations(&mut self) -> Vec<EquivocationProof> {
        std::mem::take(&mut self.equivocations)
            .into_values()
            .collect()
    }

    /// Check if a specific validator has equivocated
    pub fn has_equivocated(&self, voter: &NodeId) -> bool {
        self.equivocations.keys().any(|(_, _, _, _, v)| v == voter)
    }

    /// Get equivocation proof for a specific (view, voter) if it exists
    pub fn get_equivocation(&self, view: View, voter: &NodeId) -> Option<&EquivocationProof> {
        self.equivocations
            .iter()
            .find_map(|((_, _, _, stored_view, stored_voter), proof)| {
                (*stored_view == view && stored_voter == voter).then_some(proof)
            })
    }

    /// Prune old votes to save memory.
    ///
    /// Should be called after commits to remove votes for views that can't
    /// generate new equivocations (already committed).
    pub fn prune_below(&mut self, view: View) {
        if view <= self.prune_below {
            return;
        }
        self.prune_below = view;
        self.votes.retain(|(_, _, _, v, _), _| *v >= view);
        // Keep equivocations longer (they're evidence for slashing)
        // Only prune very old ones
        if view > 1000 {
            self.equivocations
                .retain(|(_, _, _, v, _), _| *v >= view.saturating_sub(1000));
        }
    }

    /// Get statistics for monitoring
    pub fn stats(&self) -> EquivocationStats {
        EquivocationStats {
            tracked_votes: self.votes.len(),
            detected_equivocations: self.equivocations.len(),
            unique_offenders: self
                .equivocations
                .values()
                .map(|e| e.offender)
                .collect::<std::collections::HashSet<_>>()
                .len(),
        }
    }
}

impl Default for EquivocationDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics about equivocation detection
#[derive(Debug, Clone)]
pub struct EquivocationStats {
    /// Number of votes being tracked
    pub tracked_votes: usize,
    /// Number of equivocations detected
    pub detected_equivocations: usize,
    /// Number of unique offenders
    pub unique_offenders: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_vote(view: View, voter_id: u8, block_hash: u8) -> Vote {
        let mut voter = [0u8; 32];
        voter[0] = voter_id;

        let mut hash = [0u8; 32];
        hash[0] = block_hash;

        Vote {
            epoch: 0,
            committee_hash: [0u8; 32],
            genesis_hash: [0u8; 32],
            view,
            block_hash: hash,
            app_hash: [0u8; 32],
            voter,
            signature: vec![voter_id, block_hash],
            bls_pubkey: None,
        }
    }

    #[test]
    fn test_first_vote_valid() {
        let mut detector = EquivocationDetector::new();
        let vote = test_vote(1, 1, 10);

        match detector.check_vote(&vote) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result"),
        }
    }

    #[test]
    fn test_duplicate_vote_allowed() {
        let mut detector = EquivocationDetector::new();
        let vote = test_vote(1, 1, 10);

        detector.check_vote(&vote);
        match detector.check_vote(&vote) {
            VoteCheckResult::Duplicate => {}
            _ => panic!("Expected Duplicate result"),
        }
    }

    #[test]
    fn test_equivocation_detected() {
        let mut detector = EquivocationDetector::new();

        // First vote for block 10
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Second vote for different block 20 in same view
        let vote2 = test_vote(1, 1, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Equivocation(proof) => {
                assert_eq!(proof.view, 1);
                assert_eq!(proof.hash_a[0], 10);
                assert_eq!(proof.hash_b[0], 20);
            }
            _ => panic!("Expected Equivocation result"),
        }

        assert!(detector.has_equivocated(&vote1.voter));
    }

    #[test]
    fn test_different_views_not_equivocation() {
        let mut detector = EquivocationDetector::new();

        // Vote for block 10 in view 1
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Vote for different block 20 in view 2 (different view, OK)
        let vote2 = test_vote(2, 1, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result for different view"),
        }
    }

    #[test]
    fn test_different_voters_not_equivocation() {
        let mut detector = EquivocationDetector::new();

        // Voter 1 votes for block 10
        let vote1 = test_vote(1, 1, 10);
        detector.check_vote(&vote1);

        // Voter 2 votes for different block 20 (different voter, OK)
        let vote2 = test_vote(1, 2, 20);
        match detector.check_vote(&vote2) {
            VoteCheckResult::Valid => {}
            _ => panic!("Expected Valid result for different voter"),
        }
    }

    #[test]
    fn test_prune_old_votes() {
        let mut detector = EquivocationDetector::new();

        // Add votes for views 1, 2, 3
        for view in 1..=3 {
            let vote = test_vote(view, 1, view as u8);
            detector.check_vote(&vote);
        }

        assert_eq!(detector.votes.len(), 3);

        // Prune below view 3
        detector.prune_below(3);
        assert_eq!(detector.votes.len(), 1);
    }

    #[test]
    fn test_take_equivocations() {
        let mut detector = EquivocationDetector::new();

        // Create equivocation
        detector.check_vote(&test_vote(1, 1, 10));
        detector.check_vote(&test_vote(1, 1, 20));

        let equivocations = detector.take_equivocations();
        assert_eq!(equivocations.len(), 1);

        // Should be empty after taking
        assert!(detector.take_equivocations().is_empty());
    }
}
