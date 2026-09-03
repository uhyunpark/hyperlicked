//! Authenticated epoch-transition candidates.
//!
//! This module owns the first cross-epoch protocol boundary. A staking
//! result is only a candidate until an old-committee QC, the finalized block
//! it certifies, and that block's authenticated state root all agree. The
//! candidate can then be durably staged for a later runtime activation. The
//! live runner intentionally does not swap its committee from this module:
//! network admission, pacemaker, safety, and restart recovery must move at
//! one atomic boundary.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::app::staking::ValidatorSetUpdate;
use crate::consensus::committee::verify_certificate;
use crate::types::{
    Block, Certificate, Committee, CommitteeMember, ConsensusContext, Hash,
    CONSENSUS_STATE_ROOT_SCHEMA_VERSION, MAX_COMMITTEE_MEMBERS,
};

/// Version of the staged epoch-transition proof wire object.
pub const EPOCH_TRANSITION_PROOF_SCHEMA_VERSION: u16 = 1;

/// Maximum serialized transition proof accepted by storage and ingress.
///
/// A 21-member committee with compressed BLS keys is far below this bound;
/// keeping a fixed bound prevents a malformed marker from becoming an
/// unbounded restart allocation.
pub const MAX_EPOCH_TRANSITION_PROOF_BYTES: usize = 256 * 1024;

/// State-root reference authenticated by the old finalized block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRootReference {
    pub height: u64,
    pub schema_version: u16,
    pub root: Hash,
}

impl StateRootReference {
    pub const fn new(height: u64, root: Hash) -> Self {
        Self {
            height,
            schema_version: CONSENSUS_STATE_ROOT_SCHEMA_VERSION,
            root,
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != CONSENSUS_STATE_ROOT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported transition state-root schema version {}",
                self.schema_version
            ));
        }
        if self.root == [0u8; 32] {
            return Err("transition state-root reference must be nonzero".to_string());
        }
        Ok(())
    }
}

/// Proof that an old finalized context authenticated the next committee.
///
/// `old_qc` must certify `old_block` (the staged terminal block at
/// `effective_height - 2`). Its old-context child is already the next
/// certified height under HotStuff-2, so the candidate never claims that
/// child height for a new context.
/// The block itself is passed to [`EpochTransitionProof::validate`] rather
/// than duplicated in this object; this prevents a proof from carrying two
/// independently trusted block headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpochTransitionProof {
    pub schema_version: u16,
    /// This tranche only persists a candidate. No runtime activation variant
    /// exists until the old/new cross-context boundary is implemented in
    /// app, safety, pacemaker, network, and recovery together.
    pub activation: EpochTransitionActivation,
    pub old_context: ConsensusContext,
    pub old_qc: Certificate,
    pub next_epoch: u64,
    pub next_committee: Vec<CommitteeMember>,
    pub next_committee_hash: Hash,
    pub effective_height: u64,
    pub state_root: StateRootReference,
}

/// Explicit activation status of a transition marker.
///
/// Keeping this in the authenticated marker prevents a future caller from
/// treating a staged proof as an already-authorized runtime swap. The only
/// status currently constructible is `StagedOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EpochTransitionActivation {
    StagedOnly,
}

impl EpochTransitionProof {
    /// Construct a proof from a finalized block and a staking-derived update.
    /// Cryptographic and finalized-chain checks are performed by `validate`.
    pub fn from_validator_set_update(
        old_context: ConsensusContext,
        old_qc: Certificate,
        finalized_block: &Block,
        state_root: Hash,
        update: &ValidatorSetUpdate,
    ) -> Result<Self, String> {
        let next_committee = committee_members_from_update(update)?;
        let committee = Committee::from_members(next_committee.clone())?;
        let next_epoch = old_context
            .epoch
            .checked_add(1)
            .ok_or_else(|| "next epoch overflows u64".to_string())?;
        let effective_height = finalized_block
            .height
            .checked_add(2)
            .ok_or_else(|| "effective transition height overflows u64".to_string())?;

        Ok(Self {
            schema_version: EPOCH_TRANSITION_PROOF_SCHEMA_VERSION,
            activation: EpochTransitionActivation::StagedOnly,
            old_context,
            old_qc,
            next_epoch,
            next_committee,
            next_committee_hash: committee.hash(),
            effective_height,
            state_root: StateRootReference::new(finalized_block.height, state_root),
        })
    }

