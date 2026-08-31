//! Consensus Configuration
//!
//! Configuration for the HotStuff-2 consensus protocol.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{
    hash, Hash, NodeId, View, BLOCK_HASH_VERSION, COMMITMENT_SCHEMA_VERSION, COMMITMENT_VERSION,
    CONSENSUS_STATE_ROOT_SCHEMA_VERSION,
};
use crate::crypto::bls::BlsSecretKey;

fn canonical_validator_order(validators: &[NodeId]) -> Vec<NodeId> {
    let mut ordered = validators.to_vec();
    ordered.sort_unstable();
    ordered
}

fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

/// Canonical stake-weighted schedule shared by `Committee` and
/// `ConsensusConfig`.  Keeping the hash encoding and slot arithmetic in one
/// helper prevents transport admission and the consensus runner from
/// disagreeing about the scheduled proposer.
fn weighted_leader(view: View, stakes: &[(NodeId, u128)], fallback: NodeId) -> NodeId {
    if stakes.is_empty() {
        return fallback;
    }

    let mut ordered_stakes = stakes.to_vec();
    ordered_stakes.sort_by_key(|(node_id, _)| *node_id);

    let common_divisor = ordered_stakes
        .iter()
        .map(|(_, stake)| *stake)
        .filter(|stake| *stake > 0)
        .fold(0u128, gcd_u128);
    if common_divisor == 0 {
        let idx = (view as usize) % ordered_stakes.len();
        return ordered_stakes[idx].0;
    }

    let normalized_stakes: Vec<(NodeId, u128)> = ordered_stakes
        .iter()
        .map(|(node_id, stake)| (*node_id, *stake / common_divisor))
        .collect();
    let total_stake = match normalized_stakes
        .iter()
        .try_fold(0u128, |total, (_, stake)| total.checked_add(*stake))
    {
        Some(total) if total > 0 => total,
        _ => {
            let idx = (view as usize) % ordered_stakes.len();
            return ordered_stakes[idx].0;
        }
    };

    let mut encoded = Vec::with_capacity(32 + 8 + normalized_stakes.len() * 48);
    encoded.extend_from_slice(b"HYPERLICKED_LEADER_V1");
    encoded.extend_from_slice(&view.to_le_bytes());
    encoded.extend_from_slice(&(normalized_stakes.len() as u64).to_le_bytes());
    for (node_id, stake) in &normalized_stakes {
        encoded.extend_from_slice(node_id);
        encoded.extend_from_slice(&stake.to_le_bytes());
    }
    let digest = hash(&encoded);
    let mut slot_bytes = [0u8; 16];
    slot_bytes.copy_from_slice(&digest[..16]);
    let slot = u128::from_le_bytes(slot_bytes) % total_stake;

    let mut cumulative = 0u128;
    for (node_id, stake) in &normalized_stakes {
        cumulative += *stake;
        if slot < cumulative {
            return *node_id;
        }
    }

    ordered_stakes
        .last()
        .map(|(node_id, _)| *node_id)
        .unwrap_or(fallback)
}

/// Domain separator for the epoch-0 genesis consensus context.
///
/// File-backed genesis uses V4.  V3 authenticated only consensus metadata (or
/// metadata plus the legacy allocation extension); V4 additionally carries a
/// canonical application-genesis commitment.  PoP bytes are deliberately not
/// part of either preimage because the PoP is itself signed over the resulting
/// domain.
pub const GENESIS_DOMAIN_TAG: &[u8] = b"HYPERLICKED_GENESIS_DOMAIN_V4";

/// Domain separator for the canonical application-genesis commitment.
pub const APPLICATION_GENESIS_COMMITMENT_TAG: &[u8] = b"HYPERLICKED_APPLICATION_GENESIS_V1";

/// Fixed native HYCK economic policy authenticated by file-backed genesis.
///
/// These values intentionally live beside the consensus-domain encoding.  A
/// node loading the same committee under a binary with different economic or
/// reward semantics must not authenticate the resulting application state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenesisApplicationPolicy {
    /// Number of decimal places in one whole HYCK.
    pub hyck_decimals: u8,
    /// Number of base units in one whole HYCK.
    pub hyck_base_units_per_hyck: i64,
    /// Fixed native maximum supply in base units.
    pub hyck_max_supply_base_units: i64,
    /// Native HYCK held back for future emissions, in base units.
    pub hyck_emissions_reserve_base_units: i64,
    /// Minimum self-stake accepted by the static staking model, in base units.
    pub min_self_stake_base_units: i64,
    /// Maximum validator commission in basis points.
    pub max_commission_bps: i64,
    /// Version of the authenticated reward policy.
    pub reward_policy_version: u16,
    /// Version of the integer reward distribution formula.
    pub reward_formula_version: u16,
    /// Inflation/mint rate in basis points.  Zero means no inflationary mint.
    pub reward_inflation_bps: i64,
    /// Denominator used by commission arithmetic (10,000 = basis points).
    pub reward_commission_denominator_bps: u64,
    /// Annual staking APY at the reference stake, in basis points.
    pub reward_apy_bps: u64,
    /// Reference aggregate stake for the reward curve, in base units.
    pub reward_anchor_stake_base_units: i64,
    /// Reward accrual cadence, in milliseconds.
    pub reward_accrual_interval_ms: u64,
    /// Deterministic reward year, in milliseconds.
    pub reward_year_ms: u64,
    /// Auto-compound cadence, in milliseconds.
    pub reward_auto_compound_interval_ms: u64,
    /// Rounding mode used by integer reward division (0 = floor).
    pub reward_rounding_mode: u8,
    /// Reward source (1 = funded pool only; no balance-free mint).
    pub reward_source: u8,
}

