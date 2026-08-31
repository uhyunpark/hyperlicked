//! App State Snapshots
//!
//! Serializable snapshots of application state for persistence.
//!
//! ## What's Snapshotted
//! - Account balances and positions
//! - Market configurations
//! - Mark prices
//!
//! ## What's NOT Snapshotted (rebuilt from block replay)
//! - Orderbook open orders
//! - Mempool pending transactions
//! - Trade history (recent trades)

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::app::accounts::MAX_NONCE_GAP;
use crate::app::staking::{
    HYCK_GENESIS_EMISSIONS_RESERVE, HYCK_TOTAL_SUPPLY, MAX_ACTIVE_VALIDATORS,
    STAKING_REWARD_YEAR_MS,
};
use crate::app::trigger::TriggerOrder;
use crate::app::{Account, MarketConfig, OracleState, StakingState, Symbol};
use crate::types::Price;

/// Hard upper bound for the JSON representation of one application snapshot.
///
/// Snapshots are currently a single JSON object rather than a chunked
/// manifest.  Keep the complete value bounded before handing it to serde so
/// storage/import callers cannot turn a peer or a corrupt local record into an
/// unbounded allocation.  This limit is a transport/storage guard, not an
/// application-state size target; chunked snapshots can replace it in a later
/// protocol tranche.
pub const MAX_APP_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

/// Maximum number of pending nonce markers carried by one account.
pub const MAX_SNAPSHOT_PENDING_NONCES_PER_ACCOUNT: usize = MAX_NONCE_GAP as usize;

/// Serializable app state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSnapshot {
    /// Block height at snapshot
    pub height: u64,
    /// Block timestamp at snapshot
    pub timestamp: u64,
    /// All accounts with balances and positions
    pub accounts: Vec<Account>,
    /// Market configurations
    pub market_configs: Vec<MarketConfig>,
    /// Mark prices per symbol
    pub mark_prices: Vec<(Symbol, Price)>,
    /// Insurance fund balance (in cents)
    #[serde(default)]
    pub insurance_fund: i64,
    /// Current funding rates per symbol (in bps)
    #[serde(default)]
    pub funding_rates: Vec<(Symbol, i64)>,
    /// Last funding payment times per symbol (ms timestamp)
    #[serde(default)]
    pub last_funding_times: Vec<(Symbol, u64)>,
    /// Staking state (validators, delegations, epochs)
    #[serde(default)]
    pub staking: Option<StakingState>,
    /// Oracle state (price feeds)
    #[serde(default)]
    pub oracle: Option<OracleState>,
    /// Trigger orders (TP/SL)
    #[serde(default)]
    pub trigger_orders: Vec<TriggerOrder>,
    /// Premium samples per symbol (for funding rate calculation)
    #[serde(default)]
    pub premium_samples: Vec<(Symbol, Vec<i64>)>,
    /// Trigger order sequence number
    #[serde(default)]
    pub trigger_seq: u64,
    /// Mark price EMA per symbol
    #[serde(default)]
    pub mark_price_ema: Vec<(Symbol, Price)>,
}

impl AppSnapshot {
    /// Create empty genesis snapshot
    pub fn genesis() -> Self {
        let mut treasury = Account::new(crate::app::staking::HYCK_TREASURY_ADDRESS);
        treasury.hyck_balance = HYCK_TOTAL_SUPPLY - HYCK_GENESIS_EMISSIONS_RESERVE;
        let mut staking = StakingState::new();
        staking
            .initialize_genesis_emissions_reserve()
            .expect("canonical genesis emissions reserve is valid");
        Self {
            height: 0,
            timestamp: 0,
            accounts: vec![treasury],
            market_configs: vec![MarketConfig::default()],
            mark_prices: vec![("BTC-USDT".to_string(), 5_000_000)],
            insurance_fund: 0,
            funding_rates: Vec::new(),
            last_funding_times: Vec::new(),
            staking: Some(staking),
            oracle: None,
            trigger_orders: Vec::new(),
            premium_samples: Vec::new(),
            trigger_seq: 0,
            mark_price_ema: Vec::new(),
        }
    }

    /// Reject a serialized snapshot before deserializing it.
    pub fn validate_serialized_size(len: usize) -> Result<(), String> {
        if len > MAX_APP_SNAPSHOT_BYTES {
            return Err(format!(
                "serialized app snapshot is too large: {len} bytes (maximum {MAX_APP_SNAPSHOT_BYTES})"
            ));
        }
        Ok(())
    }

    /// Serialize a snapshot only after checking its resource bounds.
    pub fn to_bounded_json(&self) -> Result<Vec<u8>, String> {
        self.validate_resource_limits()?;
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        Self::validate_serialized_size(bytes.len())?;
        Ok(bytes)
    }

    /// Deserialize a snapshot only after checking its raw byte bound, then
    /// validate all decoded cardinalities before returning it to the caller.
    ///
    /// JSON decoding may still allocate up to a bounded multiple of the raw
    /// input before field cardinalities are known. A future trusted fast-sync
    /// protocol therefore needs a chunked manifest/streaming decoder rather
    /// than raising this single-object limit.
    pub fn from_bounded_json(bytes: &[u8]) -> Result<Self, String> {
        Self::validate_serialized_size(bytes.len())?;
        let snapshot: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        snapshot.validate_resource_limits()?;
        Ok(snapshot)
    }