    /// Validate every proof binding before it can be staged or activated.
    ///
    /// `finalized_block` must come from the local finalized height index. The
    /// caller is responsible for checking that property; this function makes
    /// sure the supplied block and old QC agree exactly and that the next set
    /// is canonical and bounded.
    pub fn validate(
        &self,
        old_committee: &Committee,
        finalized_block: &Block,
        require_bls_signature: bool,
    ) -> Result<Committee, String> {
        if self.schema_version != EPOCH_TRANSITION_PROOF_SCHEMA_VERSION {
            return Err(format!(
                "unsupported epoch-transition proof schema version {}",
                self.schema_version
            ));
        }
        if self.activation != EpochTransitionActivation::StagedOnly {
            return Err("epoch-transition proof is not a staged-only candidate".to_string());
        }
        if self.old_context != finalized_block.context() {
            return Err("transition proof old context does not match finalized block".to_string());
        }
        old_committee.validate_context(self.old_context)?;
        if finalized_block.height == 0 {
            return Err("genesis cannot be an epoch-transition block".to_string());
        }
        finalized_block.validate()?;

        if self.old_qc.context() != self.old_context {
            return Err("transition proof old QC has the wrong context".to_string());
        }
        let finalized_hash = finalized_block.hash();
        if self.old_qc.block_hash != finalized_hash {
            return Err("transition proof old QC does not certify the finalized block".to_string());
        }
        if self.old_qc.view != finalized_block.view {
            return Err("transition proof old QC view does not match finalized block".to_string());
        }
        verify_certificate(
            old_committee,
            &self.old_qc,
            self.old_context,
            finalized_block.view,
            &finalized_hash,
            Some(&finalized_block.app_hash),
            require_bls_signature,
        )?;

        self.state_root.validate()?;
        if self.state_root.height != finalized_block.height {
            return Err("transition state-root height does not match finalized block".to_string());
        }
        if self.state_root.root != finalized_block.app_hash {
            return Err(
                "transition state-root does not match finalized block app hash".to_string(),
            );
        }

        let expected_next_epoch = self
            .old_context
            .epoch
            .checked_add(1)
            .ok_or_else(|| "next epoch overflows u64".to_string())?;
        if self.next_epoch != expected_next_epoch {
            return Err(format!(
                "transition next epoch {} is not old epoch {} + 1",
                self.next_epoch, self.old_context.epoch
            ));
        }
        // HotStuff-2 commits a block only after an old-context child is
        // certified. That child already occupies the next height, so a
        // staged candidate cannot claim `height + 1` as a first-new block.
        // The activation boundary is conservatively one further height away.
        let expected_effective_height = finalized_block
            .height
            .checked_add(2)
            .ok_or_else(|| "effective transition height overflows u64".to_string())?;
        if self.effective_height != expected_effective_height {
            return Err(format!(
                "transition effective height {} is not finalized height {} + 2",
                self.effective_height, finalized_block.height
            ));
        }

        if self.next_committee.is_empty() || self.next_committee.len() > MAX_COMMITTEE_MEMBERS {
            return Err(format!(
                "transition next committee must contain 1..={} members",
                MAX_COMMITTEE_MEMBERS
            ));
        }
        if self
            .next_committee
            .windows(2)
            .any(|pair| pair[0].node_id >= pair[1].node_id)
        {
            return Err("transition next committee is not in canonical node order".to_string());
        }
        let mut seen_keys = HashSet::with_capacity(self.next_committee.len());
        for member in &self.next_committee {
            let key = member
                .bls_pubkey
                .as_deref()
                .ok_or_else(|| "transition next committee member has no BLS key".to_string())?;
            if key.len() != 48 {
                return Err("transition next committee BLS key must be 48 bytes".to_string());
            }
            if !seen_keys.insert(key.to_vec()) {
                return Err("transition next committee contains duplicate BLS keys".to_string());
            }
            crate::crypto::bls::BlsPublicKey::from_bytes(
                key.try_into().map_err(|_| {
                    "transition next committee BLS key must be 48 bytes".to_string()
                })?,
            )
            .map_err(|_| "transition next committee contains an invalid BLS key".to_string())?;
        }

        let next_committee = Committee::from_members(self.next_committee.clone())?;
        if next_committee.hash() != self.next_committee_hash {
            return Err("transition next committee hash does not match members".to_string());
        }
        Ok(next_committee)
    }

