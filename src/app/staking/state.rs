//! Staking State
//!
//! Core state management for the staking system.

use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::types::{
    Delegation, EpochSnapshot, Evidence, LivenessRecord, StaticValidatorBootstrap, UnstakeRequest,
    ValidatorInfo, ValidatorSetUpdate, ValidatorStatus, HYCK_BASE_UNITS_PER_HYCK,
    MAX_ACTIVE_VALIDATORS, MIN_SELF_STAKE, ROUNDS_PER_EPOCH,
};
use crate::app::Address;
use crate::crypto::bls::{BlsProofOfPossession, BlsPublicKey};
use crate::types::{Committee, ConsensusContext, Hash, NodeId};

/// Shallow, copy-on-write storage for large staking collections.
///
/// Immutable access keeps the shared allocation.  Mutable access detaches
/// through `Arc::make_mut`, so a cloned `StakingState` cannot mutate its
/// parent or a sibling version.
#[derive(Debug)]
pub struct CowShared<T>(Arc<T>);

impl<T> Clone for CowShared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for CowShared<T> {
    fn default() -> Self {
        Self(Arc::new(T::default()))
    }
}

impl<T> From<T> for CowShared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Deref for CowShared<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T: Clone> DerefMut for CowShared<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: Serialize> Serialize for CowShared<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for CowShared<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::from)
    }
}

impl<'a, T> IntoIterator for &'a CowShared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_ref().into_iter()
    }
}

impl<T> IntoIterator for CowShared<T>
where
    T: Clone + IntoIterator,
{
    type Item = T::Item;
    type IntoIter = T::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        match Arc::try_unwrap(self.0) {
            Ok(value) => value.into_iter(),
            Err(shared) => shared.as_ref().clone().into_iter(),
        }
    }
}

impl<T, Item> std::iter::FromIterator<Item> for CowShared<T>
where
    T: std::iter::FromIterator<Item>,
{
    fn from_iter<I: IntoIterator<Item = Item>>(iter: I) -> Self {
        let value: T = iter.into_iter().collect();
        Self::from(value)
    }
}

impl<T: PartialEq> PartialEq<T> for CowShared<T> {
    fn eq(&self, other: &T) -> bool {
        self.0.as_ref() == other
    }
}

impl<T: PartialEq> PartialEq for CowShared<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_ref() == other.0.as_ref()
    }
}

impl<T: Clone + Default> CowShared<T> {
    pub(crate) fn take(&mut self) -> T {
        std::mem::take(Arc::make_mut(&mut self.0))
    }
}

#[cfg(test)]
impl<T> CowShared<T> {
    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Complete staking state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingState {
    /// Registered validators by operator address
    pub validators: CowShared<HashMap<Address, ValidatorInfo>>,
    /// Validator lookup by node_id
    #[serde(skip)]
    pub node_to_operator: CowShared<HashMap<NodeId, Address>>,
    /// Delegations: (delegator, validator) -> Delegation
    pub delegations: CowShared<HashMap<(Address, Address), Delegation>>,
    /// Unstake queue by delegator
    pub unstake_queue: CowShared<HashMap<Address, Vec<UnstakeRequest>>>,
    /// Current epoch number
    pub current_epoch: u64,
    /// Snapshot of validator set at epoch start
    pub epoch_snapshot: Option<EpochSnapshot>,
    /// Liveness records by node_id
    pub liveness: CowShared<HashMap<NodeId, LivenessRecord>>,
    /// Pending evidence to process
    pub pending_evidence: CowShared<Vec<Evidence>>,
    /// Total staked across all validators
    pub total_staked: i64,
    /// Explicitly funded future staking-emissions reserve, in base units.
    ///
    /// This value may only be initialized by moving already-issued native
    /// HYCK into the reserve; reward accrual never increases it.
    #[serde(default)]
    pub emissions_reserve: i64,
    /// Last timestamp included in reward accrual, in milliseconds.
    #[serde(default)]
    pub last_reward_accrual_timestamp: u64,
    /// Whether `last_reward_accrual_timestamp` is an established clock anchor.
    /// Old snapshots deserialize this as false and anchor on their first
    /// explicit reward call rather than guessing elapsed time.
    #[serde(default)]
    pub reward_clock_initialized: bool,
    /// Fractional base-unit reward numerator retained across accrual calls.
    /// The value is always less than `STAKING_REWARD_YEAR_MS`.
    #[serde(default)]
    pub reward_accrual_remainder: u64,
    /// Last timestamp at which pending rewards were auto-compounded.
    #[serde(default)]
    pub last_reward_compound_timestamp: u64,
    /// Whether staking is enabled (for single-node compatibility)
    pub enabled: bool,
    /// The node-configured consensus context used for evidence validation.
    ///
    /// This is runtime configuration rather than application state: it is
    /// deliberately not serialized into snapshots and must be restored from
    /// the node's genesis configuration before accepting evidence.
    #[serde(skip)]
    pub(crate) consensus_context: Option<ConsensusContext>,
    /// Genesis domain retained when only the current staking-derived
    /// committee is available.  A zero value means no evidence may be
    /// accepted.
    #[serde(skip)]
    pub(crate) consensus_genesis_hash: Hash,
    /// Trusted static-epoch committee supplied by node configuration.
    ///
    /// This is runtime-only: it is intentionally absent from snapshots and
    /// state commitments.  A restored snapshot must receive this binding
    /// again before evidence can be accepted.
    #[serde(skip)]
    pub(crate) authoritative_committee: Option<Committee>,
    /// Whether this state came from a canonical node path that requires the
    /// trusted committee binding.  Direct staking fixtures retain the legacy
    /// derived-context behavior until they opt into the binding.
    #[serde(skip)]
    pub(crate) require_authoritative_committee: bool,
}

impl Default for StakingState {
    fn default() -> Self {
        Self::new()
    }
}

impl StakingState {
    /// Create new staking state
    pub fn new() -> Self {
        Self {
            validators: CowShared::default(),
            node_to_operator: CowShared::default(),
            delegations: CowShared::default(),
            unstake_queue: CowShared::default(),
            current_epoch: 0,
            epoch_snapshot: None,
            liveness: CowShared::default(),
            pending_evidence: CowShared::default(),
            total_staked: 0,
            emissions_reserve: 0,
            last_reward_accrual_timestamp: 0,
            reward_clock_initialized: false,
            reward_accrual_remainder: 0,
            last_reward_compound_timestamp: 0,
            enabled: true,
            consensus_context: None,
            consensus_genesis_hash: [0u8; 32],
            authoritative_committee: None,
            require_authoritative_committee: false,
        }
    }

    /// Record an explicitly funded emissions reserve.
    ///
    /// The staking module has no account ledger and therefore cannot perform
    /// the treasury debit itself.  Callers must move the same amount from an
    /// existing native-HYCK balance before invoking this method.  Keeping the
    /// operation explicit prevents reward accrual from becoming a mint path.
    pub fn set_emissions_reserve(&mut self, amount: i64) -> Result<(), StakingError> {
        if amount < 0 || amount > super::types::HYCK_GENESIS_EMISSIONS_RESERVE {
            return Err(StakingError::InvalidRewardAmount);
        }
        self.emissions_reserve = amount;
        Ok(())
    }

    /// Initialize the canonical genesis emissions reserve after the caller
    /// has transferred the reserve from the already-issued treasury balance.
    pub fn initialize_genesis_emissions_reserve(&mut self) -> Result<(), StakingError> {
        self.set_emissions_reserve(super::types::HYCK_GENESIS_EMISSIONS_RESERVE)
    }

    /// Return the explicitly funded emissions balance.
    pub fn emissions_reserve_balance(&self) -> i64 {
        self.emissions_reserve
    }

    /// Method-form accessor for callers that do not bind to the persisted
    /// field layout.
    pub fn emissions_reserve(&self) -> i64 {
        self.emissions_reserve_balance()
    }

    /// Bind evidence validation to the node's static epoch-0 context.
    ///
    /// Dynamic/historical committee lookup is intentionally not implemented
    /// in this phase, so callers must update this only when the active static
    /// context is known from trusted node configuration.
    pub fn set_consensus_context(&mut self, context: ConsensusContext) {
        self.consensus_genesis_hash = context.genesis_hash;
        self.consensus_context = Some(context);
        if self
            .authoritative_committee
            .as_ref()
            .is_some_and(|committee| committee.validate_context(context).is_err())
        {
            self.authoritative_committee = None;
        }
    }

