//! Staking Types
//!
//! Core data structures for the Proof-of-Stake staking system.

use serde::{Deserialize, Serialize};

use crate::app::Address;
use crate::types::{ConsensusContext, Hash, NodeId};

/// Trusted static-epoch validator material supplied by node genesis.
///
/// This is runtime bootstrap input, not an application transaction.  The
/// proof-of-possession is retained verbatim and is re-verified when the
/// deterministic staking record is created; callers must not replace it with
/// an empty or synthetic proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticValidatorBootstrap {
    pub operator: Address,
    pub node_id: NodeId,
    pub voting_power: u128,
    pub bls_pubkey: Vec<u8>,
    pub bls_proof_of_possession: Vec<u8>,
    pub self_stake: i64,
    pub commission_bps: i64,
}

/// Validator status in the staking system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidatorStatus {
    /// Validator is active and participating in consensus
    Active,
    /// Validator is registered but not in active set (insufficient stake)
    Inactive,
    /// Validator is temporarily jailed for downtime
    Jailed,
    /// Validator is permanently banned for equivocation (slashed)
    Tombstoned,
}

impl Default for ValidatorStatus {
    fn default() -> Self {
        Self::Inactive
    }
}

/// Information about a registered validator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorInfo {
    /// Operator address (controls validator)
    pub operator: Address,
    /// Consensus identity (32 bytes)
    pub node_id: NodeId,
    /// BLS public key (48 bytes)
    pub bls_pubkey: Vec<u8>,
    /// Verified proof that the operator controls `bls_pubkey` for `node_id`.
    pub bls_proof_of_possession: Vec<u8>,
    /// Self-stake amount in HYCK base units (1 HYCK = 1_000_000 base units).
    pub self_stake: i64,
    /// Total stake including delegations, in HYCK base units.
    pub total_stake: i64,
    /// Commission rate in basis points (0-1000 = 0-10%)
    pub commission_bps: i64,
    /// Current status
    pub status: ValidatorStatus,
    /// Accumulated pending rewards in HYCK base units.
    pub pending_rewards: i64,
    /// Stake eligible for the current reward epoch.  New stake enters at the
    /// next epoch boundary and a decrease lowers this value immediately.
    #[serde(default)]
    pub reward_eligible_stake: i64,
    /// Time when jail period ends (ms timestamp, 0 if not jailed)
    pub jail_until: u64,
    /// Consecutive missed blocks/votes
    pub missed_consecutive: u64,
    /// Total blocks proposed in current epoch
    pub blocks_proposed: u64,
    /// Total votes cast in current epoch
    pub votes_cast: u64,
}

impl ValidatorInfo {
    /// Create a new validator
    pub fn new(
        operator: Address,
        node_id: NodeId,
        bls_pubkey: Vec<u8>,
        bls_proof_of_possession: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    ) -> Self {
        Self {
            operator,
            node_id,
            bls_pubkey,
            bls_proof_of_possession,
            self_stake,
            total_stake: self_stake,
            commission_bps,
            status: ValidatorStatus::Inactive,
            pending_rewards: 0,
            reward_eligible_stake: self_stake,
            jail_until: 0,
            missed_consecutive: 0,
            blocks_proposed: 0,
            votes_cast: 0,
        }
    }

    /// Check if validator can be in active set
    pub fn can_be_active(&self) -> bool {
        matches!(
            self.status,
            ValidatorStatus::Active | ValidatorStatus::Inactive
        ) && self.self_stake >= MIN_SELF_STAKE
    }

    /// Reset epoch-specific counters
    pub fn reset_epoch_counters(&mut self) {
        self.blocks_proposed = 0;
        self.votes_cast = 0;
        self.missed_consecutive = 0;
    }
}

/// Delegation from a delegator to a validator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delegation {
    /// Delegator address
    pub delegator: Address,
    /// Validator operator address
    pub validator: Address,
    /// Delegated amount in HYCK base units.
    pub amount: i64,
    /// Pending rewards in HYCK base units.
    pub pending_rewards: i64,
    /// Amount eligible for the current reward epoch.  This prevents a stake
    /// added immediately before a boundary from receiving past-epoch yield.
    #[serde(default)]
    pub reward_eligible_stake: i64,
}

