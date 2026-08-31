//! Committee-bound vote and quorum-certificate verification.
//!
//! This module is deliberately used by the networked runner instead of the
//! legacy certificate helpers.  A certificate is only meaningful relative to
//! the active committee: the signer identity, configured BLS key, voting
//! power, view, block hash, and application hash all have to agree.

use std::collections::HashSet;

use super::equivocation::EquivocationProof;
use crate::crypto::bls::{
    aggregate_signatures, verify_aggregate_same_message, BlsPublicKey, BlsSignature,
};
use crate::types::{Certificate, Committee, ConsensusContext, Hash, NodeId, View, Vote};

/// Verify one vote against the active committee and expected proposal data.
pub fn verify_vote(
    committee: &Committee,
    vote: &Vote,
    expected_context: ConsensusContext,
    expected_view: View,
    expected_block_hash: &Hash,
    expected_app_hash: &Hash,
    require_bls_signature: bool,
) -> Result<u128, String> {
    committee.validate_context(expected_context)?;
    vote.validate_context(expected_context)?;
    if vote.view != expected_view {
        return Err(format!(
            "vote view {} does not match expected view {}",
            vote.view, expected_view
        ));
    }
    if vote.block_hash != *expected_block_hash {
        return Err("vote block hash does not match expected block".to_string());
    }
    if vote.app_hash != *expected_app_hash {
        return Err("vote application hash does not match expected block".to_string());
    }

    let member = committee.member(&vote.voter).ok_or_else(|| {
        format!(
            "vote from unknown committee member {}",
            hex::encode(vote.voter)
        )
    })?;

    let configured_key = member.bls_pubkey.as_deref();
    if require_bls_signature && configured_key.is_none() {
        return Err("committee member has no configured BLS public key".to_string());
    }

    if let Some(configured_key) = configured_key {
        let supplied_key = vote
            .bls_pubkey
            .as_deref()
            .ok_or_else(|| "vote is missing its configured BLS public key".to_string())?;
        if supplied_key != configured_key {
            return Err("vote-supplied BLS public key does not match committee".to_string());
        }

        if vote.signature.len() != 96 {
            return Err("vote is missing a 96-byte BLS signature".to_string());
        }
        let key = parse_public_key(configured_key)?;
        let signature = BlsSignature::from_slice(&vote.signature)
            .map_err(|_| "invalid BLS vote signature encoding".to_string())?;
        if !key.verify(&vote.signing_data_common(), &signature) {
            return Err("BLS vote signature verification failed".to_string());
        }
    } else if require_bls_signature {
        return Err("BLS signature required but committee has no key".to_string());
    } else if vote.signature.is_empty() || vote.signature.iter().all(|byte| *byte == 0) {
        // Legacy signatures have no verifier in this crate, but an empty or
        // placeholder signature must never count as a network vote.
        return Err("vote has no network signature".to_string());
    }

    Ok(member.voting_power)
}

/// Verify an equivocation proof against the authoritative committee and
/// consensus context.
///
/// The proof only carries the two signed vote payloads, so verification
/// reconstructs both votes with the committee's configured BLS key rather
/// than trusting a key supplied by the reporter.  The reporter/origin is
/// intentionally not part of this function: a peer may relay evidence it did
/// not discover locally.
pub fn verify_equivocation_proof(
    committee: &Committee,
    proof: &EquivocationProof,
    expected_context: ConsensusContext,
    require_bls_signature: bool,
) -> Result<(), String> {
    committee.validate_context(expected_context)?;
    if proof.context != expected_context {
        return Err(
            "equivocation proof context does not match expected consensus context".to_string(),
        );
    }
    proof.validate_canonical()?;

    let member = committee
        .member(&proof.offender)
        .ok_or_else(|| "equivocation proof offender is not in the committee".to_string())?;
    let bls_pubkey = member.bls_pubkey.clone();

    let vote = |block_hash: Hash, app_hash: Hash, signature: &[u8]| Vote {
        epoch: proof.context.epoch,
        committee_hash: proof.context.committee_hash,
        genesis_hash: proof.context.genesis_hash,
        view: proof.view,
        block_hash,
        app_hash,
        voter: proof.offender,
        signature: signature.to_vec(),
        bls_pubkey: bls_pubkey.clone(),
    };
    let vote_a = vote(proof.hash_a, proof.app_hash_a, &proof.signature_a);
    let vote_b = vote(proof.hash_b, proof.app_hash_b, &proof.signature_b);

    verify_vote(
        committee,
        &vote_a,
        expected_context,
        proof.view,
        &proof.hash_a,
        &proof.app_hash_a,
        require_bls_signature,
    )?;
    verify_vote(
        committee,
        &vote_b,
        expected_context,
        proof.view,
        &proof.hash_b,
        &proof.app_hash_b,
        require_bls_signature,
    )?;
    Ok(())
}