    /// Validate snapshot cardinalities without mutating any application
    /// state.  Semantic checks (references, hashes, signatures, and derived
    /// indexes) remain the responsibility of the application import path.
    pub fn validate_resource_limits(&self) -> Result<(), String> {
        fn check(label: &str, actual: usize, maximum: usize) -> Result<(), String> {
            if actual > maximum {
                return Err(format!(
                    "snapshot {label} count {actual} exceeds maximum {maximum}"
                ));
            }
            Ok(())
        }

        let mut liquid_hyck = 0i128;
        for account in &self.accounts {
            check(
                "account pending nonces",
                account.pending_nonces.len(),
                MAX_SNAPSHOT_PENDING_NONCES_PER_ACCOUNT,
            )?;
            if account.hyck_balance < 0 {
                return Err(format!(
                    "snapshot account {} has a negative native HYCK balance",
                    account.address
                ));
            }
            liquid_hyck = liquid_hyck
                .checked_add(i128::from(account.hyck_balance))
                .ok_or_else(|| "snapshot native HYCK liquid supply overflows".to_string())?;
            if liquid_hyck > i128::from(HYCK_TOTAL_SUPPLY) {
                return Err("snapshot native HYCK liquid supply exceeds fixed issuance".to_string());
            }
        }

        let Some(staking) = &self.staking else {
            return Err("snapshot is missing mandatory staking state".to_string());
        };

        if staking.emissions_reserve < 0
            || staking.emissions_reserve > HYCK_GENESIS_EMISSIONS_RESERVE
        {
            return Err("snapshot staking emissions reserve is out of range".to_string());
        }
        if staking.reward_accrual_remainder >= STAKING_REWARD_YEAR_MS {
            return Err("snapshot staking reward remainder is out of range".to_string());
        }

        check(
            "staking liveness records",
            staking.liveness.len(),
            MAX_ACTIVE_VALIDATORS,
        )?;
        if let Some(epoch_snapshot) = &staking.epoch_snapshot {
            check(
                "staking active validators",
                epoch_snapshot.active_validators.len(),
                MAX_ACTIVE_VALIDATORS,
            )?;
        }

        Ok(())
    }

    /// Convert mark_prices vec to HashMap for use
    pub fn mark_prices_map(&self) -> HashMap<Symbol, Price> {
        self.mark_prices.iter().cloned().collect()
    }

    /// Convert funding_rates vec to HashMap for use
    pub fn funding_rates_map(&self) -> HashMap<Symbol, i64> {
        self.funding_rates.iter().cloned().collect()
    }

    /// Convert last_funding_times vec to HashMap for use
    pub fn last_funding_times_map(&self) -> HashMap<Symbol, u64> {
        self.last_funding_times.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_size_limit_accepts_exact_boundary_only() {
        assert!(AppSnapshot::validate_serialized_size(MAX_APP_SNAPSHOT_BYTES).is_ok());
        assert!(AppSnapshot::validate_serialized_size(MAX_APP_SNAPSHOT_BYTES + 1).is_err());
    }

    #[test]
    fn resource_limits_reject_oversized_record_vectors() {
        let mut snapshot = AppSnapshot::genesis();
        let mut staking = StakingState::new();
        staking.liveness = (0..=MAX_ACTIVE_VALIDATORS)
            .map(|index| ([index as u8; 32], Default::default()))
            .collect();
        snapshot.staking = Some(staking);

        let error = snapshot
            .validate_resource_limits()
            .expect_err("oversized liveness vector must fail closed");
        assert!(error.contains("liveness"));
    }

    #[test]
    fn resource_limits_reject_oversized_nested_records() {
        let mut snapshot = AppSnapshot::genesis();
        let mut account = Account::new("alice");
        account.pending_nonces = (1..=MAX_SNAPSHOT_PENDING_NONCES_PER_ACCOUNT as u64 + 1).collect();
        snapshot.accounts.push(account);

        let error = snapshot
            .validate_resource_limits()
            .expect_err("oversized pending nonce set must fail closed");
        assert!(error.contains("pending nonces"));
    }

    #[test]
    fn resource_limits_accept_existing_consensus_boundaries() {
        let mut snapshot = AppSnapshot::genesis();
        let mut account = Account::new("alice");
        account.pending_nonces = (1..=MAX_SNAPSHOT_PENDING_NONCES_PER_ACCOUNT as u64).collect();
        snapshot.accounts.push(account);

        let mut staking = StakingState::new();
        staking.liveness = (0..MAX_ACTIVE_VALIDATORS)
            .map(|index| ([index as u8; 32], Default::default()))
            .collect();
        snapshot.staking = Some(staking);

        snapshot
            .validate_resource_limits()
            .expect("exact existing consensus boundaries must remain valid");
    }

    #[test]
    fn bounded_json_round_trip_validates_decoded_snapshot() {
        let snapshot = AppSnapshot::genesis();
        let bytes = snapshot
            .to_bounded_json()
            .expect("genesis snapshot encodes");
        let decoded = AppSnapshot::from_bounded_json(&bytes).expect("genesis snapshot decodes");
        assert_eq!(decoded.height, snapshot.height);
        assert_eq!(decoded.market_configs.len(), snapshot.market_configs.len());
    }

    #[test]
    fn bounded_json_rejects_oversized_input_before_parse() {
        let bytes = vec![b'{'; MAX_APP_SNAPSHOT_BYTES + 1];
        let error = AppSnapshot::from_bounded_json(&bytes)
            .expect_err("oversized bytes must be rejected before JSON parsing");
        assert!(error.contains("serialized app snapshot is too large"));
    }
}
