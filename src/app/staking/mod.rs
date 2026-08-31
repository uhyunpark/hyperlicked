//! Staking Module
//!
//! Implements Proof-of-Stake for validator selection and economic security.
//!
//! ## Features
//!
//! - **Top-21 validators** by stake
//! - **Epoch-based rotation** (~90 min epochs)
//! - **Slashing** for equivocation (double-voting)
//! - **Jailing** for downtime (missed blocks/votes)
//! - **Delegation** with 7-day unbonding
//! - **Reserve-backed rewards** distributed proportionally by stake
//!
//! ## Single-Node Compatibility
//!
//! When running a single validator:
//! - Staking transactions still work (for testing)
//! - Single validator is always in active set
//! - No slashing (can't detect equivocation with 1 node)
//! - No jailing (single node can't have meaningful downtime)
//! - The static consensus committee remains at epoch 0 until authenticated
//!   committee-transition activation is enabled
//!
//! Slashing and jailing currently update application stake/status. In the
//! curated static phase they do not yet remove consensus voting power; that
//! requires the authenticated committee-transition path.
//!
//! ## Architecture
//!
//! ```text
//! StakingState
//! ├── validators: HashMap<Address, ValidatorInfo>
//! ├── delegations: HashMap<(Address, Address), Delegation>
//! ├── unstake_queue: HashMap<Address, Vec<UnstakeRequest>>
//! ├── epoch_snapshot: Option<EpochSnapshot>
//! └── liveness: HashMap<NodeId, LivenessRecord>
//! ```

pub mod epoch;
pub mod jailing;
pub mod rewards;
pub mod slashing;
pub mod state;
pub mod transactions;
pub mod types;

pub use epoch::EpochTransitionResult;
pub use jailing::JailInfo;
pub use slashing::SlashResult;
pub use state::{stake_to_voting_power, StakingError, StakingState};
pub use transactions::{StakingTransaction, StakingTxResult};
pub use types::{
    Delegation,
    EpochSnapshot,
    Evidence,
    EvidenceType,
    LivenessRecord,
    StaticValidatorBootstrap,
    UnstakeRequest,
    ValidatorInfo,
    ValidatorSetUpdate,
    ValidatorStatus,
    // Constants
    EQUIVOCATION_SLASH_BPS,
    HYCK_BASE_UNITS_PER_HYCK,
    HYCK_GENESIS_EMISSIONS_RESERVE,
    HYCK_GENESIS_EMISSIONS_RESERVE_HYCK,
    HYCK_TOTAL_SUPPLY,
    HYCK_TOTAL_SUPPLY_HYCK,
    HYCK_TREASURY_ADDRESS,
    JAIL_DURATION_MS,
    MAX_ACTIVE_VALIDATORS,
    MAX_COMMISSION_BPS,
    MAX_CONSECUTIVE_MISSED,
    MIN_LIVENESS_BPS,
    MIN_SELF_STAKE,
    ROUNDS_PER_EPOCH,
    STAKING_AUTO_COMPOUND_INTERVAL_MS,
    STAKING_REWARD_ANCHOR_STAKE,
    STAKING_REWARD_APY_BPS,
    STAKING_REWARD_EPOCH_MS,
    STAKING_REWARD_YEAR_MS,
    UNSTAKE_DELAY_MS,
};
