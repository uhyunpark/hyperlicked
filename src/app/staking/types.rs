//! Staking Types
//!
//! Core data structures for the Proof-of-Stake staking system.

use serde::{Deserialize, Serialize};

use crate::app::Address;
use crate::types::NodeId;

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
    /// Self-stake amount (in cents, min $10,000 = 1_000_000)
    pub self_stake: i64,
    /// Total stake including delegations
    pub total_stake: i64,
    /// Commission rate in basis points (0-1000 = 0-10%)
    pub commission_bps: i64,
    /// Current status
    pub status: ValidatorStatus,
    /// Accumulated pending rewards (in cents)
    pub pending_rewards: i64,
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
        self_stake: i64,
        commission_bps: i64,
    ) -> Self {
        Self {
            operator,
            node_id,
            bls_pubkey,
            self_stake,
            total_stake: self_stake,
            commission_bps,
            status: ValidatorStatus::Inactive,
            pending_rewards: 0,
            jail_until: 0,
            missed_consecutive: 0,
            blocks_proposed: 0,
            votes_cast: 0,
        }
    }

    /// Check if validator can be in active set
    pub fn can_be_active(&self) -> bool {
        matches!(self.status, ValidatorStatus::Active | ValidatorStatus::Inactive)
            && self.self_stake >= MIN_SELF_STAKE
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
    /// Delegated amount (in cents)
    pub amount: i64,
    /// Pending rewards (in cents)
    pub pending_rewards: i64,
}

impl Delegation {
    pub fn new(delegator: Address, validator: Address, amount: i64) -> Self {
        Self {
            delegator,
            validator,
            amount,
            pending_rewards: 0,
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
    /// Amount to unstake (in cents)
    pub amount: i64,
    /// Time when unstake completes (ms timestamp)
    pub completion_time: u64,
}

/// Evidence of misbehavior (equivocation)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// Type of evidence
    pub evidence_type: EvidenceType,
    /// Offending validator's node ID
    pub offender: NodeId,
    /// Block height where offense occurred
    pub height: u64,
    /// Timestamp of evidence submission
    pub timestamp: u64,
    /// First conflicting vote/block hash
    pub hash_a: [u8; 32],
    /// Second conflicting vote/block hash
    pub hash_b: [u8; 32],
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

/// Maximum validators in active set
pub const MAX_ACTIVE_VALIDATORS: usize = 21;

/// Minimum self-stake to register as validator ($10,000 in cents)
/// 10,000 USD * 100 cents/USD = 1,000,000 cents
pub const MIN_SELF_STAKE: i64 = 1_000_000;

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

/// Block reward per block (in cents, $0.01 = 1 cent)
pub const BLOCK_REWARD: i64 = 1;