/// Six-decimal, fixed-supply HYCK policy selected for the local/mainnet
/// genesis tranche.
pub const GENESIS_APPLICATION_POLICY: GenesisApplicationPolicy = GenesisApplicationPolicy {
    hyck_decimals: 6,
    hyck_base_units_per_hyck: crate::app::staking::types::HYCK_BASE_UNITS_PER_HYCK,
    hyck_max_supply_base_units: crate::app::staking::types::HYCK_TOTAL_SUPPLY,
    hyck_emissions_reserve_base_units: crate::app::staking::types::HYCK_GENESIS_EMISSIONS_RESERVE,
    min_self_stake_base_units: crate::app::staking::types::MIN_SELF_STAKE,
    max_commission_bps: crate::app::staking::types::MAX_COMMISSION_BPS,
    reward_policy_version: 1,
    reward_formula_version: 1,
    reward_inflation_bps: 0,
    reward_commission_denominator_bps: crate::app::staking::types::STAKING_REWARD_BPS_DENOMINATOR,
    reward_apy_bps: crate::app::staking::types::STAKING_REWARD_APY_BPS,
    reward_anchor_stake_base_units: crate::app::staking::types::STAKING_REWARD_ANCHOR_STAKE,
    reward_accrual_interval_ms: crate::app::staking::types::STAKING_REWARD_EPOCH_MS,
    reward_year_ms: crate::app::staking::types::STAKING_REWARD_YEAR_MS,
    reward_auto_compound_interval_ms: crate::app::staking::types::STAKING_AUTO_COMPOUND_INTERVAL_MS,
    reward_rounding_mode: 0,
    reward_source: 1,
};

/// Public aliases used by configuration validation and tooling.  Keeping the
/// whole-HYCK and base-unit forms explicit avoids an accidental unit mismatch.
pub const HYCK_DECIMALS: u8 = GENESIS_APPLICATION_POLICY.hyck_decimals;
pub const HYCK_MAX_SUPPLY_HYCK: i64 = GENESIS_APPLICATION_POLICY.hyck_max_supply_base_units
    / GENESIS_APPLICATION_POLICY.hyck_base_units_per_hyck;
pub const HYCK_MAX_SUPPLY_BASE_UNITS: i64 = GENESIS_APPLICATION_POLICY.hyck_max_supply_base_units;
pub const HYCK_EMISSIONS_RESERVE_HYCK: i64 = GENESIS_APPLICATION_POLICY
    .hyck_emissions_reserve_base_units
    / GENESIS_APPLICATION_POLICY.hyck_base_units_per_hyck;
pub const HYCK_EMISSIONS_RESERVE_BASE_UNITS: i64 =
    GENESIS_APPLICATION_POLICY.hyck_emissions_reserve_base_units;
pub const HYCK_GENESIS_ALLOCATABLE_SUPPLY_BASE_UNITS: i64 =
    HYCK_MAX_SUPPLY_BASE_UNITS - HYCK_EMISSIONS_RESERVE_BASE_UNITS;
pub const GENESIS_REWARD_POLICY_VERSION: u16 = GENESIS_APPLICATION_POLICY.reward_policy_version;
pub const GENESIS_REWARD_FORMULA_VERSION: u16 = GENESIS_APPLICATION_POLICY.reward_formula_version;

/// Canonical application-side validator material committed by file-backed
/// genesis.  BLS public keys are already committed by `committee_hash`; PoP
/// bytes are excluded to avoid the domain/PoP circularity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenesisApplicationValidator {
    pub node_id: NodeId,
    pub operator: String,
    pub voting_power: u128,
    pub self_stake: i64,
    pub commission_bps: i64,
}

fn append_length_prefixed_bytes(encoded: &mut Vec<u8>, value: &[u8]) {
    encoded.extend_from_slice(&(value.len() as u64).to_le_bytes());
    encoded.extend_from_slice(value);
}

fn append_length_prefixed_string(encoded: &mut Vec<u8>, value: &str) {
    append_length_prefixed_bytes(encoded, value.as_bytes());
}

fn append_genesis_policy(encoded: &mut Vec<u8>, policy: GenesisApplicationPolicy) {
    encoded.push(policy.hyck_decimals);
    encoded.extend_from_slice(&policy.hyck_base_units_per_hyck.to_le_bytes());
    encoded.extend_from_slice(&policy.hyck_max_supply_base_units.to_le_bytes());
    encoded.extend_from_slice(&policy.hyck_emissions_reserve_base_units.to_le_bytes());
    encoded.extend_from_slice(&policy.min_self_stake_base_units.to_le_bytes());
    encoded.extend_from_slice(&policy.max_commission_bps.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_policy_version.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_formula_version.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_inflation_bps.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_commission_denominator_bps.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_apy_bps.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_anchor_stake_base_units.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_accrual_interval_ms.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_year_ms.to_le_bytes());
    encoded.extend_from_slice(&policy.reward_auto_compound_interval_ms.to_le_bytes());
    encoded.push(policy.reward_rounding_mode);
    encoded.push(policy.reward_source);
}

/// Compute the canonical application-genesis commitment for a validated
/// file-backed genesis.  Validator and allocation order is not semantic and
/// is therefore sorted before encoding.  Empty allocations still emit the
/// same tagged schema and a zero count; they do not use a separate preimage.
pub fn application_genesis_commitment_with_policy(
    chain_id: &str,
    epoch: u64,
    view_timeout_ms: u64,
    committee_hash: Hash,
    validators: &[GenesisApplicationValidator],
    allocations: &[(String, i64)],
    policy: GenesisApplicationPolicy,
) -> Hash {
    let mut canonical_validators = validators.to_vec();
    canonical_validators.sort_by_key(|validator| validator.node_id);
    let mut canonical_allocations = allocations.to_vec();
    canonical_allocations.sort_by(|left, right| left.0.cmp(&right.0));

    let mut encoded = Vec::new();
    encoded.extend_from_slice(APPLICATION_GENESIS_COMMITMENT_TAG);
    append_genesis_policy(&mut encoded, policy);
    append_length_prefixed_string(&mut encoded, chain_id);
    encoded.extend_from_slice(&epoch.to_le_bytes());
    encoded.extend_from_slice(&view_timeout_ms.to_le_bytes());
    encoded.extend_from_slice(&committee_hash);

    encoded.extend_from_slice(&(canonical_validators.len() as u64).to_le_bytes());
    for validator in canonical_validators {
        encoded.extend_from_slice(&validator.node_id);
        encoded.extend_from_slice(&validator.voting_power.to_le_bytes());
        append_length_prefixed_string(&mut encoded, &validator.operator);
        encoded.extend_from_slice(&validator.self_stake.to_le_bytes());
        encoded.extend_from_slice(&validator.commission_bps.to_le_bytes());
    }

    encoded.extend_from_slice(&(canonical_allocations.len() as u64).to_le_bytes());
    for (address, amount) in canonical_allocations {
        append_length_prefixed_string(&mut encoded, &address);
        encoded.extend_from_slice(&amount.to_le_bytes());
    }
    hash(&encoded)
}