    /// Validate the proof and bind its next committee to the exact
    /// validator-set update derived from finalized application state.
    ///
    /// The ordinary structural validation above intentionally has no access
    /// to application state.  Staging callers must use this binding variant;
    /// otherwise a caller could provide a different, independently valid BLS
    /// committee and still obtain a structurally valid staged marker.
    pub fn validate_against_validator_set_update(
        &self,
        old_committee: &Committee,
        finalized_block: &Block,
        require_bls_signature: bool,
        update: &ValidatorSetUpdate,
    ) -> Result<Committee, String> {
        let next_committee =
            self.validate(old_committee, finalized_block, require_bls_signature)?;
        let expected_members = committee_members_from_update(update)?;
        if self.next_committee != expected_members {
            return Err(
                "transition next committee does not match the canonical validator-set update"
                    .to_string(),
            );
        }
        let expected_committee = Committee::from_members(expected_members)?;
        if next_committee != expected_committee {
            return Err(
                "transition next committee is not the canonical validator-set committee"
                    .to_string(),
            );
        }
        Ok(next_committee)
    }

    /// Return the candidate context that a future atomic activation would use.
    /// This is a value helper only; it never mutates a live runner.
    pub const fn candidate_context(&self) -> ConsensusContext {
        ConsensusContext::with_genesis(
            self.next_epoch,
            self.next_committee_hash,
            self.old_context.genesis_hash,
        )
    }

    /// Encode the exact bytes used by the durable staging marker.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        if self.schema_version != EPOCH_TRANSITION_PROOF_SCHEMA_VERSION {
            return Err(format!(
                "unsupported epoch-transition proof schema version {}",
                self.schema_version
            ));
        }
        let bytes = bincode::serialize(self)
            .map_err(|error| format!("cannot encode epoch-transition proof: {error}"))?;
        if bytes.len() > MAX_EPOCH_TRANSITION_PROOF_BYTES {
            return Err("epoch-transition proof exceeds its storage bound".to_string());
        }
        Ok(bytes)
    }

    /// Decode only the canonical bounded staging representation.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_EPOCH_TRANSITION_PROOF_BYTES {
            return Err("epoch-transition proof exceeds its storage bound".to_string());
        }
        let proof: Self = bincode::deserialize(bytes)
            .map_err(|error| format!("cannot decode epoch-transition proof: {error}"))?;
        let canonical = proof.canonical_bytes()?;
        if canonical != bytes {
            return Err("epoch-transition proof bytes are not canonical".to_string());
        }
        Ok(proof)
    }
}

