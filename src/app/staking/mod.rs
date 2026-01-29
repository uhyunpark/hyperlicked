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
//! - **Rewards** distributed proportionally by stake
//!
//! ## Single-Node Compatibility
//!
//! When running a single validator:
//! - Staking transactions still work (for testing)
//! - Single validator is always in active set
//! - No slashing (can't detect equivocation with 1 node)
//! - No jailing (single node can't have meaningful downtime)
//! - Epoch transitions still occur for consistency
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
pub use state::{StakingError, StakingState};
pub use transactions::{StakingTransaction, StakingTxResult};
pub use types::{
    Delegation, EpochSnapshot, Evidence, EvidenceType, LivenessRecord, UnstakeRequest,
    ValidatorInfo, ValidatorSetUpdate, ValidatorStatus,
    // Constants
    BLOCK_REWARD, EQUIVOCATION_SLASH_BPS, JAIL_DURATION_MS, MAX_ACTIVE_VALIDATORS,
    MAX_COMMISSION_BPS, MAX_CONSECUTIVE_MISSED, MIN_LIVENESS_BPS, MIN_SELF_STAKE,
    ROUNDS_PER_EPOCH, UNSTAKE_DELAY_MS,
};