/// Compute the application-genesis commitment using the fixed protocol
/// economic/reward policy.
pub fn application_genesis_commitment(
    chain_id: &str,
    epoch: u64,
    view_timeout_ms: u64,
    committee_hash: Hash,
    validators: &[GenesisApplicationValidator],
    allocations: &[(String, i64)],
) -> Hash {
    application_genesis_commitment_with_policy(
        chain_id,
        epoch,
        view_timeout_ms,
        committee_hash,
        validators,
        allocations,
        GENESIS_APPLICATION_POLICY,
    )
}

/// Compute the consensus-only domain used by in-memory/legacy callers.
///
/// File-backed genesis must use [`genesis_domain_hash_with_application`].  The
/// committee hash commits to canonical validator IDs, voting powers, and BLS
/// public keys; this helper intentionally does not claim application-state
/// authentication.
pub fn genesis_domain_hash(
    chain_id: &str,
    epoch: u64,
    view_timeout_ms: u64,
    committee_hash: Hash,
) -> Hash {
    let mut encoded = Vec::with_capacity(
        GENESIS_DOMAIN_TAG.len()
            + 2
            + 2
            + 2
            + 2
            + 8
            + chain_id.len()
            + 8
            + 8
            + committee_hash.len(),
    );
    encoded.extend_from_slice(GENESIS_DOMAIN_TAG);
    encoded.extend_from_slice(&BLOCK_HASH_VERSION.to_le_bytes());
    encoded.extend_from_slice(&CONSENSUS_STATE_ROOT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_VERSION.to_le_bytes());
    encoded.extend_from_slice(&(chain_id.len() as u64).to_le_bytes());
    encoded.extend_from_slice(chain_id.as_bytes());
    encoded.extend_from_slice(&epoch.to_le_bytes());
    encoded.extend_from_slice(&view_timeout_ms.to_le_bytes());
    encoded.extend_from_slice(&committee_hash);
    hash(&encoded)
}

/// Compute the legacy allocation-only extension used by in-memory callers.
/// File-backed genesis must use [`genesis_domain_hash_with_application`], which
/// also commits bootstrap and economic policy material.  Allocation order is
/// not part of the JSON semantics; entries are sorted before hashing.
pub fn genesis_domain_hash_with_allocations(
    chain_id: &str,
    epoch: u64,
    view_timeout_ms: u64,
    committee_hash: Hash,
    allocations: &[(String, i64)],
) -> Hash {
    let mut canonical = allocations.to_vec();
    canonical.sort_by(|left, right| left.0.cmp(&right.0));

    let mut encoded = Vec::new();
    encoded.extend_from_slice(GENESIS_DOMAIN_TAG);
    encoded.extend_from_slice(&BLOCK_HASH_VERSION.to_le_bytes());
    encoded.extend_from_slice(&CONSENSUS_STATE_ROOT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_VERSION.to_le_bytes());
    encoded.extend_from_slice(&(chain_id.len() as u64).to_le_bytes());
    encoded.extend_from_slice(chain_id.as_bytes());
    encoded.extend_from_slice(&epoch.to_le_bytes());
    encoded.extend_from_slice(&view_timeout_ms.to_le_bytes());
    encoded.extend_from_slice(&committee_hash);
    encoded.extend_from_slice(b"hyck_allocations\0");
    encoded.extend_from_slice(&(canonical.len() as u64).to_le_bytes());
    for (address, amount) in canonical {
        encoded.extend_from_slice(&(address.len() as u64).to_le_bytes());
        encoded.extend_from_slice(address.as_bytes());
        encoded.extend_from_slice(&amount.to_le_bytes());
    }
    hash(&encoded)
}

/// Compute the file-backed genesis domain with its canonical application
/// commitment.  The outer domain retains the consensus protocol versions and
/// committee hash, while the nested commitment authenticates all deterministic
/// application bootstrap/economic material.
pub fn genesis_domain_hash_with_application(
    chain_id: &str,
    epoch: u64,
    view_timeout_ms: u64,
    committee_hash: Hash,
    validators: &[GenesisApplicationValidator],
    allocations: &[(String, i64)],
) -> Hash {
    let application_commitment = application_genesis_commitment(
        chain_id,
        epoch,
        view_timeout_ms,
        committee_hash,
        validators,
        allocations,
    );
    let mut encoded = Vec::new();
    encoded.extend_from_slice(GENESIS_DOMAIN_TAG);
    encoded.extend_from_slice(&BLOCK_HASH_VERSION.to_le_bytes());
    encoded.extend_from_slice(&CONSENSUS_STATE_ROOT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_SCHEMA_VERSION.to_le_bytes());
    encoded.extend_from_slice(&COMMITMENT_VERSION.to_le_bytes());
    append_length_prefixed_string(&mut encoded, chain_id);
    encoded.extend_from_slice(&epoch.to_le_bytes());
    encoded.extend_from_slice(&view_timeout_ms.to_le_bytes());
    encoded.extend_from_slice(&committee_hash);
    encoded.extend_from_slice(b"application_genesis_commitment\0");
    encoded.extend_from_slice(&application_commitment);
    hash(&encoded)
}

/// Consensus authentication context.
///
/// The committee hash is always derived from the canonical `Committee`; the
/// genesis hash is derived from validated genesis metadata.  Both are carried
/// on every consensus object so signatures and block identities cannot be
/// replayed across validator sets, epochs, or chains with reused keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsensusContext {
    pub epoch: u64,
    pub committee_hash: Hash,
    pub genesis_hash: Hash,
}

impl ConsensusContext {
    /// Construct a context for unit-test/legacy in-memory callers that do not
    /// load a file-backed genesis.  Live node configuration must use
    /// [`Self::with_genesis`] with the hash derived from validated genesis.
    pub const fn new(epoch: u64, committee_hash: Hash) -> Self {
        Self {
            epoch,
            committee_hash,
            genesis_hash: [0u8; 32],
        }
    }

    /// Construct a context with an explicit genesis domain.
    pub const fn with_genesis(epoch: u64, committee_hash: Hash, genesis_hash: Hash) -> Self {
        Self {
            epoch,
            committee_hash,
            genesis_hash,
        }
    }

    /// Build the initial static context for a canonical committee.
    pub fn initial(committee: &Committee) -> Self {
        Self::new(0, committee.hash())
    }

    /// Return whether this context carries the live genesis domain.
    pub fn has_genesis_domain(&self) -> bool {
        self.genesis_hash != [0u8; 32]
    }
}