/// Verify a QC against the active committee and expected data.
pub fn verify_certificate(
    committee: &Committee,
    certificate: &Certificate,
    expected_context: ConsensusContext,
    expected_view: View,
    expected_block_hash: &Hash,
    expected_app_hash: Option<&Hash>,
    require_bls_signature: bool,
) -> Result<(), String> {
    committee.validate_context(expected_context)?;
    certificate.validate_context(expected_context)?;
    if certificate.view != expected_view {
        return Err(format!(
            "certificate view {} does not match expected view {}",
            certificate.view, expected_view
        ));
    }
    if certificate.block_hash != *expected_block_hash {
        return Err("certificate block hash does not match expected block".to_string());
    }

    let is_bls = certificate.is_bls();
    if require_bls_signature || is_bls {
        if !is_bls {
            return Err("BLS certificate required".to_string());
        }
        verify_bls_certificate(committee, certificate, expected_app_hash)?;
    } else {
        verify_legacy_certificate(committee, certificate, expected_context, expected_app_hash)?;
    }

    let signers = if !certificate.voters.is_empty() {
        certificate.voters.clone()
    } else {
        certificate.votes.iter().map(|vote| vote.voter).collect()
    };
    match committee.has_weighted_quorum(signers)? {
        true => Ok(()),
        false => Err("certificate does not meet strict weighted quorum".to_string()),
    }
}

/// Form a certificate from votes already collected by the runner.
///
/// BLS mode never falls back to `Certificate::new`: if aggregation or any
/// signature check fails, no certificate is produced.
pub fn form_certificate(
    committee: &Committee,
    context: ConsensusContext,
    mut votes: Vec<Vote>,
    require_bls_signature: bool,
) -> Result<Certificate, String> {
    committee.validate_context(context)?;
    if votes.is_empty() {
        return Err("cannot form a certificate without votes".to_string());
    }
    votes.sort_by_key(|vote| vote.voter);

    let view = votes[0].view;
    let block_hash = votes[0].block_hash;
    let app_hash = votes[0].app_hash;
    for vote in &votes {
        verify_vote(
            committee,
            vote,
            context,
            view,
            &block_hash,
            &app_hash,
            require_bls_signature,
        )?;
    }

    let signers: Vec<NodeId> = votes.iter().map(|vote| vote.voter).collect();
    if !committee.has_weighted_quorum(signers)? {
        return Err("votes do not meet strict weighted quorum".to_string());
    }

    if require_bls_signature {
        let signatures: Result<Vec<_>, _> = votes
            .iter()
            .map(|vote| BlsSignature::from_slice(&vote.signature))
            .collect();
        let aggregate = aggregate_signatures(
            &signatures.map_err(|_| "invalid BLS signature for aggregation".to_string())?,
        )
        .map_err(|_| "BLS signature aggregation failed".to_string())?;
        Ok(Certificate::new_bls(
            context,
            view,
            block_hash,
            votes,
            aggregate.to_bytes().to_vec(),
        )?)
    } else {
        Certificate::new(context, view, block_hash, votes)
    }
}