    /// Bind evidence validation to a configured genesis domain while the
    /// committee is derived from the current staking set.
    pub fn set_consensus_genesis_hash(&mut self, genesis_hash: Hash) {
        self.consensus_genesis_hash = genesis_hash;
        if let Some(context) = self.consensus_context {
            if context.genesis_hash != genesis_hash {
                self.consensus_context = None;
                self.authoritative_committee = None;
            }
        }
    }

    /// Bind the application evidence path to the node's trusted static
    /// committee.  Committee data is runtime-only and does not enter the
    /// application state root; the registered validator records checked by
    /// evidence remain ordinary application state.
    pub fn bind_authoritative_committee(
        &mut self,
        committee: Committee,
        context: ConsensusContext,
    ) -> Result<(), StakingError> {
        committee
            .validate_context(context)
            .map_err(StakingError::InvalidAuthoritativeCommittee)?;
        if context.epoch != 0 || !context.has_genesis_domain() {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "static committee binding requires epoch 0 and a nonzero genesis domain"
                    .to_string(),
            ));
        }
        if committee.members().iter().any(|member| {
            member.bls_pubkey.as_ref().is_none_or(|key| {
                if key.len() != 48 {
                    return true;
                }
                let mut bytes = [0u8; 48];
                bytes.copy_from_slice(key);
                BlsPublicKey::from_bytes(&bytes).is_err()
            })
        }) {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "every static committee member must have a valid BLS public key".to_string(),
            ));
        }

        if self.current_epoch != 0 {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "static committee binding requires staking state at epoch 0".to_string(),
            ));
        }

        // Every static member must have a canonical staking record.  Binding
        // an unregistered committee member would make the runtime committee
        // authoritative for consensus while leaving its slashable application
        // record absent after snapshot recovery.
        for member in committee.members() {
            let Some(validator) = self
                .validators
                .values()
                .find(|validator| validator.node_id == member.node_id)
            else {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "static committee member {} has no registered staking record",
                    hex::encode(member.node_id)
                )));
            };
            let configured_key = member.bls_pubkey.as_deref().expect("validated above");
            if validator.bls_pubkey != configured_key {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "registered validator {} has a different BLS key",
                    hex::encode(member.node_id)
                )));
            }
            let mut key_bytes = [0u8; 48];
            key_bytes.copy_from_slice(configured_key);
            let public_key =
                BlsPublicKey::from_bytes(&key_bytes).map_err(|_| StakingError::InvalidBlsKey)?;
            let proof = BlsProofOfPossession::from_slice(&validator.bls_proof_of_possession)
                .map_err(|_| StakingError::InvalidBlsProofOfPossession)?;
            if !public_key.verify_proof_of_possession(
                &context.genesis_hash,
                &member.node_id,
                &proof,
            ) {
                return Err(StakingError::InvalidBlsProofOfPossession);
            }
        }

        let committee_nodes: HashSet<NodeId> = committee
            .members()
            .iter()
            .map(|member| member.node_id)
            .collect();
        let Some(snapshot) = self.epoch_snapshot.as_ref() else {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "static committee binding requires an epoch-0 staking snapshot".to_string(),
            ));
        };
        let snapshot_nodes: HashSet<NodeId> = snapshot.active_validators.iter().copied().collect();
        if snapshot.epoch != 0
            || snapshot.active_validators.len() != committee_nodes.len()
            || snapshot_nodes != committee_nodes
        {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "epoch-0 staking snapshot does not match the static committee".to_string(),
            ));
        }

        self.consensus_genesis_hash = context.genesis_hash;
        self.consensus_context = Some(context);
        self.authoritative_committee = Some(committee);
        self.require_authoritative_committee = true;
        Ok(())
    }

    /// Create deterministic, slashable staking records for the curated
    /// genesis committee.
    ///
    /// The current genesis schema carries voting power and PoP but not full
    /// staking economics.  Callers therefore pass the resolved operator,
    /// stake, and commission mapping explicitly.  Local fixtures use
    /// `system:genesis:<node-id>` and `voting_power * HYCK_BASE_UNITS_PER_HYCK`;
    /// a future mainnet genesis must provide real operator/stake semantics
    /// instead.
    pub fn bootstrap_static_committee(
        &mut self,
        committee: &Committee,
        records: &[StaticValidatorBootstrap],
        context: ConsensusContext,
    ) -> Result<(), StakingError> {
        committee
            .validate_context(context)
            .map_err(StakingError::InvalidAuthoritativeCommittee)?;
        if context.epoch != 0 || !context.has_genesis_domain() {
            return Err(StakingError::InvalidAuthoritativeCommittee(
                "static committee bootstrap requires epoch 0 and a nonzero genesis domain"
                    .to_string(),
            ));
        }

        let mut seen = HashSet::with_capacity(records.len());
        for record in records {
            if !seen.insert(record.node_id) {
                return Err(StakingError::InvalidAuthoritativeCommittee(
                    "static committee bootstrap contains a duplicate node".to_string(),
                ));
            }
            let member = committee.member(&record.node_id).ok_or_else(|| {
                StakingError::InvalidAuthoritativeCommittee(format!(
                    "bootstrap record {} is not a committee member",
                    hex::encode(record.node_id)
                ))
            })?;
            if member.voting_power != record.voting_power
                || member.bls_pubkey.as_deref() != Some(record.bls_pubkey.as_slice())
            {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "bootstrap record {} does not match the committee",
                    hex::encode(record.node_id)
                )));
            }
            let expected_stake = static_stake(record.voting_power)?;
            if record.self_stake != expected_stake {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "bootstrap record {} has stake {}, expected {} from voting power",
                    hex::encode(record.node_id),
                    record.self_stake,
                    expected_stake
                )));
            }
            if record.commission_bps < 0 || record.commission_bps > super::types::MAX_COMMISSION_BPS
            {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "bootstrap record {} has invalid commission",
                    hex::encode(record.node_id)
                )));
            }
            if record.bls_pubkey.len() != 48 || record.bls_proof_of_possession.len() != 96 {
                return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                    "bootstrap record {} has invalid BLS material",
                    hex::encode(record.node_id)
                )));
            }
            let mut key_bytes = [0u8; 48];
            key_bytes.copy_from_slice(&record.bls_pubkey);
            let public_key =
                BlsPublicKey::from_bytes(&key_bytes).map_err(|_| StakingError::InvalidBlsKey)?;
            let proof = BlsProofOfPossession::from_slice(&record.bls_proof_of_possession)
                .map_err(|_| StakingError::InvalidBlsProofOfPossession)?;
            if !public_key.verify_proof_of_possession(
                &context.genesis_hash,
                &record.node_id,
                &proof,
            ) {
                return Err(StakingError::InvalidBlsProofOfPossession);
            }

            let existing = self
                .validators
                .iter()
                .find(|(_, validator)| validator.node_id == record.node_id)
                .map(|(operator, validator)| (operator.clone(), validator.clone()));
            if let Some((operator, validator)) = existing {
                if validator.bls_pubkey != record.bls_pubkey
                    || validator.bls_proof_of_possession != record.bls_proof_of_possession
                    || validator.self_stake != record.self_stake
                    || validator.total_stake != record.self_stake
                {
                    return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                        "registered validator {} does not match static bootstrap",
                        hex::encode(record.node_id)
                    )));
                }
                if operator != record.operator {
                    // Existing application records may use an explicit
                    // operator.  Preserve that state, but never permit a
                    // second record for the same consensus identity.
                    continue;
                }
                continue;
            }

            self.register_validator(
                record.operator.clone(),
                record.node_id,
                record.bls_pubkey.clone(),
                record.bls_proof_of_possession.clone(),
                context.genesis_hash,
                record.self_stake,
                record.commission_bps,
            )?;
        }

        if seen.len() != committee.members().len() {
            return Err(StakingError::InvalidAuthoritativeCommittee(format!(
                "static committee bootstrap has {} records for {} members",
                seen.len(),
                committee.members().len()
            )));
        }
        self.rebuild_index()?;

        // Seed the static epoch explicitly.  Otherwise the first committed
        // block would enter the dynamic epoch-transition path and emit a
        // validator-set update even though epoch rotation is intentionally
        // disabled in this tranche.
        if self.epoch_snapshot.is_none() {
            let active_validators: Vec<_> = committee
                .members()
                .iter()
                .map(|member| member.node_id)
                .collect();
            for node_id in &active_validators {
                if let Some(validator) = self.get_validator_by_node_mut(node_id) {
                    if validator.status != ValidatorStatus::Tombstoned {
                        validator.status = ValidatorStatus::Active;
                    }
                }
                self.liveness.entry(*node_id).or_default();
            }
            let mut snapshot = EpochSnapshot::new(0, 0, 0);
            snapshot.active_validators = active_validators;
            snapshot.total_staked = self.total_staked;
            self.epoch_snapshot = Some(snapshot);
        }
        Ok(())
    }

    /// Return the trusted runtime committee, if one has been injected.
    pub fn authoritative_committee(&self) -> Option<&Committee> {
        self.authoritative_committee.as_ref()
    }

    /// Whether this state must be operated with a trusted static committee.
    ///
    /// Snapshot restoration deliberately sets this flag before the runtime
    /// committee is re-injected. Callers must not fall back to the
    /// staking-derived/dynamic path while that binding is pending.
    pub fn requires_authoritative_committee(&self) -> bool {
        self.require_authoritative_committee
    }

    /// Whether a trusted static committee is required but has not yet been
    /// restored from node configuration.
    pub fn static_committee_binding_pending(&self) -> bool {
        self.require_authoritative_committee && self.authoritative_committee.is_none()
    }

    /// Register a new validator
    pub fn register_validator(
        &mut self,
        operator: Address,
        node_id: NodeId,
        bls_pubkey: Vec<u8>,
        bls_proof_of_possession: Vec<u8>,
        chain_domain: [u8; 32],
        self_stake: i64,
        commission_bps: i64,
    ) -> Result<(), StakingError> {
        // Validate inputs
        if self_stake < MIN_SELF_STAKE {
            return Err(StakingError::InsufficientSelfStake);
        }
        if commission_bps < 0 || commission_bps > super::types::MAX_COMMISSION_BPS {
            return Err(StakingError::InvalidCommission);
        }
        if self.validators.contains_key(&operator) {
            return Err(StakingError::ValidatorAlreadyExists);
        }
        if self.node_to_operator.contains_key(&node_id) {
            return Err(StakingError::NodeIdAlreadyRegistered);
        }

        parse_and_verify_bls_binding(&bls_pubkey, &bls_proof_of_possession, chain_domain, node_id)?;
        if self
            .validators
            .values()
            .any(|validator| validator.bls_pubkey == bls_pubkey)
        {
            return Err(StakingError::BlsKeyAlreadyRegistered);
        }
        // Check the global aggregate before inserting the validator.  The
        // account-side self-bond transfer is performed by AppState, so a
        // direct staking mutation must never panic or leave a partial record
        // when the aggregate cannot represent the new stake.
        self.total_staked = self
            .total_staked
            .checked_add(self_stake)
            .ok_or(StakingError::StakeAggregateOverflow)?;

        let mut validator = ValidatorInfo::new(
            operator.clone(),
            node_id,
            bls_pubkey,
            bls_proof_of_possession,
            self_stake,
            commission_bps,
        );
        if self.reward_clock_initialized {
            // A validator registered after the reward clock starts cannot
            // claim the already-running epoch.
            validator.reward_eligible_stake = 0;
        }
        self.validators.insert(operator.clone(), validator);
        self.node_to_operator.insert(node_id, operator);

        Ok(())
    }

    /// Rotate a validator's BLS key.  The new key is retained for the next
    /// epoch's set calculation; the currently active consensus committee is
    /// immutable until that epoch transition commits.
    pub fn rotate_validator_key(
        &mut self,
        operator: &Address,
        new_bls_pubkey: Vec<u8>,
        bls_proof_of_possession: Vec<u8>,
        chain_domain: [u8; 32],
    ) -> Result<(), StakingError> {
        if self.requires_authoritative_committee() {
            return Err(StakingError::StaticCommitteeKeyRotationDisabled);
        }
        let validator = self
            .validators
            .get(operator)
            .ok_or(StakingError::ValidatorNotFound)?;
        let node_id = validator.node_id;
        let new_public_key = parse_and_verify_bls_binding(
            &new_bls_pubkey,
            &bls_proof_of_possession,
            chain_domain,
            node_id,
        )?;
        if self.validators.iter().any(|(address, validator)| {
            address != operator && validator.bls_pubkey == new_bls_pubkey
        }) {
            return Err(StakingError::BlsKeyAlreadyRegistered);
        }

        let validator = self
            .validators
            .get_mut(operator)
            .ok_or(StakingError::ValidatorNotFound)?;
        validator.bls_pubkey = new_public_key.to_bytes().to_vec();
        validator.bls_proof_of_possession = bls_proof_of_possession;
        Ok(())
    }

    /// Get validator by operator address
    pub fn get_validator(&self, operator: &Address) -> Option<&ValidatorInfo> {
        self.validators.get(operator)
    }

    /// Get validator by node_id
    pub fn get_validator_by_node(&self, node_id: &NodeId) -> Option<&ValidatorInfo> {
        self.node_to_operator
            .get(node_id)
            .and_then(|op| self.validators.get(op))
    }

    /// Get mutable validator by operator address
    pub fn get_validator_mut(&mut self, operator: &Address) -> Option<&mut ValidatorInfo> {
        self.validators.get_mut(operator)
    }

    /// Get mutable validator by node_id
    pub fn get_validator_by_node_mut(&mut self, node_id: &NodeId) -> Option<&mut ValidatorInfo> {
        let operator = self.node_to_operator.get(node_id)?.clone();
        self.validators.get_mut(&operator)
    }

    /// Add delegation to a validator
    pub fn delegate(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: i64,
    ) -> Result<(), StakingError> {
        if amount <= 0 {
            return Err(StakingError::InvalidAmount);
        }

        let val = self
            .validators
            .get(&validator)
            .ok_or(StakingError::ValidatorNotFound)?;

        if val.status == ValidatorStatus::Tombstoned {
            return Err(StakingError::ValidatorTombstoned);
        }

        let updated_validator_stake = val
            .total_stake
            .checked_add(amount)
            .ok_or(StakingError::StakeAggregateOverflow)?;
        let updated_total_staked = self
            .total_staked
            .checked_add(amount)
            .ok_or(StakingError::StakeAggregateOverflow)?;
        let key = (delegator.clone(), validator.clone());
        let updated_delegation = self
            .delegations
            .get(&key)
            .map(|delegation| {
                delegation
                    .amount
                    .checked_add(amount)
                    .ok_or(StakingError::StakeAggregateOverflow)
            })
            .transpose()?;

        // Update validator total stake
        self.validators
            .get_mut(&validator)
            .expect("validator was checked above")
            .total_stake = updated_validator_stake;
        self.total_staked = updated_total_staked;

        // Update or create delegation
        if let Some(delegation) = self.delegations.get_mut(&key) {
            delegation.amount = updated_delegation.expect("existing delegation amount checked");
        } else {
            let mut delegation = Delegation::new(delegator, validator, amount);
            if self.reward_clock_initialized {
                // New stake starts earning at the next reward epoch.
                delegation.reward_eligible_stake = 0;
            }
            self.delegations.insert(key, delegation);
        }

        Ok(())
    }

    /// Queue undelegation (enters 7-day unbonding period)
    pub fn undelegate(
        &mut self,
        delegator: Address,
        validator: Address,
        amount: i64,
        current_time: u64,
    ) -> Result<(), StakingError> {
        if amount <= 0 {
            return Err(StakingError::InvalidAmount);
        }

        let key = (delegator.clone(), validator.clone());
        let delegation_amount = self
            .delegations
            .get(&key)
            .ok_or(StakingError::DelegationNotFound)?;

        if delegation_amount.amount < amount {
            return Err(StakingError::InsufficientDelegation);
        }

        let validator_info = self
            .validators
            .get(&validator)
            .ok_or(StakingError::ValidatorNotFound)?;
        let updated_validator_stake = validator_info
            .total_stake
            .checked_sub(amount)
            .ok_or(StakingError::StakeAggregateOverflow)?;
        let updated_total_staked = self
            .total_staked
            .checked_sub(amount)
            .ok_or(StakingError::StakeAggregateOverflow)?;
        let completion_time = current_time
            .checked_add(super::types::UNSTAKE_DELAY_MS)
            .ok_or(StakingError::StakeAggregateOverflow)?;

        // Reduce delegation immediately.  A fully undelegated record with
        // pending rewards is retained as a zero-stake claim record; removing
        // it here would destroy the delegator's already-earned HYCK.  The
        // record is removed after its pending reward is claimed or after a
        // later delegation restores a positive amount.
        if delegation_amount.amount == amount {
            if delegation_amount.pending_rewards == 0 {
                self.delegations.remove(&key);
            } else {
                self.delegations
                    .get_mut(&key)
                    .expect("delegation was checked above")
                    .amount = 0;
                self.delegations
                    .get_mut(&key)
                    .expect("delegation was checked above")
                    .reward_eligible_stake = 0;
            }
        } else {
            let delegation = self
                .delegations
                .get_mut(&key)
                .expect("delegation was checked above");
            delegation.amount -= amount;
            delegation.reward_eligible_stake =
                delegation.reward_eligible_stake.min(delegation.amount);
        }

        // Reduce validator total stake
        self.validators
            .get_mut(&validator)
            .expect("validator was checked above")
            .total_stake = updated_validator_stake;
        self.total_staked = updated_total_staked;

        // Add to unstake queue
        let request = UnstakeRequest {
            delegator: delegator.clone(),
            validator: Some(validator.clone()),
            amount,
            completion_time,
        };
        self.unstake_queue
            .entry(delegator)
            .or_default()
            .push(request);

        Ok(())
    }

    /// Process completed unstake requests, returns total amount to return
    pub fn process_unstake_queue(&mut self, current_time: u64) -> Vec<(Address, i64)> {
        let mut completed = Vec::new();

        for (delegator, requests) in self.unstake_queue.iter_mut() {
            let (ready, pending): (Vec<_>, Vec<_>) = requests
                .drain(..)
                .partition(|r| r.completion_time <= current_time);

            let total_amount: i64 = ready.iter().map(|r| r.amount).sum();
            if total_amount > 0 {
                completed.push((delegator.clone(), total_amount));
            }
            *requests = pending;
        }

        // Clean up empty entries
        self.unstake_queue.retain(|_, v| !v.is_empty());

        completed
    }

    /// Process only the completed unstake requests owned by `delegator`.
    ///
    /// Unlike [`Self::process_unstake_queue`], this claim path must not drain
    /// another delegator's matured requests when a user claims their own
    /// funds.
    pub fn process_unstake_queue_for(&mut self, delegator: &Address, current_time: u64) -> i64 {
        let completed_amount = if let Some(requests) = self.unstake_queue.get_mut(delegator) {
            let mut completed_amount = 0i64;
            requests.retain(|request| {
                if request.completion_time <= current_time {
                    completed_amount += request.amount;
                    false
                } else {
                    true
                }
            });
            completed_amount
        } else {
            0
        };

        if self.unstake_queue.get(delegator).is_some_and(Vec::is_empty) {
            self.unstake_queue.remove(delegator);
        }

        completed_amount
    }

    /// Return the matured amount for a delegator without changing the queue.
    ///
    /// Application-level claim execution uses this preview before crediting
    /// the account so a failed balance update cannot consume the claim.
    pub fn completed_unstake_amount_for(
        &self,
        delegator: &Address,
        current_time: u64,
    ) -> Result<i64, StakingError> {
        self.unstake_queue
            .get(delegator)
            .map(|requests| {
                requests
                    .iter()
                    .filter(|request| request.completion_time <= current_time)
                    .try_fold(0i64, |total, request| {
                        total
                            .checked_add(request.amount)
                            .ok_or(StakingError::StakeAggregateOverflow)
                    })
            })
            .unwrap_or(Ok(0))
    }

    /// Get active validators sorted by stake (descending)
    pub fn active_validators(&self) -> Vec<NodeId> {
        let mut validators: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.can_be_active())
            .collect();

        // Sort by total stake descending, then by operator address for determinism
        validators.sort_by(|a, b| {
            b.total_stake
                .cmp(&a.total_stake)
                .then_with(|| a.operator.cmp(&b.operator))
        });

        // Take top N
        validators
            .into_iter()
            .take(MAX_ACTIVE_VALIDATORS)
            .map(|v| v.node_id)
            .collect()
    }

    /// Get active validator set for consensus
    ///
    /// Returns (node_ids, bls_pubkeys, stakes) tuples for the active set.
    /// Used to update consensus configuration on epoch transitions.
    pub fn active_validator_set_for_consensus(&self) -> ValidatorSetUpdate {
        let mut validators: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.can_be_active())
            .collect();

        // Sort by total stake descending, then by operator address for determinism
        validators.sort_by(|a, b| {
            b.total_stake
                .cmp(&a.total_stake)
                .then_with(|| a.operator.cmp(&b.operator))
        });

        // Take top N
        let active: Vec<_> = validators.into_iter().take(MAX_ACTIVE_VALIDATORS).collect();

        ValidatorSetUpdate {
            node_ids: active.iter().map(|validator| validator.node_id).collect(),
            bls_pubkeys: active
                .iter()
                .map(|validator| validator.bls_pubkey.clone())
                .collect(),
            stakes: active
                .iter()
                .map(|validator| {
                    (
                        validator.node_id,
                        stake_to_voting_power(validator.total_stake).expect(
                            "active validator total stake must contain at least one whole HYCK",
                        ),
                    )
                })
                .collect(),
        }
    }

    /// Check if should transition to new epoch
    pub fn should_transition_epoch(&self, view: u64) -> bool {
        match &self.epoch_snapshot {
            Some(snapshot) => {
                let rounds_in_epoch = view.saturating_sub(snapshot.start_view);
                rounds_in_epoch >= ROUNDS_PER_EPOCH
            }
            None => true, // First epoch
        }
    }

    /// Get delegations for a specific delegator
    pub fn delegations_for(&self, delegator: &Address) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|((d, _), _)| d == delegator)
            .map(|(_, del)| del)
            .collect()
    }

    /// Get all delegations to a specific validator
    pub fn delegations_to(&self, validator: &Address) -> Vec<&Delegation> {
        self.delegations
            .iter()
            .filter(|((_, v), _)| v == validator)
            .map(|(_, del)| del)
            .collect()
    }

    /// Get pending unstake requests for a delegator
    pub fn get_pending_unstakes(&self, delegator: &Address) -> Vec<&UnstakeRequest> {
        self.unstake_queue
            .get(delegator)
            .map(|requests| requests.iter().collect())
            .unwrap_or_default()
    }

    /// Get total amount in unbonding for a delegator
    pub fn total_unbonding(&self, delegator: &Address) -> i64 {
        self.unstake_queue
            .get(delegator)
            .map(|requests| requests.iter().map(|r| r.amount).sum())
            .unwrap_or(0)
    }

    /// Rebuild the transient node-to-operator mapping after deserialization.
    ///
    /// The replacement is atomic: malformed validator data leaves the
    /// existing index untouched.
    pub fn rebuild_index(&mut self) -> Result<(), StakingError> {
        let rebuilt = self.build_node_index()?;
        self.node_to_operator = rebuilt.into();
        Ok(())
    }

    /// Validate the validator map and its derived node index without mutating
    /// either one.
    pub fn validate_invariants(&self) -> Result<(), StakingError> {
        let rebuilt = self.build_node_index()?;
        if &rebuilt != &*self.node_to_operator {
            return Err(StakingError::ValidatorIndexMismatch);
        }
        Ok(())
    }

    /// Validate the authoritative staking state without consulting or
    /// mutating the transient `node_to_operator` index.
    ///
    /// The validator map, delegations, queues, snapshots, liveness records,
    /// and pending evidence are all persisted state.  This check therefore
    /// validates relationships between those records, while
    /// [`Self::validate_invariants`] remains the separate derived-index check.
    pub fn validate_primary_state(&self) -> Result<(), StakingError> {
        // Build the authoritative node map locally.  This both checks key and
        // node uniqueness and keeps this method independent of the cache.
        let mut operators_by_node = HashMap::with_capacity(self.validators.len());
        let mut bls_keys = HashSet::with_capacity(self.validators.len());
        let mut parsed_keys = Vec::with_capacity(self.validators.len());
        let mut pending_reward_total = 0i64;

        for (operator, validator) in &self.validators {
            if operator != &validator.operator {
                return Err(StakingError::ValidatorOperatorMismatch);
            }
            if operators_by_node
                .insert(validator.node_id, operator.clone())
                .is_some()
            {
                return Err(StakingError::DuplicateNodeId);
            }

            if validator.bls_pubkey.len() != 48 {
                return Err(StakingError::InvalidBlsKey);
            }
            let mut public_key_bytes = [0u8; 48];
            public_key_bytes.copy_from_slice(&validator.bls_pubkey);
            let public_key = BlsPublicKey::from_bytes(&public_key_bytes)
                .map_err(|_| StakingError::InvalidBlsKey)?;
            if !bls_keys.insert(validator.bls_pubkey.clone()) {
                return Err(StakingError::BlsKeyAlreadyRegistered);
            }

            // Parse the proof even when no trusted chain domain has been
            // restored.  Cryptographic binding is checked below whenever a
            // configured genesis domain is available.
            BlsProofOfPossession::from_slice(&validator.bls_proof_of_possession)
                .map_err(|_| StakingError::InvalidBlsProofOfPossession)?;

            if validator.self_stake < 0 || validator.total_stake < 0 {
                return Err(StakingError::InvalidValidatorStake);
            }
            // Equivocation slashing deliberately leaves tombstoned validators
            // below the registration minimum.  Other runtime statuses still
            // represent registered validators and retain the minimum stake.
            if validator.status != ValidatorStatus::Tombstoned
                && validator.self_stake < MIN_SELF_STAKE
            {
                return Err(StakingError::InsufficientSelfStake);
            }
            if validator.commission_bps < 0
                || validator.commission_bps > super::types::MAX_COMMISSION_BPS
            {
                return Err(StakingError::InvalidCommission);
            }
            if validator.pending_rewards < 0 {
                return Err(StakingError::InvalidRewardAmount);
            }
            if validator.reward_eligible_stake < 0
                || validator.reward_eligible_stake > validator.self_stake
            {
                return Err(StakingError::InvalidValidatorStake);
            }
            // Jailed validators carry their expiry.  A tombstoned validator
            // may retain a previous jail expiry because slashing does not
            // rewrite that field; all other non-jailed statuses clear it.
            if !matches!(
                validator.status,
                ValidatorStatus::Jailed | ValidatorStatus::Tombstoned
            ) && validator.jail_until != 0
            {
                return Err(StakingError::InvalidValidatorStatus);
            }
            if validator.status == ValidatorStatus::Jailed && validator.jail_until == 0 {
                return Err(StakingError::InvalidValidatorStatus);
            }

            pending_reward_total = pending_reward_total
                .checked_add(validator.pending_rewards)
                .ok_or(StakingError::RewardAggregateOverflow)?;
            parsed_keys.push((validator, public_key));
        }

        // A state loaded without runtime configuration can still be checked
        // for key/proof encoding.  Once the trusted genesis domain is known,
        // also verify that every stored proof is bound to that domain and node.
        if let Some(chain_domain) = self.trusted_chain_domain() {
            for (validator, public_key) in parsed_keys {
                let proof = BlsProofOfPossession::from_slice(&validator.bls_proof_of_possession)
                    .map_err(|_| StakingError::InvalidBlsProofOfPossession)?;
                if !public_key.verify_proof_of_possession(&chain_domain, &validator.node_id, &proof)
                {
                    return Err(StakingError::InvalidBlsProofOfPossession);
                }
            }
        }

        // Delegations are authoritative amounts.  Build per-validator sums
        // with checked arithmetic before comparing them with ValidatorInfo.
        let mut delegated_by_validator: HashMap<Address, i64> = HashMap::new();
        for ((delegator_key, validator_key), delegation) in &self.delegations {
            if delegator_key != &delegation.delegator || validator_key != &delegation.validator {
                return Err(StakingError::DelegationKeyMismatch);
            }
            // A zero-amount record is a deliberate claim-only tombstone for
            // rewards earned before a full undelegation.  It may not be an
            // empty record: zero stake and zero pending rewards is removed
            // eagerly by the mutation paths.
            if delegation.amount < 0 || (delegation.amount == 0 && delegation.pending_rewards == 0)
            {
                return Err(StakingError::InvalidAmount);
            }
            if delegation.pending_rewards < 0 {
                return Err(StakingError::InvalidRewardAmount);
            }
            if delegation.reward_eligible_stake < 0
                || delegation.reward_eligible_stake > delegation.amount
            {
                return Err(StakingError::InvalidValidatorStake);
            }
            if !self.validators.contains_key(validator_key) {
                return Err(StakingError::ValidatorNotFound);
            }

            let delegated = delegated_by_validator
                .entry(validator_key.clone())
                .or_insert(0);
            *delegated = delegated
                .checked_add(delegation.amount)
                .ok_or(StakingError::StakeAggregateOverflow)?;
            pending_reward_total = pending_reward_total
                .checked_add(delegation.pending_rewards)
                .ok_or(StakingError::RewardAggregateOverflow)?;
        }

        let mut validator_total = 0i64;
        for (operator, validator) in &self.validators {
            let delegated = delegated_by_validator.get(operator).copied().unwrap_or(0);
            let expected_total = validator
                .self_stake
                .checked_add(delegated)
                .ok_or(StakingError::StakeAggregateOverflow)?;
            if validator.total_stake != expected_total {
                return Err(StakingError::ValidatorTotalStakeMismatch);
            }
            validator_total = validator_total
                .checked_add(validator.total_stake)
                .ok_or(StakingError::StakeAggregateOverflow)?;
        }
        if self.total_staked < 0 {
            return Err(StakingError::InvalidTotalStaked);
        }
        if self.total_staked != validator_total {
            return Err(StakingError::TotalStakedMismatch);
        }
        if self.emissions_reserve < 0
            || self.emissions_reserve > super::types::HYCK_GENESIS_EMISSIONS_RESERVE
        {
            return Err(StakingError::InvalidRewardAmount);
        }
        if self.reward_accrual_remainder >= super::types::STAKING_REWARD_YEAR_MS {
            return Err(StakingError::InvalidRewardAmount);
        }

        // Queue amounts have already been removed from delegation and global
        // stake totals by `undelegate`; do not count them a second time.  They
        // still need valid ownership, amounts, and validator references.
        let mut queued_total = 0i64;
        for (delegator, requests) in &self.unstake_queue {
            if requests.is_empty() {
                return Err(StakingError::EmptyUnstakeQueue);
            }
            for request in requests {
                if &request.delegator != delegator {
                    return Err(StakingError::UnstakeDelegatorMismatch);
                }
                if request.amount <= 0 || request.completion_time == 0 {
                    return Err(StakingError::InvalidUnstakeRequest);
                }
                if let Some(validator) = &request.validator {
                    if !self.validators.contains_key(validator) {
                        return Err(StakingError::ValidatorNotFound);
                    }
                }
                queued_total = queued_total
                    .checked_add(request.amount)
                    .ok_or(StakingError::StakeAggregateOverflow)?;
            }
        }

        // `queued_total` is intentionally not compared with current stake:
        // completed undelegations are no longer represented in validator
        // totals, and several requests may legitimately span epochs.
        let _ = queued_total;
        let _ = pending_reward_total;

        if let Some(snapshot) = &self.epoch_snapshot {
            if snapshot.epoch != self.current_epoch || snapshot.total_staked < 0 {
                return Err(StakingError::InvalidEpochSnapshot);
            }
            if snapshot.active_validators.len() > MAX_ACTIVE_VALIDATORS {
                return Err(StakingError::InvalidEpochSnapshot);
            }
            let mut snapshot_nodes = HashSet::with_capacity(snapshot.active_validators.len());
            for node_id in &snapshot.active_validators {
                if !snapshot_nodes.insert(*node_id) || !operators_by_node.contains_key(node_id) {
                    return Err(StakingError::InvalidEpochSnapshot);
                }
            }
            if self.liveness.len() != snapshot_nodes.len()
                || self
                    .liveness
                    .keys()
                    .any(|node_id| !snapshot_nodes.contains(node_id))
                || self.validators.values().any(|validator| {
                    validator.status == ValidatorStatus::Active
                        && !snapshot_nodes.contains(&validator.node_id)
                })
            {
                return Err(StakingError::InvalidLivenessRecord);
            }
        } else if self.current_epoch != 0 {
            return Err(StakingError::InvalidEpochSnapshot);
        } else if !self.liveness.is_empty()
            || self
                .validators
                .values()
                .any(|validator| validator.status == ValidatorStatus::Active)
        {
            return Err(StakingError::InvalidLivenessRecord);
        }

        for (node_id, record) in &self.liveness {
            if !operators_by_node.contains_key(node_id) {
                return Err(StakingError::InvalidLivenessRecord);
            }
            if let Some(snapshot) = &self.epoch_snapshot {
                if !snapshot.active_validators.contains(node_id) {
                    return Err(StakingError::InvalidLivenessRecord);
                }
            } else {
                return Err(StakingError::InvalidLivenessRecord);
            }
            record
                .expected_proposals
                .checked_add(record.expected_votes)
                .ok_or(StakingError::LivenessAggregateOverflow)?;
            record
                .actual_proposals
                .checked_add(record.actual_votes)
                .ok_or(StakingError::LivenessAggregateOverflow)?;
        }

        let mut pending_offenders = HashSet::with_capacity(self.pending_evidence.len());
        for evidence in &self.pending_evidence {
            let Some(operator) = operators_by_node.get(&evidence.offender) else {
                return Err(StakingError::InvalidPendingEvidence);
            };
            let validator = &self.validators[operator];
            if validator.status == ValidatorStatus::Tombstoned
                || !pending_offenders.insert(evidence.offender)
                || !self.validate_evidence_for_validator(evidence, validator)
            {
                return Err(StakingError::InvalidPendingEvidence);
            }
        }

        if let Some(context) = self.consensus_context {
            if context.genesis_hash != self.consensus_genesis_hash {
                return Err(StakingError::ConsensusContextMismatch);
            }
        }

        Ok(())
    }

    fn trusted_chain_domain(&self) -> Option<Hash> {
        if let Some(context) = self.consensus_context {
            if context.has_genesis_domain() {
                return Some(context.genesis_hash);
            }
        }
        (self.consensus_genesis_hash != [0u8; 32]).then_some(self.consensus_genesis_hash)
    }

    fn build_node_index(&self) -> Result<HashMap<NodeId, Address>, StakingError> {
        let mut rebuilt = HashMap::with_capacity(self.validators.len());
        for (operator, validator) in &self.validators {
            if operator != &validator.operator {
                return Err(StakingError::ValidatorOperatorMismatch);
            }
            if rebuilt
                .insert(validator.node_id, operator.clone())
                .is_some()
            {
                return Err(StakingError::DuplicateNodeId);
            }
        }
        Ok(rebuilt)
    }
}