/// A member of the active validator committee.
///
/// The committee is the consensus authority for a single epoch.  Its order is
/// canonical (ascending `NodeId`) and must not depend on the order in which a
/// configuration was assembled.  BLS keys are kept as bytes here so the
/// committee hash can be computed without relying on a crypto implementation's
/// serialization details; callers validate the key when they verify a vote or
/// certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitteeMember {
    pub node_id: NodeId,
    pub bls_pubkey: Option<Vec<u8>>,
    pub voting_power: u128,
}

/// Maximum number of members accepted by the static consensus committee.
pub const MAX_COMMITTEE_MEMBERS: usize = 21;

/// Canonical active validator committee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committee {
    members: Vec<CommitteeMember>,
    total_voting_power: u128,
    hash: Hash,
}

impl Committee {
    /// Build a committee from already canonicalized member material.
    ///
    /// This constructor is used by authenticated epoch-transition proofs. It
    /// deliberately does not accept an epoch: a committee is the member/key/
    /// power material, while its epoch is bound by [`ConsensusContext`].
    pub fn from_members(mut members: Vec<CommitteeMember>) -> Result<Self, String> {
        if members.is_empty() {
            return Err("committee must contain at least one validator".to_string());
        }
        if members.len() > MAX_COMMITTEE_MEMBERS {
            return Err(format!(
                "committee must contain at most {MAX_COMMITTEE_MEMBERS} validators"
            ));
        }

        for member in &members {
            if member.voting_power == 0 {
                return Err(format!(
                    "validator {} has zero voting power",
                    hex::encode(member.node_id)
                ));
            }
        }

        members.sort_by_key(|member| member.node_id);
        if members
            .windows(2)
            .any(|pair| pair[0].node_id == pair[1].node_id)
        {
            return Err("committee contains duplicate validator IDs".to_string());
        }

        let total_voting_power = members
            .iter()
            .try_fold(0u128, |total, member| {
                total.checked_add(member.voting_power)
            })
            .ok_or_else(|| "committee voting power overflows u128".to_string())?;
        if total_voting_power == 0 {
            return Err("committee voting power must be non-zero".to_string());
        }

        let hash = Self::compute_hash(&members);
        Ok(Self {
            members,
            total_voting_power,
            hash,
        })
    }

    /// Build a committee from the parallel fields in `ConsensusConfig`.
    pub fn from_config(config: &ConsensusConfig) -> Result<Self, String> {
        if config.epoch != 0 {
            return Err(format!(
                "only static epoch 0 is supported, got epoch {}",
                config.epoch
            ));
        }

        if config.validators.is_empty() {
            return Err("committee must contain at least one validator".to_string());
        }
        if config.validators.len() > MAX_COMMITTEE_MEMBERS {
            return Err(format!(
                "committee must contain at most {MAX_COMMITTEE_MEMBERS} validators"
            ));
        }

        if config.voting_powers.len() != config.validators.len() {
            return Err(format!(
                "voting power count {} does not match validator count {}",
                config.voting_powers.len(),
                config.validators.len()
            ));
        }

        if !config.bls_pubkeys.is_empty() && config.bls_pubkeys.len() != config.validators.len() {
            return Err(format!(
                "BLS public key count {} does not match validator count {}",
                config.bls_pubkeys.len(),
                config.validators.len()
            ));
        }

        let mut members = Vec::with_capacity(config.validators.len());
        for (index, node_id) in config.validators.iter().copied().enumerate() {
            let voting_power = u128::from(config.voting_powers[index]);
            if voting_power == 0 {
                return Err(format!(
                    "validator {} has zero voting power",
                    hex::encode(node_id)
                ));
            }

            let bls_pubkey = if config.bls_pubkeys.is_empty() {
                None
            } else {
                Some(config.bls_pubkeys[index].clone())
            };

            members.push(CommitteeMember {
                node_id,
                bls_pubkey,
                voting_power,
            });
        }

        Self::from_members(members)
    }

    fn compute_hash(members: &[CommitteeMember]) -> Hash {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"HYPERLICKED_COMMITTEE_V1");
        encoded.extend_from_slice(&(members.len() as u64).to_le_bytes());
        for member in members {
            encoded.extend_from_slice(&member.node_id);
            encoded.extend_from_slice(&member.voting_power.to_le_bytes());
            match &member.bls_pubkey {
                Some(key) => {
                    encoded.push(1);
                    encoded.extend_from_slice(&(key.len() as u64).to_le_bytes());
                    encoded.extend_from_slice(key);
                }
                None => encoded.push(0),
            }
        }
        hash(&encoded)
    }

    pub fn members(&self) -> &[CommitteeMember] {
        &self.members
    }

    pub fn member(&self, node_id: &NodeId) -> Option<&CommitteeMember> {
        self.members
            .binary_search_by_key(node_id, |member| member.node_id)
            .ok()
            .map(|index| &self.members[index])
    }

    pub fn total_voting_power(&self) -> u128 {
        self.total_voting_power
    }

    pub fn hash(&self) -> Hash {
        self.hash
    }

    /// Build a context using this committee's canonical hash.
    pub const fn context(&self, epoch: u64) -> ConsensusContext {
        ConsensusContext::new(epoch, self.hash)
    }

    /// Build a context using this committee and a validated genesis domain.
    pub const fn context_with_genesis(&self, epoch: u64, genesis_hash: Hash) -> ConsensusContext {
        ConsensusContext::with_genesis(epoch, self.hash, genesis_hash)
    }

    /// Build the initial static context for this committee.
    pub const fn initial_context(&self) -> ConsensusContext {
        self.context(0)
    }

    /// Validate a context against this canonical committee in the static phase.
    pub fn validate_context(&self, context: ConsensusContext) -> Result<(), String> {
        if context.epoch != 0 {
            return Err(format!(
                "only static epoch 0 is supported, got epoch {}",
                context.epoch
            ));
        }
        if context.committee_hash != self.hash {
            return Err("consensus context committee hash does not match committee".to_string());
        }
        Ok(())
    }

    pub fn voting_power(&self, node_id: &NodeId) -> Option<u128> {
        self.member(node_id).map(|member| member.voting_power)
    }

    pub fn bls_pubkey(&self, node_id: &NodeId) -> Option<&[u8]> {
        self.member(node_id)
            .and_then(|member| member.bls_pubkey.as_deref())
    }

    /// Whether this committee has configured BLS keys for every member.
    pub fn bls_enabled(&self) -> bool {
        !self.members.is_empty()
            && self
                .members
                .iter()
                .all(|member| member.bls_pubkey.is_some())
    }

    /// Return the canonical stake-weighted leader for this committee.
    pub fn leader(&self, view: View) -> NodeId {
        let stakes: Vec<_> = self
            .members
            .iter()
            .map(|member| (member.node_id, member.voting_power))
            .collect();
        let fallback = self
            .members
            .first()
            .map(|member| member.node_id)
            .unwrap_or([0u8; 32]);
        weighted_leader(view, &stakes, fallback)
    }

    /// Check weighted quorum using unique, committee members only.
    ///
    /// The strict inequality is intentional: exactly two thirds is not a QC.
    pub fn has_weighted_quorum<I>(&self, signers: I) -> Result<bool, String>
    where
        I: IntoIterator<Item = NodeId>,
    {
        let mut unique = std::collections::HashSet::new();
        let mut signer_power = 0u128;
        for signer in signers {
            if !unique.insert(signer) {
                return Err(format!(
                    "duplicate committee signer {}",
                    hex::encode(signer)
                ));
            }
            let power = self
                .voting_power(&signer)
                .ok_or_else(|| format!("unknown committee signer {}", hex::encode(signer)))?;
            signer_power = signer_power
                .checked_add(power)
                .ok_or_else(|| "signer voting power overflows u128".to_string())?;
        }

        let left = signer_power
            .checked_mul(3)
            .ok_or_else(|| "quorum calculation overflows u128".to_string())?;
        let right = self
            .total_voting_power
            .checked_mul(2)
            .ok_or_else(|| "quorum calculation overflows u128".to_string())?;
        Ok(left > right)
    }
}