fn parse_public_key(bytes: &[u8]) -> Result<BlsPublicKey, String> {
    if bytes.len() != 48 {
        return Err("configured BLS public key must be 48 bytes".to_string());
    }
    let mut array = [0u8; 48];
    array.copy_from_slice(bytes);
    BlsPublicKey::from_bytes(&array).map_err(|_| "invalid configured BLS public key".to_string())
}

fn verify_bls_certificate(
    committee: &Committee,
    certificate: &Certificate,
    expected_app_hash: Option<&Hash>,
) -> Result<(), String> {
    let app_hash = certificate
        .app_hash
        .ok_or_else(|| "BLS certificate is missing app hash".to_string())?;
    if let Some(expected) = expected_app_hash {
        if app_hash != *expected {
            return Err("certificate application hash does not match expected block".to_string());
        }
    }
    if certificate.voters.is_empty() {
        return Err("BLS certificate has no voters".to_string());
    }
    if !certificate.votes.is_empty() {
        return Err("BLS certificate must not carry individual votes".to_string());
    }
    if certificate.voters.len() != certificate.bls_pubkeys.len() {
        return Err("certificate voter/public-key count mismatch".to_string());
    }

    let mut unique = HashSet::new();
    let mut public_keys = Vec::with_capacity(certificate.voters.len());
    for (index, voter) in certificate.voters.iter().enumerate() {
        if !unique.insert(*voter) {
            return Err("certificate contains duplicate voter".to_string());
        }
        let configured = committee
            .bls_pubkey(voter)
            .ok_or_else(|| format!("certificate contains unknown voter {}", hex::encode(voter)))?;
        if certificate.bls_pubkeys[index].as_slice() != configured {
            return Err("certificate public key does not match committee".to_string());
        }
        public_keys.push(parse_public_key(configured)?);
    }

    let aggregate_signature = BlsSignature::from_slice(&certificate.agg_signature)
        .map_err(|_| "invalid aggregate BLS signature encoding".to_string())?;
    let message = Certificate::build_signing_message(
        certificate.context(),
        certificate.view,
        &certificate.block_hash,
        &app_hash,
    );
    if !verify_aggregate_same_message(&message, &aggregate_signature, &public_keys) {
        return Err("aggregate BLS signature verification failed".to_string());
    }
    Ok(())
}