/// Convert a bonded HYCK base-unit balance into the whole-HYCK voting power
/// used by consensus. The conversion is checked and rejects negative or
/// sub-HYCK balances instead of allowing an unchecked integer cast.
pub fn stake_to_voting_power(base_units: i64) -> Result<u64, StakingError> {
    let base_units = u64::try_from(base_units).map_err(|_| StakingError::InvalidValidatorStake)?;
    let voting_power = base_units
        .checked_div(HYCK_BASE_UNITS_PER_HYCK as u64)
        .ok_or(StakingError::InvalidValidatorStake)?;
    if voting_power == 0 {
        return Err(StakingError::InvalidValidatorStake);
    }
    Ok(voting_power)
}

fn static_stake(voting_power: u128) -> Result<i64, StakingError> {
    let stake = voting_power
        .checked_mul(HYCK_BASE_UNITS_PER_HYCK as u128)
        .ok_or(StakingError::InvalidStaticStake)?;
    i64::try_from(stake).map_err(|_| StakingError::InvalidStaticStake)
}

/// Staking errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum StakingError {
    #[error(
        "insufficient self-stake (minimum {} HYCK base units required)",
        MIN_SELF_STAKE
    )]
    InsufficientSelfStake,
    #[error("invalid commission rate")]
    InvalidCommission,
    #[error("invalid validator stake")]
    InvalidValidatorStake,
    #[error("invalid total staked amount")]
    InvalidTotalStaked,
    #[error("validator total stake does not match self-stake and delegations")]
    ValidatorTotalStakeMismatch,
    #[error("total staked amount does not match validator totals")]
    TotalStakedMismatch,
    #[error("invalid staking reward amount")]
    InvalidRewardAmount,
    #[error("staking reward timestamp regressed")]
    RewardTimestampRegression,
    #[error("invalid validator status fields")]
    InvalidValidatorStatus,
    #[error("staking stake aggregate overflows")]
    StakeAggregateOverflow,
    #[error("staking reward aggregate overflows")]
    RewardAggregateOverflow,
    #[error("liveness aggregate overflows")]
    LivenessAggregateOverflow,
    #[error("invalid BLS key (must be 48 bytes)")]
    InvalidBlsKey,
    #[error("invalid BLS proof of possession")]
    InvalidBlsProofOfPossession,
    #[error("BLS public key already registered")]
    BlsKeyAlreadyRegistered,
    #[error("validator already exists")]
    ValidatorAlreadyExists,
    #[error("validator map key does not match validator.operator")]
    ValidatorOperatorMismatch,
    #[error("duplicate validator node ID")]
    DuplicateNodeId,
    #[error("validator node index does not match validators")]
    ValidatorIndexMismatch,
    #[error("node ID already registered")]
    NodeIdAlreadyRegistered,
    #[error("validator not found")]
    ValidatorNotFound,
    #[error("validator is tombstoned")]
    ValidatorTombstoned,
    #[error("invalid amount")]
    InvalidAmount,
    #[error("delegation not found")]
    DelegationNotFound,
    #[error("insufficient delegation")]
    InsufficientDelegation,
    #[error("delegation map key does not match delegation record")]
    DelegationKeyMismatch,
    #[error("unstake queue contains an empty request list")]
    EmptyUnstakeQueue,
    #[error("unstake request delegator does not match queue key")]
    UnstakeDelegatorMismatch,
    #[error("invalid unstake request")]
    InvalidUnstakeRequest,
    #[error("invalid epoch snapshot")]
    InvalidEpochSnapshot,
    #[error("invalid liveness record")]
    InvalidLivenessRecord,
    #[error("invalid pending evidence")]
    InvalidPendingEvidence,
    #[error("invalid authoritative committee: {0}")]
    InvalidAuthoritativeCommittee(String),
    #[error("static committee stake overflows the application balance type")]
    InvalidStaticStake,
    #[error("BLS key rotation is disabled while static committee mode is active")]
    StaticCommitteeKeyRotationDisabled,
    #[error("staking consensus context does not match its trusted genesis domain")]
    ConsensusContextMismatch,
    #[error("validator is jailed")]
    ValidatorJailed,
    #[error("validator not jailed")]
    ValidatorNotJailed,
    #[error("jail period not expired")]
    JailPeriodNotExpired,
    #[error("invalid evidence")]
    InvalidEvidence,
}