/// Consensus configuration
#[derive(Clone)]
pub struct ConsensusConfig {
    /// Active consensus epoch.  Phase A supports only the initial epoch.
    pub epoch: u64,
    /// Cryptographic domain derived from the validated genesis file.
    pub genesis_hash: Hash,
    /// This node's ID
    pub node_id: NodeId,
    /// All validator node IDs (including self)
    pub validators: Vec<NodeId>,
    /// Voting power for each validator (same order as `validators`).
    ///
    /// The committee converts these bounded configuration values to `u128`
    /// before doing quorum arithmetic.
    pub voting_powers: Vec<u64>,
    /// Timeout before view change (milliseconds)
    pub view_timeout_ms: u64,
    /// BLS public keys for each validator (same order as validators), 48 bytes each
    pub bls_pubkeys: Vec<Vec<u8>>,
    /// Our BLS secret key (32 bytes seed), None if BLS disabled
    pub bls_secret_key: Option<[u8; 32]>,
}

impl fmt::Debug for ConsensusConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConsensusConfig")
            .field("epoch", &self.epoch)
            .field(
                "genesis_hash",
                &crate::types::hash_short(&self.genesis_hash),
            )
            .field("node_id", &self.node_id)
            .field("validators", &self.validators)
            .field("voting_powers", &self.voting_powers)
            .field("view_timeout_ms", &self.view_timeout_ms)
            .field("bls_pubkeys_count", &self.bls_pubkeys.len())
            .field("bls_secret_key_present", &self.bls_secret_key.is_some())
            .finish()
    }
}

impl ConsensusConfig {
    /// Number of validators
    pub fn n(&self) -> usize {
        self.validators.len()
    }

    /// Build the canonical active committee for this configuration.
    pub fn committee(&self) -> Result<Committee, String> {
        Committee::from_config(self)
    }

    /// Build the authentication context for this configuration.
    pub fn context(&self) -> Result<ConsensusContext, String> {
        self.committee()
            .map(|committee| committee.context_with_genesis(self.epoch, self.genesis_hash))
    }