fn verify_legacy_certificate(
    committee: &Committee,
    certificate: &Certificate,
    expected_context: ConsensusContext,
    expected_app_hash: Option<&Hash>,
) -> Result<(), String> {
    if certificate.votes.is_empty() {
        return Err("legacy certificate has no individual votes".to_string());
    }
    let app_hash = certificate
        .app_hash
        .or_else(|| certificate.votes.first().map(|vote| vote.app_hash));
    if let Some(expected) = expected_app_hash {
        if app_hash != Some(*expected) {
            return Err("certificate application hash does not match expected block".to_string());
        }
    }

    let mut unique = HashSet::new();
    for vote in &certificate.votes {
        verify_vote(
            committee,
            vote,
            expected_context,
            certificate.view,
            &certificate.block_hash,
            &app_hash.ok_or_else(|| "legacy certificate is missing app hash".to_string())?,
            false,
        )?;
        if !unique.insert(vote.voter) {
            return Err("legacy certificate contains duplicate voter".to_string());
        }
    }
    if !certificate.voters.is_empty() {
        let mut listed = certificate.voters.clone();
        listed.sort_unstable();
        let mut actual: Vec<_> = unique.iter().copied().collect();
        actual.sort_unstable();
        if listed != actual {
            return Err("certificate voters do not match individual votes".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::bls::BlsSecretKey;
    use crate::types::ConsensusConfig;

    fn fixture_with_powers(
        powers: &[u64],
    ) -> (Committee, Vec<BlsSecretKey>, Vec<NodeId>, Hash, Hash) {
        let voters: Vec<NodeId> = (1..=powers.len()).map(|id| [id as u8; 32]).collect();
        let secrets: Vec<_> = (1..=powers.len())
            .map(|id| {
                let mut seed = [0u8; 32];
                seed[0] = id as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: voters[0],
            validators: voters.clone(),
            voting_powers: powers.to_vec(),
            view_timeout_ms: 1000,
            bls_pubkeys: secrets
                .iter()
                .map(|secret| secret.public_key().to_bytes().to_vec())
                .collect(),
            bls_secret_key: Some(secrets[0].to_bytes()),
        };
        (
            config.committee().unwrap(),
            secrets,
            voters,
            [7u8; 32],
            [8u8; 32],
        )
    }

    fn fixture() -> (Committee, Vec<BlsSecretKey>, Vec<NodeId>, Hash, Hash) {
        fixture_with_powers(&[1, 1, 1, 1])
    }

    fn votes(
        context: ConsensusContext,
        secrets: &[BlsSecretKey],
        voters: &[NodeId],
        count: usize,
        block_hash: Hash,
        app_hash: Hash,
    ) -> Vec<Vote> {
        (0..count)
            .map(|index| {
                Vote::new_bls(
                    context,
                    3,
                    block_hash,
                    app_hash,
                    voters[index],
                    &secrets[index],
                )
            })
            .collect()
    }

    #[test]
    fn weighted_quorum_accepts_more_than_two_thirds() {
        let (committee, secrets, voters, block_hash, app_hash) = fixture();
        let context = committee.initial_context();
        let cert = form_certificate(
            &committee,
            context,
            votes(context, &secrets, &voters, 3, block_hash, app_hash),
            true,
        )
        .unwrap();
        assert!(verify_certificate(
            &committee,
            &cert,
            context,
            3,
            &block_hash,
            Some(&app_hash),
            true,
        )
        .is_ok());
    }

    #[test]
    fn exactly_two_thirds_is_rejected() {
        let (committee, secrets, voters, block_hash, app_hash) = fixture_with_powers(&[1, 1, 1]);
        let context = committee.initial_context();
        let cert = Certificate::new_bls(
            context,
            3,
            block_hash,
            votes(context, &secrets, &voters, 2, block_hash, app_hash),
            aggregate_signatures(
                &votes(context, &secrets, &voters, 2, block_hash, app_hash)
                    .iter()
                    .map(|vote| BlsSignature::from_slice(&vote.signature).unwrap())
                    .collect::<Vec<_>>(),
            )
            .unwrap()
            .to_bytes()
            .to_vec(),
        )
        .unwrap();
        assert!(verify_certificate(
            &committee,
            &cert,
            context,
            3,
            &block_hash,
            Some(&app_hash),
            true,
        )
        .is_err());
    }

    #[test]
    fn unequal_power_boundary_is_enforced() {
        // The high-power signer alone is exactly 2/3 (4/6), so strict quorum
        // must reject it.  Adding either one-power validator makes it 5/6.
        let (committee, secrets, voters, block_hash, app_hash) = fixture_with_powers(&[4, 1, 1]);
        let context = committee.initial_context();
        let high_only = votes(context, &secrets, &voters, 1, block_hash, app_hash);
        assert!(form_certificate(&committee, context, high_only, true).is_err());

        let high_plus_low = vec![
            Vote::new_bls(context, 3, block_hash, app_hash, voters[0], &secrets[0]),
            Vote::new_bls(context, 3, block_hash, app_hash, voters[1], &secrets[1]),
        ];
        assert!(form_certificate(&committee, context, high_plus_low, true).is_ok());
    }

    #[test]
    fn duplicate_unknown_and_key_mismatch_are_rejected() {
        let (committee, secrets, voters, block_hash, app_hash) = fixture();
        let context = committee.initial_context();
        let valid = Vote::new_bls(context, 3, block_hash, app_hash, voters[0], &secrets[0]);
        assert!(verify_vote(&committee, &valid, context, 3, &block_hash, &app_hash, true).is_ok());

        let mut unknown = valid.clone();
        unknown.voter = [99u8; 32];
        assert!(verify_vote(
            &committee,
            &unknown,
            context,
            3,
            &block_hash,
            &app_hash,
            true
        )
        .is_err());

        let mut wrong_key = valid.clone();
        wrong_key.bls_pubkey = Some(secrets[1].public_key().to_bytes().to_vec());
        assert!(verify_vote(
            &committee,
            &wrong_key,
            context,
            3,
            &block_hash,
            &app_hash,
            true
        )
        .is_err());

        let duplicate = Certificate {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 3,
            block_hash,
            app_hash: Some(app_hash),
            votes: vec![],
            voters: vec![voters[0], voters[0]],
            bls_pubkeys: vec![
                secrets[0].public_key().to_bytes().to_vec(),
                secrets[0].public_key().to_bytes().to_vec(),
            ],
            agg_signature: vec![0u8; 96],
        };
        assert!(verify_certificate(
            &committee,
            &duplicate,
            context,
            3,
            &block_hash,
            Some(&app_hash),
            true
        )
        .is_err());
    }

    #[test]
    fn wrong_vote_bindings_are_rejected() {
        let (committee, secrets, voters, block_hash, app_hash) = fixture();
        let context = committee.initial_context();
        let vote = Vote::new_bls(context, 3, block_hash, app_hash, voters[0], &secrets[0]);
        assert!(verify_vote(&committee, &vote, context, 4, &block_hash, &app_hash, true).is_err());
        assert!(verify_vote(&committee, &vote, context, 3, &[9u8; 32], &app_hash, true).is_err());
        assert!(verify_vote(&committee, &vote, context, 3, &block_hash, &[9u8; 32], true).is_err());

        let wrong_context = ConsensusContext::new(0, [99u8; 32]);
        assert!(verify_vote(
            &committee,
            &vote,
            wrong_context,
            3,
            &block_hash,
            &app_hash,
            true
        )
        .is_err());
    }

    #[test]
    fn equivocation_proof_reconstructs_and_verifies_both_votes() {
        let (committee, secrets, voters, _, _) = fixture();
        let context = committee.context_with_genesis(0, [9u8; 32]);
        let vote_a = Vote::new_bls(context, 3, [1u8; 32], [11u8; 32], voters[0], &secrets[0]);
        let vote_b = Vote::new_bls(context, 3, [2u8; 32], [12u8; 32], voters[0], &secrets[0]);
        let proof = EquivocationProof {
            context,
            offender: voters[0],
            view: 3,
            hash_a: vote_a.block_hash,
            app_hash_a: vote_a.app_hash,
            hash_b: vote_b.block_hash,
            app_hash_b: vote_b.app_hash,
            signature_a: vote_a.signature,
            signature_b: vote_b.signature,
        };
        assert!(verify_equivocation_proof(&committee, &proof, context, true).is_ok());

        let mut forged = proof.clone();
        forged.signature_b[0] ^= 1;
        assert!(verify_equivocation_proof(&committee, &forged, context, true).is_err());

        let mut wrong_context = proof.clone();
        wrong_context.context.genesis_hash = [8u8; 32];
        assert!(verify_equivocation_proof(&committee, &wrong_context, context, true).is_err());

        let mut noncanonical = proof;
        std::mem::swap(&mut noncanonical.hash_a, &mut noncanonical.hash_b);
        std::mem::swap(&mut noncanonical.app_hash_a, &mut noncanonical.app_hash_b);
        std::mem::swap(&mut noncanonical.signature_a, &mut noncanonical.signature_b);
        assert!(verify_equivocation_proof(&committee, &noncanonical, context, true).is_err());
    }
}