impl Delegation {
    pub fn new(delegator: Address, validator: Address, amount: i64) -> Self {
        Self {
            delegator,
            validator,
            amount,
            pending_rewards: 0,
            reward_eligible_stake: amount,
        }
    }
}

/// Unstake request (enters 7-day queue)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnstakeRequest {
    /// Delegator address
    pub delegator: Address,
    /// Validator address (for delegation unstake) or None (for self-stake)
    pub validator: Option<Address>,
    /// Amount to unstake in HYCK base units.
    pub amount: i64,
    /// Time when unstake completes (ms timestamp)
    pub completion_time: u64,
}

/// Evidence of misbehavior (equivocation)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// Type of evidence
    pub evidence_type: EvidenceType,
    /// Offending validator's node ID
    pub offender: NodeId,
    /// Consensus view where the offense occurred.
    pub view: u64,
    /// Timestamp of evidence submission
    pub timestamp: u64,
    /// Consensus authentication context in which both signed messages were
    /// created.  Evidence is never verified against a context reconstructed
    /// from attacker-controlled fields alone.
    pub context: ConsensusContext,
    /// First conflicting vote/block hash
    pub hash_a: [u8; 32],
    /// Application state hash carried by the first vote.
    pub app_hash_a: Hash,
    /// Second conflicting vote/block hash
    pub hash_b: [u8; 32],
    /// Application state hash carried by the second vote.
    pub app_hash_b: Hash,
    /// Signature on first item
    pub signature_a: Vec<u8>,
    /// Signature on second item
    pub signature_b: Vec<u8>,
}

/// Type of misbehavior evidence
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceType {
    /// Validator voted for two different blocks at the same height
    DoubleVote,
    /// Validator proposed two different blocks at the same height
    DoublePropose,
}

/// Snapshot of validator set at epoch start
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochSnapshot {
    /// Epoch number
    pub epoch: u64,
    /// Starting view of this epoch
    pub start_view: u64,
    /// Active validators at epoch start (sorted by stake descending)
    pub active_validators: Vec<NodeId>,
    /// Total staked across all validators
    pub total_staked: i64,
    /// Timestamp when epoch started
    pub timestamp: u64,
}

impl EpochSnapshot {
    pub fn new(epoch: u64, start_view: u64, timestamp: u64) -> Self {
        Self {
            epoch,
            start_view,
            active_validators: Vec::new(),
            total_staked: 0,
            timestamp,
        }
    }
}

/// Record of validator liveness for jailing decisions
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LivenessRecord {
    /// Blocks that should have been proposed
    pub expected_proposals: u64,
    /// Blocks actually proposed
    pub actual_proposals: u64,
    /// Votes expected (all blocks in epoch)
    pub expected_votes: u64,
    /// Votes actually cast
    pub actual_votes: u64,
}

impl LivenessRecord {
    /// Calculate liveness percentage in basis points (10000 = 100%)
    pub fn liveness_bps(&self) -> i64 {
        let expected = self.expected_proposals + self.expected_votes;
        if expected == 0 {
            return 10000; // 100% if no expectations
        }
        let actual = self.actual_proposals + self.actual_votes;
        ((actual as i128 * 10000) / expected as i128) as i64
    }
}

// =============================================================================
// Constants
// =============================================================================

/// Blocks per epoch (~90 min at 100ms blocks)
pub const ROUNDS_PER_EPOCH: u64 = 54_000;

/// Number of application base units in one whole HYCK.
pub const HYCK_BASE_UNITS_PER_HYCK: i64 = 1_000_000;

/// Fixed native HYCK issuance for the showcase/testnet genesis.
pub const HYCK_TOTAL_SUPPLY_HYCK: i64 = 1_000_000_000;
/// Fixed native HYCK issuance in base units (1e15).
pub const HYCK_TOTAL_SUPPLY: i64 = HYCK_TOTAL_SUPPLY_HYCK * HYCK_BASE_UNITS_PER_HYCK;
/// Portion of the fixed genesis supply reserved for future staking emissions.
///
/// The reserve is a balance bucket, not an issuance permission.  A node must
/// fund it from an existing native-HYCK account before rewards can consume it.
pub const HYCK_GENESIS_EMISSIONS_RESERVE_HYCK: i64 = 388_880_000;
/// Genesis staking-emissions reserve in HYCK base units.
pub const HYCK_GENESIS_EMISSIONS_RESERVE: i64 =
    HYCK_GENESIS_EMISSIONS_RESERVE_HYCK * HYCK_BASE_UNITS_PER_HYCK;