    /// Number of validators with optional dynamic override
    pub fn n_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        dynamic
            .map(|d| d.len())
            .unwrap_or_else(|| self.validators.len())
    }

    /// Maximum Byzantine faults tolerated: f = (n-1)/3
    pub fn f(&self) -> usize {
        (self.n() - 1) / 3
    }

    /// Maximum Byzantine faults with optional dynamic override
    pub fn f_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        (self.n_with(dynamic) - 1) / 3
    }

    /// Quorum size: need majority for safety
    /// For BFT with n=3f+1: quorum = 2f+1
    /// But for n=3, we use simple majority (2) for testing
    pub fn quorum(&self) -> usize {
        let bft_quorum = 2 * self.f() + 1;
        let majority = self.n() / 2 + 1;
        // Use the larger of BFT quorum or simple majority
        bft_quorum.max(majority)
    }

    /// Quorum size with optional dynamic override
    pub fn quorum_with(&self, dynamic: Option<&[NodeId]>) -> usize {
        let n = self.n_with(dynamic);
        let f = self.f_with(dynamic);
        let bft_quorum = 2 * f + 1;
        let majority = n / 2 + 1;
        bft_quorum.max(majority)
    }

    /// Check if we are the leader for a given view
    pub fn is_leader(&self, view: View) -> bool {
        self.leader_of_active(view) == self.node_id
    }

    /// Check if we are the leader with dynamic validator set
    pub fn is_leader_with(&self, view: View, dynamic: Option<&[NodeId]>) -> bool {
        self.leader_of_with(view, dynamic) == self.node_id
    }

    /// Get leader for a given view (legacy canonical round-robin)
    pub fn leader_of(&self, view: View) -> NodeId {
        let validators = canonical_validator_order(&self.validators);
        if validators.is_empty() {
            return self.node_id;
        }
        validators[(view as usize) % validators.len()]
    }

    /// Get the canonical stake-weighted leader for the active committee.
    ///
    /// The validator/power parallel arrays are normalized by
    /// `leader_of_weighted`, so configuration input order cannot change the
    /// proposer schedule. Invalid/incomplete local configuration falls back
    /// to canonical round-robin; live runners reject that configuration before
    /// starting.
    pub fn leader_of_active(&self, view: View) -> NodeId {
        self.committee()
            .map(|committee| committee.leader(view))
            .unwrap_or_else(|_| {
                if self.validators.is_empty() {
                    self.node_id
                } else {
                    let validators = canonical_validator_order(&self.validators);
                    validators[(view as usize) % validators.len()]
                }
            })
    }

    /// Get leader with dynamic validator set
    pub fn leader_of_with(&self, view: View, dynamic: Option<&[NodeId]>) -> NodeId {
        let validators = canonical_validator_order(dynamic.unwrap_or(&self.validators));
        if validators.is_empty() {
            return self.node_id; // Single-node fallback
        }
        validators[(view as usize) % validators.len()]
    }

    /// Get leader using weighted selection based on stake
    ///
    /// Leaders are selected proportionally to stake using a deterministic
    /// hash slot. The hash prevents a high-power offline validator from
    /// monopolizing one long contiguous range of views.
    pub fn leader_of_weighted(&self, view: View, stakes: &[(NodeId, u64)]) -> NodeId {
        let stakes = stakes
            .iter()
            .map(|(node_id, stake)| (*node_id, u128::from(*stake)))
            .collect::<Vec<_>>();
        weighted_leader(view, &stakes, self.node_id)
    }

    /// Check if we are the leader using weighted selection
    pub fn is_leader_weighted(&self, view: View, stakes: &[(NodeId, u64)]) -> bool {
        self.leader_of_weighted(view, stakes) == self.node_id
    }

    /// Get active validators, preferring dynamic set if available
    pub fn active_validators<'a>(&'a self, dynamic: Option<&'a [NodeId]>) -> &'a [NodeId] {
        dynamic.unwrap_or(&self.validators)
    }

    /// Update the static validator list (used during epoch transitions)
    pub fn update_validators(&mut self, validators: Vec<NodeId>, bls_pubkeys: Vec<Vec<u8>>) {
        let voting_powers = vec![1; validators.len()];
        self.update_validators_with_powers(validators, bls_pubkeys, voting_powers);
    }

    /// Update the static validator list and voting powers.
    pub fn update_validators_with_powers(
        &mut self,
        validators: Vec<NodeId>,
        bls_pubkeys: Vec<Vec<u8>>,
        voting_powers: Vec<u64>,
    ) {
        self.validators = validators;
        self.bls_pubkeys = bls_pubkeys;
        self.voting_powers = voting_powers;
    }

    /// Check if BLS is enabled
    pub fn bls_enabled(&self) -> bool {
        !self.bls_pubkeys.is_empty() && self.bls_secret_key.is_some()
    }

    /// Whether the active committee is configured to authenticate votes with
    /// BLS, regardless of whether this process has its own signing key.
    pub fn bls_configured(&self) -> bool {
        !self.bls_pubkeys.is_empty()
    }

    /// Get BLS public key for a validator
    pub fn bls_pubkey(&self, node_id: &NodeId) -> Option<&[u8]> {
        self.validators
            .iter()
            .position(|v| v == node_id)
            .and_then(|i| self.bls_pubkeys.get(i))
            .map(|v| v.as_slice())
    }

    /// Create config for single-node testing with BLS enabled
    pub fn single_node() -> Self {
        let node_id = [1u8; 32];
        // Use deterministic seed for testing
        let bls_seed = [42u8; 32];
        let bls_sk = BlsSecretKey::from_seed(&bls_seed);
        let bls_pk = bls_sk.public_key().to_bytes().to_vec();

        Self {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![bls_pk],
            bls_secret_key: Some(bls_seed),
        }
    }

    /// Get our BLS secret key
    pub fn bls_secret_key(&self) -> Option<BlsSecretKey> {
        self.bls_secret_key
            .map(|seed| BlsSecretKey::from_seed(&seed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ConsensusConfig {
        ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: [1u8; 32],
            validators: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            voting_powers: vec![1, 1, 1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        }
    }

    #[test]
    fn consensus_config_debug_does_not_expose_bls_secret_seed() {
        let seed = [0xabu8; 32];
        let mut config = test_config();
        config.bls_pubkeys = vec![vec![0xcdu8; 48]];
        config.bls_secret_key = Some(seed);

        let rendered = format!("{config:?}");

        assert!(rendered.contains("bls_pubkeys_count: 1"));
        assert!(rendered.contains("bls_secret_key_present: true"));
        assert!(!rendered.contains(&hex::encode(seed)));
        assert!(!rendered.contains(&format!("{seed:?}")));
        assert!(!rendered.contains("bls_secret_key: Some"));
        assert!(!rendered.contains("bls_pubkeys: [["));
    }

    #[test]
    fn test_round_robin_leader() {
        let config = test_config();

        // Round-robin should cycle through validators
        assert_eq!(config.leader_of(0), [1u8; 32]);
        assert_eq!(config.leader_of(1), [2u8; 32]);
        assert_eq!(config.leader_of(2), [3u8; 32]);
        assert_eq!(config.leader_of(3), [1u8; 32]); // Wraps around
    }

    #[test]
    fn committee_hash_and_context_are_canonical() {
        let config = test_config();
        let committee = config.committee().unwrap();

        let mut reordered = config.clone();
        reordered.validators = vec![[3u8; 32], [1u8; 32], [2u8; 32]];
        reordered.voting_powers = vec![1, 1, 1];
        let reordered_committee = reordered.committee().unwrap();

        assert_eq!(committee.hash(), reordered_committee.hash());
        assert_eq!(
            config.context().unwrap(),
            ConsensusContext::new(0, committee.hash())
        );
        assert_eq!(committee.initial_context(), config.context().unwrap());
    }

    #[test]
    fn genesis_domain_is_deterministic_and_sensitive_to_chain_parameters() {
        let committee_hash = [7u8; 32];
        let first = genesis_domain_hash("hyperlicked-local", 0, 3000, committee_hash);
        let second = genesis_domain_hash("hyperlicked-local", 0, 3000, committee_hash);
        assert_eq!(first, second);
        assert_ne!(
            first,
            genesis_domain_hash("hyperlicked-other", 0, 3000, committee_hash)
        );
        assert_ne!(
            first,
            genesis_domain_hash("hyperlicked-local", 1, 3000, committee_hash)
        );
        assert_ne!(
            first,
            genesis_domain_hash("hyperlicked-local", 0, 3001, committee_hash)
        );
        let mut different_committee = committee_hash;
        different_committee[0] ^= 1;
        assert_ne!(
            first,
            genesis_domain_hash("hyperlicked-local", 0, 3000, different_committee)
        );
    }

    #[test]
    fn genesis_domain_allocations_are_canonical_and_authenticated() {
        let committee_hash = [7u8; 32];
        let ordered = vec![("alice".to_string(), 11), ("bob".to_string(), 22)];
        let reversed = vec![("bob".to_string(), 22), ("alice".to_string(), 11)];

        assert_eq!(
            genesis_domain_hash_with_allocations(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &ordered,
            ),
            genesis_domain_hash_with_allocations(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &reversed,
            )
        );

        let mut changed_amount = ordered.clone();
        changed_amount[0].1 += 1;
        assert_ne!(
            genesis_domain_hash_with_allocations(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &ordered,
            ),
            genesis_domain_hash_with_allocations(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &changed_amount,
            )
        );
    }

    #[test]
    fn application_genesis_commitment_canonicalizes_records_and_empty_allocations() {
        let committee_hash = [7u8; 32];
        let validators = vec![
            GenesisApplicationValidator {
                node_id: [2u8; 32],
                operator: "operator-two".to_string(),
                voting_power: 2,
                self_stake: 2_000_000,
                commission_bps: 17,
            },
            GenesisApplicationValidator {
                node_id: [1u8; 32],
                operator: "operator-one".to_string(),
                voting_power: 1,
                self_stake: 1_000_000,
                commission_bps: 0,
            },
        ];
        let reordered = vec![validators[1].clone(), validators[0].clone()];
        let ordered_allocations = vec![("alice".to_string(), 11), ("bob".to_string(), 22)];
        let reversed_allocations = vec![("bob".to_string(), 22), ("alice".to_string(), 11)];

        let first = genesis_domain_hash_with_application(
            "hyperlicked-local",
            0,
            3000,
            committee_hash,
            &validators,
            &ordered_allocations,
        );
        assert_eq!(
            first,
            genesis_domain_hash_with_application(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &reordered,
                &reversed_allocations,
            )
        );

        let empty = genesis_domain_hash_with_application(
            "hyperlicked-local",
            0,
            3000,
            committee_hash,
            &validators,
            &[],
        );
        assert_ne!(empty, first);
        // The application marker/policy schema is present even when there are
        // no explicit allocations; the legacy domain is a different context.
        assert_ne!(
            empty,
            genesis_domain_hash("hyperlicked-local", 0, 3000, committee_hash)
        );
    }

    #[test]
    fn application_genesis_commitment_binds_all_bootstrap_and_reward_policy_fields() {
        let committee_hash = [7u8; 32];
        let validators = vec![GenesisApplicationValidator {
            node_id: [1u8; 32],
            operator: "operator-one".to_string(),
            voting_power: 1,
            self_stake: 1_000_000,
            commission_bps: 0,
        }];
        let allocations = vec![("alice".to_string(), 11)];
        let baseline = application_genesis_commitment(
            "hyperlicked-local",
            0,
            3000,
            committee_hash,
            &validators,
            &allocations,
        );

        let mut changed_validator = validators[0].clone();
        changed_validator.operator = "operator-two".to_string();
        assert_ne!(
            baseline,
            application_genesis_commitment(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &[changed_validator],
                &allocations,
            )
        );

        let mut changed_policy = GENESIS_APPLICATION_POLICY;
        changed_policy.reward_apy_bps += 1;
        assert_ne!(
            baseline,
            application_genesis_commitment_with_policy(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &validators,
                &allocations,
                changed_policy,
            )
        );

        let mut changed_intervals = GENESIS_APPLICATION_POLICY;
        changed_intervals.reward_accrual_interval_ms += 1;
        changed_intervals.reward_auto_compound_interval_ms += 1;
        changed_intervals.reward_year_ms += 1;
        assert_ne!(
            baseline,
            application_genesis_commitment_with_policy(
                "hyperlicked-local",
                0,
                3000,
                committee_hash,
                &validators,
                &allocations,
                changed_intervals,
            )
        );
    }

    #[test]
    fn genesis_application_policy_matches_fixed_hyck_economics() {
        use crate::app::staking::types;

        assert_eq!(HYCK_DECIMALS, 6);
        assert_eq!(HYCK_MAX_SUPPLY_HYCK, 1_000_000_000);
        assert_eq!(HYCK_MAX_SUPPLY_BASE_UNITS, 1_000_000_000_000_000);
        assert_eq!(HYCK_EMISSIONS_RESERVE_HYCK, 388_880_000);
        assert_eq!(HYCK_EMISSIONS_RESERVE_BASE_UNITS, 388_880_000_000_000);
        assert_eq!(
            HYCK_GENESIS_ALLOCATABLE_SUPPLY_BASE_UNITS,
            611_120_000_000_000
        );
        assert_eq!(GENESIS_REWARD_POLICY_VERSION, 1);
        assert_eq!(GENESIS_REWARD_FORMULA_VERSION, 1);
        assert_eq!(GENESIS_APPLICATION_POLICY.reward_apy_bps, 237);
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_anchor_stake_base_units,
            400_000_000_000_000
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_accrual_interval_ms,
            5_400_000
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_auto_compound_interval_ms,
            86_400_000
        );
        assert_eq!(GENESIS_APPLICATION_POLICY.reward_year_ms, 31_536_000_000);
        // Keep the domain policy tied to the staking source constants rather
        // than a second hand-maintained copy.
        assert_eq!(
            GENESIS_APPLICATION_POLICY.hyck_base_units_per_hyck,
            types::HYCK_BASE_UNITS_PER_HYCK
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.hyck_max_supply_base_units,
            types::HYCK_TOTAL_SUPPLY
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.hyck_emissions_reserve_base_units,
            types::HYCK_GENESIS_EMISSIONS_RESERVE
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_apy_bps,
            types::STAKING_REWARD_APY_BPS
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_anchor_stake_base_units,
            types::STAKING_REWARD_ANCHOR_STAKE
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_commission_denominator_bps,
            types::STAKING_REWARD_BPS_DENOMINATOR
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_accrual_interval_ms,
            types::STAKING_REWARD_EPOCH_MS
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_year_ms,
            types::STAKING_REWARD_YEAR_MS
        );
        assert_eq!(
            GENESIS_APPLICATION_POLICY.reward_auto_compound_interval_ms,
            types::STAKING_AUTO_COMPOUND_INTERVAL_MS
        );
    }

    #[test]
    fn genesis_domain_v4_rejects_the_v2_control_plane_domain() {
        let chain_id = "hyperlicked-local";
        let epoch = 0u64;
        let view_timeout_ms = 3000u64;
        let committee_hash = [7u8; 32];
        let current = genesis_domain_hash(chain_id, epoch, view_timeout_ms, committee_hash);

        let mut legacy = Vec::new();
        legacy.extend_from_slice(b"HYPERLICKED_GENESIS_DOMAIN_V2");
        legacy.extend_from_slice(&4u16.to_le_bytes());
        legacy.extend_from_slice(&3u16.to_le_bytes());
        legacy.extend_from_slice(&(chain_id.len() as u64).to_le_bytes());
        legacy.extend_from_slice(chain_id.as_bytes());
        legacy.extend_from_slice(&epoch.to_le_bytes());
        legacy.extend_from_slice(&view_timeout_ms.to_le_bytes());
        legacy.extend_from_slice(&committee_hash);

        assert_ne!(current, hash(&legacy));
    }

    #[test]
    fn nonzero_config_epoch_is_rejected_in_static_phase() {
        let mut config = test_config();
        config.epoch = 1;

        assert!(config.committee().is_err());
        assert!(config.context().is_err());
    }

    #[test]
    fn committee_rejects_more_than_the_protocol_member_limit() {
        let validators: Vec<_> = (1u8..=22).map(|id| [id; 32]).collect();
        let config = ConsensusConfig {
            epoch: 0,
            genesis_hash: [0u8; 32],
            node_id: validators[0],
            validators,
            voting_powers: vec![1; 22],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![],
            bls_secret_key: None,
        };

        let error = config
            .committee()
            .expect_err("committee larger than the protocol limit must fail");
        assert!(error.contains("at most 21"));
    }

    #[test]
    fn leader_selection_is_canonical_for_input_order() {
        let mut reversed = test_config();
        reversed.validators = vec![[3u8; 32], [1u8; 32], [2u8; 32]];
        reversed.voting_powers = vec![1, 1, 1];

        assert_eq!(reversed.leader_of(0), [1u8; 32]);
        assert_eq!(reversed.leader_of(1), [2u8; 32]);
        assert_eq!(reversed.leader_of(2), [3u8; 32]);

        let ordered_stakes = vec![([1u8; 32], 1), ([2u8; 32], 2), ([3u8; 32], 3)];
        let reversed_stakes = vec![([3u8; 32], 3), ([1u8; 32], 1), ([2u8; 32], 2)];
        for view in 0..12 {
            assert_eq!(
                reversed.leader_of_weighted(view, &ordered_stakes),
                reversed.leader_of_weighted(view, &reversed_stakes)
            );
        }

        let mut weighted = test_config();
        weighted.voting_powers = vec![1, 2, 3];
        let mut weighted_reversed = weighted.clone();
        weighted_reversed.validators = vec![[3u8; 32], [1u8; 32], [2u8; 32]];
        weighted_reversed.voting_powers = vec![3, 1, 2];
        for view in 0..24 {
            assert_eq!(
                weighted.leader_of_active(view),
                weighted_reversed.leader_of_active(view)
            );
        }

        let mut scaled = weighted.clone();
        scaled.voting_powers = vec![100, 200, 300];
        for view in 0..1024 {
            assert_eq!(
                weighted.leader_of_active(view),
                scaled.leader_of_active(view)
            );
        }
    }

    #[test]
    fn test_weighted_leader_equal_stakes() {
        let config = test_config();

        let stakes = vec![([1u8; 32], 100), ([2u8; 32], 100), ([3u8; 32], 100)];
        let mut counts = [0u32; 3];
        let mut longest_run = 0usize;
        let mut current_run = 0usize;
        let mut previous = None;
        for view in 0..6000 {
            let leader = config.leader_of_weighted(view, &stakes);
            let index = if leader == [1u8; 32] {
                0
            } else if leader == [2u8; 32] {
                1
            } else if leader == [3u8; 32] {
                2
            } else {
                unreachable!()
            };
            counts[index] += 1;
            if previous == Some(index) {
                current_run += 1;
            } else {
                current_run = 1;
            }
            longest_run = longest_run.max(current_run);
            previous = Some(index);
        }

        assert!(counts.iter().all(|count| (1500..=2500).contains(count)));
        assert!(longest_run < 30);
    }

    #[test]
    fn test_weighted_leader_unequal_stakes() {
        let config = test_config();

        let stakes = vec![([1u8; 32], 1), ([2u8; 32], 2), ([3u8; 32], 3)];
        let mut counts = [0u32; 3];
        for view in 0..60_000 {
            let leader = config.leader_of_weighted(view, &stakes);
            if leader == [1u8; 32] {
                counts[0] += 1;
            } else if leader == [2u8; 32] {
                counts[1] += 1;
            } else if leader == [3u8; 32] {
                counts[2] += 1;
            } else {
                unreachable!();
            }
        }

        assert!(counts[2] > counts[1]);
        assert!(counts[1] > counts[0]);
        assert!(counts[2] >= counts[0] * 2);
    }

    #[test]
    fn test_weighted_leader_distribution() {
        let config = test_config();

        let stakes = vec![([1u8; 32], 1), ([2u8; 32], 3), ([3u8; 32], 6)];
        let mut counts = [0u32; 3];
        for view in 0..100_000 {
            let leader = config.leader_of_weighted(view, &stakes);
            if leader == [1u8; 32] {
                counts[0] += 1;
            } else if leader == [2u8; 32] {
                counts[1] += 1;
            } else {
                counts[2] += 1;
            }
        }

        assert!((8000..=12000).contains(&counts[0]));
        assert!((26000..=34000).contains(&counts[1]));
        assert!((56000..=64000).contains(&counts[2]));
    }

    #[test]
    fn large_power_does_not_monopolize_a_contiguous_view_range() {
        let config = test_config();
        let stakes = vec![([1u8; 32], 10_000), ([2u8; 32], 1)];
        let mut longest_high_power_run = 0usize;
        let mut current_high_power_run = 0usize;
        let mut saw_low_power_leader = false;

        for view in 0..100_000 {
            if config.leader_of_weighted(view, &stakes) == [1u8; 32] {
                current_high_power_run += 1;
                longest_high_power_run = longest_high_power_run.max(current_high_power_run);
            } else {
                saw_low_power_leader = true;
                current_high_power_run = 0;
            }
        }

        assert!(saw_low_power_leader);
        assert!(longest_high_power_run < 100_000);
    }

    #[test]
    fn test_weighted_leader_empty_stakes() {
        let config = test_config();

        // Empty stakes should return self
        let stakes: Vec<(NodeId, u64)> = vec![];
        assert_eq!(config.leader_of_weighted(0, &stakes), config.node_id);
    }

    #[test]
    fn test_weighted_leader_zero_stakes() {
        let config = test_config();

        // All zero stakes should fall back to round-robin
        let stakes = vec![([1u8; 32], 0), ([2u8; 32], 0), ([3u8; 32], 0)];

        // Falls back to round-robin
        assert_eq!(config.leader_of_weighted(0, &stakes), [1u8; 32]);
        assert_eq!(config.leader_of_weighted(1, &stakes), [2u8; 32]);
        assert_eq!(config.leader_of_weighted(2, &stakes), [3u8; 32]);
    }
}