/// Convert the staking layer's top-21 result to canonical committee material.
///
/// Staking sorts by bonded power/operator, while consensus hashes members by
/// node ID. This helper verifies both parallel arrays and then canonicalizes
/// the member order through `Committee::from_members`.
pub fn committee_members_from_update(
    update: &ValidatorSetUpdate,
) -> Result<Vec<CommitteeMember>, String> {
    if update.node_ids.is_empty() || update.node_ids.len() > MAX_COMMITTEE_MEMBERS {
        return Err(format!(
            "validator-set update must contain 1..={} validators",
            MAX_COMMITTEE_MEMBERS
        ));
    }
    if update.bls_pubkeys.len() != update.node_ids.len() {
        return Err("validator-set update BLS key count does not match node count".to_string());
    }
    if update.stakes.len() != update.node_ids.len() {
        return Err("validator-set update stake count does not match node count".to_string());
    }

    let mut members = Vec::with_capacity(update.node_ids.len());
    let mut seen_keys = HashSet::with_capacity(update.node_ids.len());
    for (index, node_id) in update.node_ids.iter().copied().enumerate() {
        let (stake_node, stake) = update.stakes[index];
        if stake_node != node_id {
            return Err("validator-set update stake order does not match node order".to_string());
        }
        if stake == 0 {
            return Err("validator-set update contains zero voting power".to_string());
        }
        let key = &update.bls_pubkeys[index];
        if key.len() != 48 {
            return Err("validator-set update BLS key must be 48 bytes".to_string());
        }
        if !seen_keys.insert(key.clone()) {
            return Err("validator-set update contains duplicate BLS keys".to_string());
        }
        members.push(CommitteeMember {
            node_id,
            bls_pubkey: Some(key.clone()),
            voting_power: u128::from(stake),
        });
    }

    let committee = Committee::from_members(members.clone())?;
    let canonical = committee.members().to_vec();
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::form_certificate;
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::{ConsensusConfig, Vote};

    fn config() -> (ConsensusConfig, BlsSecretKey) {
        let mut config = ConsensusConfig::single_node();
        config.genesis_hash = [0x11; 32];
        let secret = config.bls_secret_key().expect("test config has a BLS key");
        (config, secret)
    }

    fn transition_fixture() -> (EpochTransitionProof, Committee, Block) {
        let (config, secret) = config();
        let committee = config.committee().expect("valid old committee");
        let context = config.context().expect("valid old context");
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 7,
            height: 42,
            parent: [0x22; 32],
            payload: Vec::new(),
            proposer: config.node_id,
            commitment_root: [0x33; 32],
            app_hash: [0x44; 32],
            timestamp: 123,
            justify: None,
        };
        let vote = Vote::new_bls(
            context,
            block.view,
            block.hash(),
            block.app_hash,
            config.node_id,
            &secret,
        );
        let old_qc = form_certificate(&committee, context, vec![vote], true)
            .expect("single-node old committee forms a QC");
        let update = ValidatorSetUpdate {
            node_ids: vec![config.node_id],
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            stakes: vec![(config.node_id, 2)],
        };
        let proof = EpochTransitionProof::from_validator_set_update(
            context,
            old_qc,
            &block,
            block.app_hash,
            &update,
        )
        .expect("candidate proof should construct");
        (proof, committee, block)
    }

    #[test]
    fn finalized_update_produces_a_canonical_next_committee() {
        let (proof, old_committee, block) = transition_fixture();
        let next = proof
            .validate(&old_committee, &block, true)
            .expect("valid transition should verify");
        assert_eq!(next.members().len(), 1);
        assert_eq!(next.total_voting_power(), 2);
        assert_eq!(proof.effective_height, block.height + 2);
        assert_eq!(proof.candidate_context().epoch, 1);
    }

    #[test]
    fn transition_proof_round_trip_is_canonical() {
        let (proof, _, _) = transition_fixture();
        let bytes = proof.canonical_bytes().expect("proof encodes");
        let decoded = EpochTransitionProof::from_canonical_bytes(&bytes).expect("proof decodes");
        assert_eq!(decoded, proof);
    }

    #[test]
    fn transition_rejects_root_height_and_effective_height_tampering() {
        let (proof, committee, block) = transition_fixture();

        let mut wrong_root = proof.clone();
        wrong_root.state_root.root[0] ^= 1;
        assert!(wrong_root.validate(&committee, &block, true).is_err());

        let mut wrong_height = proof.clone();
        wrong_height.state_root.height += 1;
        assert!(wrong_height.validate(&committee, &block, true).is_err());

        let mut skipped = proof;
        skipped.effective_height += 1;
        assert!(skipped.validate(&committee, &block, true).is_err());
    }

    #[test]
    fn transition_rejects_wrong_qc_context_or_subject() {
        let (proof, committee, block) = transition_fixture();

        let mut wrong_qc = proof.clone();
        wrong_qc.old_qc.block_hash[0] ^= 1;
        assert!(wrong_qc.validate(&committee, &block, true).is_err());

        let mut wrong_context = proof;
        wrong_context.old_qc.epoch += 1;
        assert!(wrong_context.validate(&committee, &block, true).is_err());
    }

    #[test]
    fn transition_rejects_an_alternate_valid_bls_committee_when_staging_binding_is_used() {
        let (proof, committee, block) = transition_fixture();
        let expected_update = ValidatorSetUpdate {
            node_ids: proof
                .next_committee
                .iter()
                .map(|member| member.node_id)
                .collect(),
            bls_pubkeys: proof
                .next_committee
                .iter()
                .map(|member| member.bls_pubkey.clone().expect("fixture BLS key"))
                .collect(),
            stakes: proof
                .next_committee
                .iter()
                .map(|member| (member.node_id, member.voting_power as u64))
                .collect(),
        };

        let mut alternate = proof;
        let alternate_secret = BlsSecretKey::from_seed(&[0x77; 32]);
        alternate.next_committee[0].bls_pubkey =
            Some(alternate_secret.public_key().to_bytes().to_vec());
        alternate.next_committee_hash = Committee::from_members(alternate.next_committee.clone())
            .expect("alternate BLS key forms a valid committee")
            .hash();

        // The alternate key is structurally valid, but it is not the key
        // authenticated by the finalized staking state.
        assert!(alternate.validate(&committee, &block, true).is_ok());
        assert!(alternate
            .validate_against_validator_set_update(&committee, &block, true, &expected_update)
            .is_err());
    }

    #[test]
    fn update_parallel_arrays_and_key_material_are_checked() {
        let (config, secret) = config();
        let update = ValidatorSetUpdate {
            node_ids: vec![config.node_id],
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            stakes: vec![(config.node_id, 0)],
        };
        assert!(committee_members_from_update(&update).is_err());

        let mut mismatched = update;
        mismatched.stakes = vec![([0x99; 32], 1)];
        assert!(committee_members_from_update(&mismatched).is_err());
    }
}