/// Alias spelling retained for callers that make the unit explicit.
pub const HYCK_GENESIS_EMISSIONS_RESERVE_BASE_UNITS: i64 = HYCK_GENESIS_EMISSIONS_RESERVE;

/// Annual staking reward rate at the reference stake, in basis points.
pub const STAKING_REWARD_APY_BPS: u64 = 237; // 2.37%
/// Reference aggregate stake for the inverse-square-root reward curve.
pub const STAKING_REWARD_ANCHOR_STAKE_HYCK: i64 = 400_000_000;
/// Reference aggregate stake in HYCK base units.
pub const STAKING_REWARD_ANCHOR_STAKE: i64 =
    STAKING_REWARD_ANCHOR_STAKE_HYCK * HYCK_BASE_UNITS_PER_HYCK;
/// Basis-point denominator.
pub const STAKING_REWARD_BPS_DENOMINATOR: u64 = 10_000;
/// Milliseconds in the deterministic 365-day reward year.
pub const STAKING_REWARD_YEAR_MS: u64 = 365 * 24 * 60 * 60 * 1_000;
/// Reward accounting follows the existing 90-minute epoch cadence.
pub const STAKING_REWARD_EPOCH_MS: u64 = ROUNDS_PER_EPOCH * 100;
/// Pending rewards are periodically compounded into active stake.
pub const STAKING_AUTO_COMPOUND_INTERVAL_MS: u64 = 24 * 60 * 60 * 1_000;
/// Canonical account receiving the unallocated genesis reserve and slash
/// proceeds.  It is an ordinary native-liquid account, never perp collateral.
pub const HYCK_TREASURY_ADDRESS: &str = "system:treasury";

/// Minimum self-stake is one whole HYCK in the current local model.
pub const MIN_SELF_STAKE: i64 = HYCK_BASE_UNITS_PER_HYCK;

/// Maximum validators in active set.  Keep this aligned with the consensus
/// committee limit so genesis, consensus, and staking cannot disagree.
pub const MAX_ACTIVE_VALIDATORS: usize = crate::types::MAX_COMMITTEE_MEMBERS;

/// Unstake delay (7 days in ms)
pub const UNSTAKE_DELAY_MS: u64 = 604_800_000;

/// Slash percentage for equivocation (50% = 5000 bps)
pub const EQUIVOCATION_SLASH_BPS: i64 = 5000;

/// Minimum liveness to avoid jailing (90% = 9000 bps)
pub const MIN_LIVENESS_BPS: i64 = 9000;

/// Jail duration (1 hour in ms)
pub const JAIL_DURATION_MS: u64 = 3_600_000;

/// Max consecutive missed blocks before auto-jail
pub const MAX_CONSECUTIVE_MISSED: u64 = 100;

/// Maximum commission rate (10% = 1000 bps)
pub const MAX_COMMISSION_BPS: i64 = 1000;

// =============================================================================
// Validator Set Update (for consensus integration)
// =============================================================================

/// Update to validator set for consensus layer
///
/// This is returned by `StakingState::active_validator_set_for_consensus()`
/// and used to update the consensus configuration on epoch transitions.
#[derive(Debug, Clone)]
pub struct ValidatorSetUpdate {
    /// Active validator node IDs (sorted by stake descending)
    pub node_ids: Vec<NodeId>,
    /// BLS public keys (same order as node_ids)
    pub bls_pubkeys: Vec<Vec<u8>>,
    /// Whole-HYCK voting powers for weighted leader selection.
    pub stakes: Vec<(NodeId, u64)>,
}

impl ValidatorSetUpdate {
    /// Check if the update is empty (no validators)
    pub fn is_empty(&self) -> bool {
        self.node_ids.is_empty()
    }

    /// Number of validators
    pub fn len(&self) -> usize {
        self.node_ids.len()
    }
}