fn parse_and_verify_bls_binding(
    bls_pubkey: &[u8],
    bls_proof_of_possession: &[u8],
    chain_domain: [u8; 32],
    node_id: NodeId,
) -> Result<BlsPublicKey, StakingError> {
    if bls_pubkey.len() != 48 {
        return Err(StakingError::InvalidBlsKey);
    }
    let mut public_key_bytes = [0u8; 48];
    public_key_bytes.copy_from_slice(bls_pubkey);
    let public_key =
        BlsPublicKey::from_bytes(&public_key_bytes).map_err(|_| StakingError::InvalidBlsKey)?;
    let proof = BlsProofOfPossession::from_slice(bls_proof_of_possession)
        .map_err(|_| StakingError::InvalidBlsProofOfPossession)?;
    if !public_key.verify_proof_of_possession(&chain_domain, &node_id, &proof) {
        return Err(StakingError::InvalidBlsProofOfPossession);
    }
    Ok(public_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::bls::BlsSecretKey;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    fn test_bls_key(n: u8) -> Vec<u8> {
        let mut seed = [0u8; 32];
        seed[0] = n;
        BlsSecretKey::from_seed(&seed)
            .public_key()
            .to_bytes()
            .to_vec()
    }

    fn test_bls_proof(n: u8, node_id: NodeId, domain: [u8; 32]) -> Vec<u8> {
        let mut seed = [0u8; 32];
        seed[0] = n;
        BlsSecretKey::from_seed(&seed)
            .create_proof_of_possession(&domain, &node_id)
            .to_bytes()
            .to_vec()
    }

    #[test]
    fn test_register_validator() {
        let mut state = StakingState::new();

        // Should succeed with minimum stake
        assert!(state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .is_ok());

        assert_eq!(state.validators.len(), 1);
        assert_eq!(state.total_staked, MIN_SELF_STAKE);

        // Should fail with insufficient stake
        assert!(matches!(
            state.register_validator(
                "bob".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE - 1,
                500,
            ),
            Err(StakingError::InsufficientSelfStake)
        ));
    }

    #[test]
    fn test_delegation() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "validator".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        // Delegate
        state
            .delegate("delegator".into(), "validator".into(), 1000_00)
            .unwrap();

        let val = state.get_validator(&"validator".into()).unwrap();
        assert_eq!(val.total_stake, MIN_SELF_STAKE + 1000_00);
        assert_eq!(state.total_staked, MIN_SELF_STAKE + 1000_00);

        // Undelegate
        state
            .undelegate("delegator".into(), "validator".into(), 500_00, 0)
            .unwrap();

        let val = state.get_validator(&"validator".into()).unwrap();
        assert_eq!(val.total_stake, MIN_SELF_STAKE + 500_00);
    }

    #[test]
    fn clone_isolates_validator_and_delegation_mutations_while_sharing_untouched() {
        let mut parent = StakingState::new();
        let validator = "validator".to_string();
        parent
            .register_validator(
                validator.clone(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        parent
            .delegate("delegator".into(), validator.clone(), 100_00)
            .unwrap();

        let mut child = parent.clone();
        let mut sibling = parent.clone();

        assert!(parent.validators.ptr_eq(&child.validators));
        assert!(parent.delegations.ptr_eq(&sibling.delegations));
        assert!(parent.node_to_operator.ptr_eq(&child.node_to_operator));
        assert!(parent.unstake_queue.ptr_eq(&sibling.unstake_queue));
        assert!(parent.liveness.ptr_eq(&child.liveness));
        assert!(parent.pending_evidence.ptr_eq(&sibling.pending_evidence));

        child.get_validator_mut(&validator).unwrap().pending_rewards += 7;
        sibling
            .delegate("second-delegator".into(), validator.clone(), 50_00)
            .unwrap();

        assert_eq!(parent.get_validator(&validator).unwrap().pending_rewards, 0);
        assert_eq!(child.get_validator(&validator).unwrap().pending_rewards, 7);
        assert_eq!(parent.delegations.len(), 1);
        assert_eq!(child.delegations.len(), 1);
        assert_eq!(sibling.delegations.len(), 2);
        assert_eq!(
            sibling
                .delegations
                .get(&("second-delegator".into(), validator.clone()))
                .unwrap()
                .amount,
            50_00
        );

        assert!(!parent.validators.ptr_eq(&child.validators));
        assert!(!parent.validators.ptr_eq(&sibling.validators));
        assert!(!parent.delegations.ptr_eq(&sibling.delegations));
        assert!(parent.node_to_operator.ptr_eq(&child.node_to_operator));
        assert!(parent.node_to_operator.ptr_eq(&sibling.node_to_operator));
        assert!(parent.unstake_queue.ptr_eq(&child.unstake_queue));
        assert!(parent.liveness.ptr_eq(&sibling.liveness));
        assert!(parent.pending_evidence.ptr_eq(&child.pending_evidence));
    }

    #[test]
    fn test_active_validators_sorted() {
        let mut state = StakingState::new();

        // Register validators with different stakes
        state
            .register_validator(
                "low".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "high".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE * 2,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "mid".into(),
                test_node_id(3),
                test_bls_key(3),
                test_bls_proof(3, test_node_id(3), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE + 100_00,
                500,
            )
            .unwrap();

        let active = state.active_validators();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0], test_node_id(2)); // highest stake
        assert_eq!(active[1], test_node_id(3)); // mid stake
        assert_eq!(active[2], test_node_id(1)); // lowest stake
    }

    #[test]
    fn test_registration_rejects_invalid_proof_and_duplicate_key() {
        let mut state = StakingState::new();
        let domain = [9u8; 32];
        let first_key = test_bls_key(1);
        let first_proof = test_bls_proof(1, test_node_id(1), domain);
        state
            .register_validator(
                "first".into(),
                test_node_id(1),
                first_key.clone(),
                first_proof,
                domain,
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        let mut wrong_domain_proof = test_bls_proof(2, test_node_id(2), domain);
        wrong_domain_proof[0] ^= 1;
        assert!(matches!(
            state.register_validator(
                "bad".into(),
                test_node_id(2),
                test_bls_key(2),
                wrong_domain_proof,
                domain,
                MIN_SELF_STAKE,
                500,
            ),
            Err(StakingError::InvalidBlsProofOfPossession)
        ));

        assert!(matches!(
            state.register_validator(
                "wrong-domain".into(),
                test_node_id(4),
                test_bls_key(4),
                test_bls_proof(4, test_node_id(4), [8u8; 32]),
                domain,
                MIN_SELF_STAKE,
                500,
            ),
            Err(StakingError::InvalidBlsProofOfPossession)
        ));

        assert!(matches!(
            state.register_validator(
                "invalid-curve".into(),
                test_node_id(3),
                vec![0u8; 48],
                vec![0u8; 96],
                domain,
                MIN_SELF_STAKE,
                500,
            ),
            Err(StakingError::InvalidBlsKey)
        ));

        let duplicate_proof = test_bls_proof(1, test_node_id(2), domain);
        assert!(matches!(
            state.register_validator(
                "duplicate".into(),
                test_node_id(2),
                first_key,
                duplicate_proof,
                domain,
                MIN_SELF_STAKE,
                500,
            ),
            Err(StakingError::BlsKeyAlreadyRegistered)
        ));
    }

    #[test]
    fn test_rotate_validator_key_preserves_node_id_until_next_epoch() {
        let mut state = StakingState::new();
        let domain = [7u8; 32];
        state
            .register_validator(
                "first".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), domain),
                domain,
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        let new_key = test_bls_key(2);
        let new_proof = test_bls_proof(2, test_node_id(1), domain);
        state
            .rotate_validator_key(
                &"first".to_string(),
                new_key.clone(),
                new_proof.clone(),
                domain,
            )
            .unwrap();
        let validator = state.get_validator(&"first".to_string()).unwrap();
        assert_eq!(validator.node_id, test_node_id(1));
        assert_eq!(validator.bls_pubkey, new_key);
        assert_eq!(validator.bls_proof_of_possession, new_proof);

        let invalid = state.rotate_validator_key(
            &"first".to_string(),
            test_bls_key(3),
            test_bls_proof(3, test_node_id(2), domain),
            domain,
        );
        assert!(matches!(
            invalid,
            Err(StakingError::InvalidBlsProofOfPossession)
        ));
    }

    #[test]
    fn static_committee_rejects_bls_key_rotation() {
        let node_id = test_node_id(1);
        let mut seed = [0u8; 32];
        seed[0] = 1;
        let secret = BlsSecretKey::from_seed(&seed);
        let config = crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            bls_secret_key: None,
        };
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let mut state = StakingState::new();
        state.set_consensus_context(context);
        state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: "system:genesis:static-rotation".to_string(),
                    node_id,
                    voting_power: 1,
                    bls_pubkey: secret.public_key().to_bytes().to_vec(),
                    bls_proof_of_possession: secret
                        .create_proof_of_possession(&context.genesis_hash, &node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
                context,
            )
            .unwrap();
        state
            .bind_authoritative_committee(committee, context)
            .unwrap();

        let mut replacement_seed = [0u8; 32];
        replacement_seed[0] = 2;
        let replacement = BlsSecretKey::from_seed(&replacement_seed);
        let error = state.rotate_validator_key(
            &"system:genesis:static-rotation".to_string(),
            replacement.public_key().to_bytes().to_vec(),
            replacement
                .create_proof_of_possession(&context.genesis_hash, &node_id)
                .to_bytes()
                .to_vec(),
            context.genesis_hash,
        );

        assert!(matches!(
            error,
            Err(StakingError::StaticCommitteeKeyRotationDisabled)
        ));
    }

    #[test]
    fn static_committee_binding_rejects_missing_member_record() {
        let node_id = test_node_id(1);
        let config = crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![test_bls_key(1)],
            bls_secret_key: None,
        };
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let mut state = StakingState::new();
        state.set_consensus_context(context);
        let mut snapshot = EpochSnapshot::new(0, 0, 0);
        snapshot.active_validators = vec![node_id];
        state.epoch_snapshot = Some(snapshot);

        let error = state
            .bind_authoritative_committee(committee, context)
            .expect_err("every static member must have a staking record");
        assert!(matches!(
            error,
            StakingError::InvalidAuthoritativeCommittee(message)
                if message.contains("no registered staking record")
        ));
    }

    #[test]
    fn static_committee_binding_rejects_nonzero_staking_epoch() {
        let node_id = test_node_id(1);
        let secret = BlsSecretKey::from_seed(&[1u8; 32]);
        let config = crate::types::ConsensusConfig {
            epoch: 0,
            genesis_hash: [9u8; 32],
            node_id,
            validators: vec![node_id],
            voting_powers: vec![1],
            view_timeout_ms: 3000,
            bls_pubkeys: vec![secret.public_key().to_bytes().to_vec()],
            bls_secret_key: None,
        };
        let committee = config.committee().unwrap();
        let context = config.context().unwrap();
        let mut state = StakingState::new();
        state.set_consensus_context(context);
        state
            .bootstrap_static_committee(
                &committee,
                &[StaticValidatorBootstrap {
                    operator: "system:genesis:epoch-check".to_string(),
                    node_id,
                    voting_power: 1,
                    bls_pubkey: secret.public_key().to_bytes().to_vec(),
                    bls_proof_of_possession: secret
                        .create_proof_of_possession(&context.genesis_hash, &node_id)
                        .to_bytes()
                        .to_vec(),
                    self_stake: MIN_SELF_STAKE,
                    commission_bps: 0,
                }],
                context,
            )
            .unwrap();
        state.current_epoch = 1;

        let error = state
            .bind_authoritative_committee(committee, context)
            .expect_err("static epoch-0 committee cannot bind to a later epoch");
        assert!(matches!(
            error,
            StakingError::InvalidAuthoritativeCommittee(message)
                if message.contains("state at epoch 0")
        ));
    }

    #[test]
    fn consensus_update_uses_whole_hyck_voting_power_not_base_units() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "one".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                0,
            )
            .unwrap();
        state
            .register_validator(
                "two".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE * 2,
                0,
            )
            .unwrap();

        let update = state.active_validator_set_for_consensus();
        assert_eq!(
            update.stakes,
            vec![(test_node_id(2), 2), (test_node_id(1), 1)]
        );
    }

    #[test]
    fn rebuild_index_repairs_and_validates_atomically() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "bob".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        state.node_to_operator.clear();
        assert!(state.validate_invariants().is_err());
        state.rebuild_index().unwrap();
        assert!(state.validate_invariants().is_ok());

        let before = state.node_to_operator.clone();
        state.validators.get_mut("alice").unwrap().operator = "not-alice".into();
        assert!(matches!(
            state.rebuild_index(),
            Err(StakingError::ValidatorOperatorMismatch)
        ));
        assert_eq!(state.node_to_operator, before);
    }

    #[test]
    fn rebuild_index_rejects_duplicate_node_ids_without_partial_mutation() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "bob".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        let before = state.node_to_operator.clone();
        state.validators.get_mut("bob").unwrap().node_id = test_node_id(1);
        assert!(matches!(
            state.rebuild_index(),
            Err(StakingError::DuplicateNodeId)
        ));
        assert_eq!(state.node_to_operator, before);
    }

    #[test]
    fn validate_primary_state_accepts_current_stake_and_unbonding_flow() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .delegate("delegator".into(), "alice".into(), 50_000)
            .unwrap();
        state.transition_epoch(0, 1_000);
        state
            .undelegate("delegator".into(), "alice".into(), 25_000, 2_000)
            .unwrap();

        assert!(state.validate_primary_state().is_ok());

        // The primary check deliberately does not require a fresh transient
        // node index; the separate derived-index check still reports it.
        state.node_to_operator.clear();
        assert!(state.validate_primary_state().is_ok());
        assert!(matches!(
            state.validate_invariants(),
            Err(StakingError::ValidatorIndexMismatch)
        ));
    }

    #[test]
    fn validate_primary_state_rejects_corrupt_totals_and_duplicate_keys() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .register_validator(
                "bob".into(),
                test_node_id(2),
                test_bls_key(2),
                test_bls_proof(2, test_node_id(2), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        state.total_staked += 1;
        assert!(matches!(
            state.validate_primary_state(),
            Err(StakingError::TotalStakedMismatch)
        ));
        state.total_staked -= 1;

        let alice_key = state.validators["alice"].bls_pubkey.clone();
        state.validators.get_mut("bob").unwrap().bls_pubkey = alice_key;
        assert!(matches!(
            state.validate_primary_state(),
            Err(StakingError::BlsKeyAlreadyRegistered)
        ));
    }

    #[test]
    fn validate_primary_state_rejects_excess_validator_reward_eligibility() {
        let mut state = StakingState::new();
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), [0u8; 32]),
                [0u8; 32],
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        state
            .validators
            .get_mut("alice")
            .unwrap()
            .reward_eligible_stake = MIN_SELF_STAKE + 1;

        assert!(matches!(
            state.validate_primary_state(),
            Err(StakingError::InvalidValidatorStake)
        ));
    }

    #[test]
    fn validate_primary_state_checks_trusted_pop_and_record_keys() {
        let domain = [7u8; 32];
        let mut state = StakingState::new();
        state.set_consensus_genesis_hash(domain);
        state
            .register_validator(
                "alice".into(),
                test_node_id(1),
                test_bls_key(1),
                test_bls_proof(1, test_node_id(1), domain),
                domain,
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();
        assert!(state.validate_primary_state().is_ok());

        state
            .validators
            .get_mut("alice")
            .unwrap()
            .bls_proof_of_possession = test_bls_proof(1, test_node_id(1), [8u8; 32]);
        assert!(matches!(
            state.validate_primary_state(),
            Err(StakingError::InvalidBlsProofOfPossession)
        ));

        state
            .validators
            .get_mut("alice")
            .unwrap()
            .bls_proof_of_possession = test_bls_proof(1, test_node_id(1), domain);
        state.delegations.insert(
            ("delegator".into(), "alice".into()),
            Delegation::new("different-delegator".into(), "alice".into(), 1),
        );
        assert!(matches!(
            state.validate_primary_state(),
            Err(StakingError::DelegationKeyMismatch)
        ));
    }

    #[test]
    fn validate_primary_state_does_not_trust_the_transient_index_for_evidence() {
        let domain = [9u8; 32];
        let node_id = test_node_id(1);
        let mut seed = [0u8; 32];
        seed[0] = 1;
        let secret = BlsSecretKey::from_seed(&seed);
        let mut state = StakingState::new();
        state.set_consensus_genesis_hash(domain);
        state
            .register_validator(
                "alice".into(),
                node_id,
                secret.public_key().to_bytes().to_vec(),
                test_bls_proof(1, node_id, domain),
                domain,
                MIN_SELF_STAKE,
                500,
            )
            .unwrap();

        let context = state.static_consensus_context().unwrap();
        let view = 7;
        let hash_a = [1u8; 32];
        let hash_b = [2u8; 32];
        let app_hash_a = [3u8; 32];
        let app_hash_b = [4u8; 32];
        let signature_a = secret
            .sign(&crate::types::Certificate::build_signing_message(
                context,
                view,
                &hash_a,
                &app_hash_a,
            ))
            .to_bytes()
            .to_vec();
        let signature_b = secret
            .sign(&crate::types::Certificate::build_signing_message(
                context,
                view,
                &hash_b,
                &app_hash_b,
            ))
            .to_bytes()
            .to_vec();
        state.pending_evidence.push(Evidence {
            evidence_type: crate::app::staking::EvidenceType::DoubleVote,
            offender: node_id,
            view,
            timestamp: 1,
            context,
            hash_a,
            app_hash_a,
            hash_b,
            app_hash_b,
            signature_a,
            signature_b,
        });

        state.node_to_operator.clear();
        state.validate_primary_state().unwrap();
    }
}
