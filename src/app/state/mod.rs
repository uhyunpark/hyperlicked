//! Application State
//!
//! Integrates orderbook, accounts, and mempool into a single
//! AppHook implementation for consensus.

pub mod artifacts;
mod consensus;
mod cow;
mod execution;
pub mod full_state_hash;
mod parallel;
mod trigger;

use std::borrow::Borrow;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash as StdHash;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::app::{
    accounts::{AccountManager, NonceResult},
    candles::{Candle, CandleManager, Interval},
    mempool::{Mempool, MempoolError},
    oracle::OracleState,
    orderbook::{Fill, Order, OrderBook},
    staking::{
        StakingState, StaticValidatorBootstrap, HYCK_GENESIS_EMISSIONS_RESERVE, HYCK_TOTAL_SUPPLY,
        HYCK_TREASURY_ADDRESS,
    },
    trigger::{Cloid, TriggerEvent, TriggerOrder, TriggerOrderId},
    Address, MarketConfig, Symbol, Transaction,
};
use crate::app::{ConsensusTransaction, SignedEnvelope};
use crate::types::{Committee, Hash, Price, Size, View};

pub use artifacts::{
    BlockExecutionArtifacts, ExecutionTransactionArtifact, StakingRewardEpochInfo,
    TransactionArtifact,
};
use cow::Shared;

/// A shallow-cloning map whose values detach independently on mutation.
///
/// The wrapper keeps the map's existing read/write surface (including raw
/// `V` values at its API boundary) while storing each value behind the
/// runtime-only [`Shared`] handle.  AppState still wraps the whole map in a
/// `Shared`, so a candidate first detaches the map's key table and then only
/// the touched value.  No wrapper state is serialized or hashed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CowMap<K: Eq + StdHash, V> {
    values: HashMap<K, Shared<V>>,
}

impl<K: Eq + StdHash, V> CowMap<K, V> {
    pub(crate) fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }
}

impl<K: Eq + StdHash, V> Default for CowMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> CowMap<K, V>
where
    K: Eq + StdHash,
{
    pub(crate) fn from_map(values: HashMap<K, V>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(key, value)| (key, Shared::new(value)))
                .collect(),
        }
    }

    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + StdHash,
    {
        self.values.get(key).map(|value| value.deref())
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + StdHash,
        V: Clone,
    {
        self.values
            .get_mut(key)
            .map(|value| DerefMut::deref_mut(value))
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        V: Clone,
    {
        self.values
            .insert(key, Shared::new(value))
            .map(|value| value.deref().clone())
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + StdHash,
        V: Clone,
    {
        self.values.remove(key).map(|value| value.deref().clone())
    }

    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + StdHash,
    {
        self.values.contains_key(key)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.values.iter().map(|(key, value)| (key, value.deref()))
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &V> {
        self.values.values().map(|value| value.deref())
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &K> {
        self.values.keys()
    }

    pub(crate) fn entry(&mut self, key: K) -> CowMapEntry<'_, K, V> {
        CowMapEntry {
            entry: self.values.entry(key),
        }
    }

    #[cfg(test)]
    pub(crate) fn value_ptr_eq<Q>(&self, key: &Q, other: &Self) -> bool
    where
        K: Borrow<Q>,
        Q: ?Sized + Eq + StdHash,
    {
        match (self.values.get(key), other.values.get(key)) {
            (Some(left), Some(right)) => left.ptr_eq(right),
            (None, None) => true,
            _ => false,
        }
    }
}

pub(crate) struct CowMapEntry<'a, K, V> {
    entry: std::collections::hash_map::Entry<'a, K, Shared<V>>,
}

impl<'a, K, V> CowMapEntry<'a, K, V>
where
    V: Clone,
{
    pub(crate) fn or_insert_with<F>(self, default: F) -> &'a mut V
    where
        F: FnOnce() -> V,
    {
        match self.entry {
            std::collections::hash_map::Entry::Occupied(entry) => &mut *entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                &mut *entry.insert(Shared::new(default()))
            }
        }
    }

    pub(crate) fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}

/// Maximum trades stored per symbol
pub const MAX_TRADES_PER_SYMBOL: usize = 1000;

/// Maintenance margin rate in basis points (500 = 5%)
pub const MAINTENANCE_MARGIN_BPS: i64 = 500;

/// Insurance fund warning threshold (CRITICAL-5)
/// When fund drops below this level ($1M in cents), emit warning log
pub const INSURANCE_FUND_WARNING_THRESHOLD: i64 = 100_000_000;

/// Errors raised while validating or rebuilding trigger-order indexes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TriggerIndexError {
    #[error("trigger order map key does not match order.id")]
    OrderIdMismatch,
    #[error("duplicate trigger client order ID")]
    DuplicateCloid,
    #[error("trigger sequence is behind an existing trigger order ID")]
    TriggerSequenceBehind,
    #[error("trigger indexes do not match trigger orders")]
    IndexMismatch,
}

/// Order update info for WebSocket event emission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderUpdateInfo {
    pub trader: String,
    pub order_id: String,
    pub symbol: String,
    pub side: String,       // "buy" or "sell"
    pub price: i64,         // Order price (cents)
    pub original_size: i64, // Original order size (satoshis)
    pub status: String,     // "open", "partial", "filled", "cancelled"
    pub filled: i64,
    pub remaining: i64,
}

/// Deposit info for WebSocket event emission
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepositInfo {
    pub trader: String,
    pub amount: i64,
}

/// Complete application state
#[derive(Clone)]
pub struct AppState {
    /// Explicit application/chain domain used by signed transaction
    /// validation.  This must be wired from node genesis/configuration.
    pub(crate) chain_domain: [u8; 32],
    /// Transient conservative invalidation mask for the authenticated state
    /// root shadow
    /// component tree. It is neither serialized nor part of any commitment.
    pub(crate) full_state_dirty: full_state_hash::DirtyTracker,
    /// Local-only development envelopes are enabled by the default fixture
    /// constructor.  Mainnet/testnet startup must disable this flag.
    pub(crate) allow_dev_envelopes: bool,
    /// Candidate/replay execution must not mutate its stale mempool snapshot.
    /// Canonical promotion reconciles against the current API-visible pool.
    pub(crate) speculative_execution: bool,
    /// Orderbooks by symbol
    pub(crate) orderbooks: Shared<HashMap<Symbol, OrderBook>>,
    /// Account manager
    pub(crate) accounts: Shared<AccountManager>,
    /// Transaction mempool
    pub(crate) mempool: Shared<Mempool>,
    /// Market configurations
    pub(crate) configs: Shared<HashMap<Symbol, MarketConfig>>,
    /// Oracle prices (mark prices for liquidation)
    pub(crate) mark_prices: Shared<HashMap<Symbol, Price>>,
    /// Mark price EMA (resists manipulation)
    pub(crate) mark_price_ema: Shared<HashMap<Symbol, Price>>,
    /// Current timestamp
    pub(crate) timestamp: u64,
    /// Fills from the last block execution (for event emission)
    pub(crate) pending_fills: Shared<Vec<Fill>>,
    /// Order updates from the last block execution (for event emission)
    pub(crate) pending_order_updates: Shared<Vec<OrderUpdateInfo>>,
    /// Trade history by symbol (recent trades, capped at MAX_TRADES_PER_SYMBOL)
    pub(crate) trade_history: Shared<CowMap<Symbol, VecDeque<Fill>>>,
    /// Insurance fund balance (in cents)
    pub(crate) insurance_fund: i64,
    /// Liquidations from the last block execution (for event emission)
    pub(crate) pending_liquidations: Shared<Vec<crate::app::liquidation::LiquidationResult>>,
    // Funding rate state
    /// Rolling premium samples per symbol (in bps)
    pub(crate) premium_samples: Shared<CowMap<Symbol, VecDeque<i64>>>,
    /// Current funding rate per symbol (in bps)
    pub(crate) current_funding_rates: Shared<HashMap<Symbol, i64>>,
    /// Last funding payment time per symbol (ms timestamp)
    pub(crate) last_funding_times: Shared<HashMap<Symbol, u64>>,
    /// Funding events from last block (for event emission)
    pub(crate) pending_funding: Shared<Vec<crate::app::funding::FundingResult>>,
    /// Deposits from last block (for WebSocket balance updates)
    pub(crate) pending_deposits: Shared<Vec<DepositInfo>>,
    /// Candle (OHLCV) aggregation manager
    pub(crate) candle_manager: Shared<CandleManager>,
    /// Staking state (validators, delegations, epochs)
    pub(crate) staking: Shared<StakingState>,
    /// Pending staking events from last block
    pub(crate) pending_staking_events: Shared<Vec<crate::app::staking::StakingTxResult>>,
    /// Pending validator set update from epoch transition (for consensus to consume)
    pub(crate) pending_validator_update: Option<crate::app::staking::ValidatorSetUpdate>,
    /// Current view (for epoch tracking)
    pub(crate) current_view: View,
    // === Trigger Orders ===
    /// All trigger orders by ID
    pub(crate) trigger_orders: Shared<CowMap<TriggerOrderId, TriggerOrder>>,
    /// Trigger order IDs by trader address
    pub(crate) trigger_orders_by_trader: Shared<CowMap<Address, Vec<TriggerOrderId>>>,
    /// Trigger order IDs by symbol (for efficient mark price checking)
    pub(crate) trigger_orders_by_symbol: Shared<CowMap<Symbol, Vec<TriggerOrderId>>>,
    /// Trigger order ID by (trader, symbol, cloid) for cloid lookup
    pub(crate) trigger_orders_by_cloid: Shared<CowMap<(Address, Symbol, Cloid), TriggerOrderId>>,
    /// Sequence number for generating trigger order IDs
    pub(crate) trigger_seq: u64,
    /// Pending trigger events from last block (for WebSocket emission)
    pub(crate) pending_trigger_events: Shared<Vec<TriggerEvent>>,
    /// Pending ADL events from last block (for WebSocket emission)
    pub(crate) pending_adl_events: Shared<Vec<crate::app::adl::ADLResult>>,
    /// Ordered receipt/event artifacts from the last valid block execution.
    /// This is transient application output and is not part of snapshots or
    /// the block wire format.
    pub(crate) last_execution_artifacts: Option<Arc<BlockExecutionArtifacts>>,
    /// Oracle state (external price feeds for funding rate)
    pub(crate) oracle: Shared<OracleState>,
    /// Committed block height (for API status)
    pub(crate) committed_height: u64,
    // === Daily Stats (for 24h metrics) ===
    /// Start-of-day price per symbol (for 24h change calculation)
    pub(crate) prev_day_prices: Shared<HashMap<Symbol, Price>>,
    /// Start-of-day timestamp (ms)
    pub(crate) day_start: u64,
    /// Cumulative daily volume per symbol (satoshis)
    pub(crate) day_volume: Shared<HashMap<Symbol, Size>>,
    /// Cumulative daily notional volume per symbol (cents)
    pub(crate) day_notional_volume: Shared<HashMap<Symbol, i64>>,
}

impl AppState {
    /// Validate every authoritative consensus-state record without mutation.
    /// Derived lookup caches are checked separately by
    /// [`Self::validate_derived_indexes`].
    pub fn validate_primary_state(&self) -> Result<(), String> {
        let mut config_symbols: Vec<_> = self.configs.keys().collect();
        config_symbols.sort();

        if self.orderbooks.len() != self.configs.len()
            || self.mark_prices.len() != self.configs.len()
        {
            return Err(
                "market configs, orderbooks, and mark prices must have identical key sets"
                    .to_string(),
            );
        }

        for symbol in config_symbols {
            let config = &self.configs[symbol];
            if config.symbol != *symbol {
                return Err(format!(
                    "market config key {symbol} does not match symbol {}",
                    config.symbol
                ));
            }
            let orderbook = self
                .orderbooks
                .get(symbol)
                .ok_or_else(|| format!("market {symbol} has no orderbook"))?;
            orderbook
                .validate_primary_state(symbol, config)
                .map_err(|error| format!("orderbook {symbol}: {error}"))?;
            if self.mark_prices.get(symbol).copied().unwrap_or(0) <= 0 {
                return Err(format!("market {symbol} has no positive mark price"));
            }
        }

        let mut ema_symbols: Vec<_> = self.mark_price_ema.keys().collect();
        ema_symbols.sort();
        for symbol in ema_symbols {
            if !self.configs.contains_key(symbol) || self.mark_price_ema[symbol] <= 0 {
                return Err(format!("market {symbol} has an invalid mark-price EMA"));
            }
        }

        self.accounts
            .validate_primary_state(&self.configs)
            .map_err(|error| format!("accounts: {error}"))?;
        self.staking
            .validate_primary_state()
            .map_err(|error| format!("staking: {error}"))?;
        self.validate_hyck_supply()?;
        self.oracle
            .validate_primary_state()
            .map_err(|error| format!("oracle: {error}"))?;
        self.validate_trigger_orders()
            .map_err(|error| format!("trigger orders: {error}"))?;

        if self.insurance_fund < 0 {
            return Err("insurance fund must not be negative".to_string());
        }

        for symbol in self
            .premium_samples
            .keys()
            .chain(self.current_funding_rates.keys())
            .chain(self.last_funding_times.keys())
        {
            if !self.configs.contains_key(symbol) {
                return Err(format!("funding state references unknown market {symbol}"));
            }
        }
        for (symbol, samples) in self.premium_samples.iter() {
            let interval_samples = self.configs[symbol].funding_interval_ms / 100;
            let max_samples = usize::try_from(interval_samples.max(1)).unwrap_or(usize::MAX);
            if samples.len() > max_samples {
                return Err(format!("market {symbol} has too many premium samples"));
            }
        }
        for (symbol, rate) in &self.current_funding_rates {
            let max_rate = self.configs[symbol].max_funding_rate_bps;
            if rate.checked_abs().is_none_or(|rate| rate > max_rate) {
                return Err(format!("market {symbol} has an invalid funding rate"));
            }
        }
        if self
            .last_funding_times
            .iter()
            .any(|(_, timestamp)| *timestamp > self.timestamp)
        {
            return Err("funding state has a future last-payment timestamp".to_string());
        }

        for symbol in self
            .oracle
            .prices
            .keys()
            .chain(self.oracle.source_prices.keys())
            .chain(self.oracle.configs.keys())
            .chain(self.oracle.last_update.keys())
        {
            if !self.configs.contains_key(symbol) {
                return Err(format!("oracle state references unknown market {symbol}"));
            }
        }

        Ok(())
    }

    /// Validate authoritative records and their derived lookup caches once at
    /// an execution/replay boundary.
    pub fn validate_consensus_state(&self) -> Result<(), String> {
        self.validate_primary_state()?;
        self.validate_derived_indexes()
    }

    /// Validate every derived index against its authoritative state without
    /// mutating the application. Normal execution uses this guard to fail
    /// closed if a candidate was corrupted after construction; import and
    /// recovery use `validate_and_rebuild_derived_indexes` below instead.
    pub fn validate_derived_indexes(&self) -> Result<(), String> {
        let mut symbols: Vec<_> = self.orderbooks.keys().collect();
        symbols.sort();
        for symbol in symbols {
            let orderbook = &self.orderbooks[symbol];
            orderbook
                .validate_derived_indexes()
                .map_err(|error| format!("orderbook {symbol}: {error}"))?;
        }

        self.staking
            .validate_invariants()
            .map_err(|error| format!("staking invariants: {error}"))?;
        self.validate_trigger_indexes()
            .map_err(|error| format!("trigger invariants: {error}"))?;

        Ok(())
    }

    /// Rebuild and validate every derived index from its authoritative state.
    ///
    /// Orderbook queues, staking validator records, and trigger orders are
    /// primary state.  Their lookup maps/counters are caches used by the
    /// execution path and may be stale after deserialization or an import.
    /// Rebuild on a private copy so a malformed primary record cannot leave a
    /// partially repaired `AppState` visible to callers.
    pub fn validate_and_rebuild_derived_indexes(&mut self) -> Result<(), String> {
        let mut rebuilt = self.clone();

        rebuilt.validate_primary_state()?;

        let mut symbols: Vec<_> = rebuilt.orderbooks.keys().cloned().collect();
        symbols.sort();
        for symbol in symbols {
            let orderbook = rebuilt
                .orderbooks
                .get_mut(&symbol)
                .expect("orderbook key was collected from the same map");
            orderbook
                .rebuild_derived_indexes()
                .map_err(|error| format!("orderbook {symbol}: {error}"))?;
        }

        rebuilt
            .staking
            .rebuild_index()
            .map_err(|error| format!("staking index: {error}"))?;
        rebuilt
            .staking
            .validate_invariants()
            .map_err(|error| format!("staking invariants: {error}"))?;

        rebuilt
            .rebuild_trigger_indexes()
            .map_err(|error| format!("trigger indexes: {error}"))?;
        rebuilt
            .validate_trigger_indexes()
            .map_err(|error| format!("trigger invariants: {error}"))?;

        rebuilt.validate_consensus_state()?;
        *self = rebuilt;
        Ok(())
    }

    pub fn new() -> Self {
        Self::new_with_chain_domain([0u8; 32])
    }

    /// Construct application state with an explicit chain domain.
    ///
    /// The default remains development-friendly for existing local fixtures;
    /// production node startup must call [`Self::set_allow_dev_envelopes`]
    /// with `false` before consensus starts.
    pub fn new_with_chain_domain(chain_domain: [u8; 32]) -> Self {
        let mut accounts = AccountManager::new();
        let liquid_genesis_supply = HYCK_TOTAL_SUPPLY
            .checked_sub(HYCK_GENESIS_EMISSIONS_RESERVE)
            .expect("emissions reserve fits fixed HYCK supply");
        accounts
            .deposit_hyck(HYCK_TREASURY_ADDRESS, liquid_genesis_supply)
            .expect("fixed HYCK supply fits native balance type");
        let mut staking = StakingState::new();
        staking
            .initialize_genesis_emissions_reserve()
            .expect("genesis HYCK emissions reserve is valid");
        let mut state = Self {
            chain_domain,
            full_state_dirty: full_state_hash::DirtyTracker::all(),
            allow_dev_envelopes: true,
            speculative_execution: false,
            orderbooks: HashMap::new().into(),
            accounts: accounts.into(),
            mempool: Mempool::default().into(),
            configs: HashMap::new().into(),
            mark_prices: HashMap::new().into(),
            mark_price_ema: HashMap::new().into(),
            // Genesis application state must be identical on every node.
            // The first accepted block establishes consensus time.
            timestamp: 0,
            pending_fills: Vec::new().into(),
            pending_order_updates: Vec::new().into(),
            trade_history: CowMap::new().into(),
            insurance_fund: 0,
            pending_liquidations: Vec::new().into(),
            premium_samples: CowMap::new().into(),
            current_funding_rates: HashMap::new().into(),
            last_funding_times: HashMap::new().into(),
            pending_funding: Vec::new().into(),
            pending_deposits: Vec::new().into(),
            candle_manager: CandleManager::new().into(),
            staking: staking.into(),
            pending_staking_events: Vec::new().into(),
            pending_validator_update: None,
            current_view: 0,
            trigger_orders: CowMap::new().into(),
            trigger_orders_by_trader: CowMap::new().into(),
            trigger_orders_by_symbol: CowMap::new().into(),
            trigger_orders_by_cloid: CowMap::new().into(),
            trigger_seq: 0,
            pending_trigger_events: Vec::new().into(),
            pending_adl_events: Vec::new().into(),
            last_execution_artifacts: None,
            oracle: OracleState::new().into(),
            committed_height: 0,
            prev_day_prices: HashMap::new().into(),
            day_start: 0,
            day_volume: HashMap::new().into(),
            day_notional_volume: HashMap::new().into(),
        };

        // Add default BTC-USDT market
        state.add_market(MarketConfig::default());
        state.staking.set_consensus_genesis_hash(chain_domain);

        state
    }

    /// Construct state with an explicit envelope policy.  Node startup should
    /// use `allow_dev_envelopes = false` for testnet/mainnet.
    pub fn new_with_chain_domain_and_dev(
        chain_domain: [u8; 32],
        allow_dev_envelopes: bool,
    ) -> Self {
        let mut state = Self::new_with_chain_domain(chain_domain);
        state.allow_dev_envelopes = allow_dev_envelopes;
        state
    }

    /// Set the explicit chain domain after loading node configuration.
    pub fn set_chain_domain(&mut self, chain_domain: [u8; 32]) {
        self.chain_domain = chain_domain;
        self.mark_full_state_dirty_unknown();
        self.staking.set_consensus_genesis_hash(chain_domain);
    }

    /// Bind the application staking/evidence path to the node's trusted
    /// static consensus context. Runtime startup should call this immediately
    /// after loading genesis and before consensus/network processing begins.
    pub fn set_consensus_context(&mut self, context: crate::types::ConsensusContext) {
        self.chain_domain = context.genesis_hash;
        self.mark_full_state_dirty_unknown();
        self.staking.set_consensus_context(context);
    }

    /// Inject the trusted static committee used by the runtime evidence path.
    /// The committee is runtime-only and is deliberately excluded from the
    /// application state root; canonical startup must also bootstrap the
    /// deterministic staking records separately.
    pub fn bind_authoritative_committee(&mut self, committee: Committee) -> Result<(), String> {
        let context = self
            .staking
            .consensus_context
            .ok_or_else(|| "consensus context must be set before committee binding".to_string())?;
        self.staking
            .bind_authoritative_committee(committee, context)
            .map_err(|error| error.to_string())
    }

    /// Bootstrap deterministic slashable staking records for the trusted
    /// curated committee. The records, unlike the runtime committee handle,
    /// are application state and therefore intentionally affect the state
    /// root in a deterministic way.
    pub fn bootstrap_static_committee(
        &mut self,
        committee: &Committee,
        records: &[StaticValidatorBootstrap],
    ) -> Result<(), String> {
        let before = self.clone();
        let bonded_before = self.staking.total_staked;
        let context = self.staking.consensus_context.ok_or_else(|| {
            "consensus context must be set before committee bootstrap".to_string()
        })?;
        if let Err(error) = self
            .staking
            .bootstrap_static_committee(committee, records, context)
        {
            return Err(error.to_string());
        }

        // Genesis self-stakes are bonded immediately and therefore leave the
        // liquid treasury reserve.  Re-running bootstrap is idempotent: only
        // newly added bonded stake is transferred.
        let newly_bonded = self
            .staking
            .total_staked
            .checked_sub(bonded_before)
            .ok_or_else(|| "genesis bonded stake moved backwards".to_string())?;
        if newly_bonded > 0 {
            if let Err(error) = self
                .accounts
                .withdraw_hyck(HYCK_TREASURY_ADDRESS, newly_bonded)
            {
                *self = before;
                return Err(format!("genesis HYCK allocation failed: {error}"));
            }
        }
        self.mark_full_state_dirty_unknown();
        if let Err(error) = self.validate_hyck_supply() {
            *self = before;
            return Err(error);
        }
        Ok(())
    }

    /// Apply explicit liquid HYCK allocations from a validated genesis file.
    ///
    /// The treasury is the sole source of these funds.  This method repeats
    /// the basic canonical checks at the state boundary and rolls back the
    /// complete state if any transfer or conservation check fails.
    pub fn apply_genesis_hyck_allocations(
        &mut self,
        allocations: &[(String, i64)],
    ) -> Result<(), String> {
        let before = self.clone();
        let mut seen = std::collections::HashSet::with_capacity(allocations.len());
        let mut total = 0i128;
        for (address, amount) in allocations {
            let canonical = address.trim().to_lowercase();
            if canonical.is_empty() || canonical != *address || canonical == HYCK_TREASURY_ADDRESS {
                return Err("genesis HYCK allocation address is not canonical".to_string());
            }
            if *amount <= 0 {
                return Err("genesis HYCK allocation amount must be positive".to_string());
            }
            if !seen.insert(canonical.clone()) {
                return Err(format!(
                    "genesis HYCK allocation address duplicated: {canonical}"
                ));
            }
            total = total
                .checked_add(i128::from(*amount))
                .ok_or_else(|| "genesis HYCK allocation total overflows".to_string())?;
        }
        if total > i128::from(HYCK_TOTAL_SUPPLY) {
            return Err("genesis HYCK allocations exceed fixed native supply".to_string());
        }
        let treasury_balance = i128::from(self.accounts.hyck_balance(HYCK_TREASURY_ADDRESS));
        if total > treasury_balance {
            return Err(format!(
                "genesis HYCK allocations exceed the available treasury reserve: requested {total}, available {treasury_balance}"
            ));
        }

        for (address, amount) in allocations {
            if let Err(error) = self
                .accounts
                .transfer_hyck(HYCK_TREASURY_ADDRESS, address, *amount)
            {
                *self = before;
                return Err(format!("genesis HYCK allocation failed: {error}"));
            }
        }
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_ACCOUNTS);
        if let Err(error) = self.validate_hyck_supply() {
            *self = before;
            return Err(error);
        }
        Ok(())
    }

    /// Enable or disable the explicit local development envelope scheme.
    pub fn set_allow_dev_envelopes(&mut self, allow: bool) {
        self.allow_dev_envelopes = allow;
    }

    pub fn chain_domain(&self) -> [u8; 32] {
        self.chain_domain
    }

    pub(crate) fn mark_full_state_dirty(&mut self, mask: full_state_hash::ComponentDirtyMask) {
        self.full_state_dirty.mark(mask);
    }

    pub(crate) fn mark_full_state_dirty_unknown(&mut self) {
        self.full_state_dirty
            .mark(full_state_hash::COMPONENT_DIRTY_UNKNOWN);
    }

    pub(crate) fn full_state_dirty(&self) -> full_state_hash::ComponentDirtyMask {
        self.full_state_dirty.bits()
    }

    pub(crate) fn clear_full_state_dirty(&mut self) {
        self.full_state_dirty.clear();
    }

    /// Clone a verified candidate parent for child block execution. The
    /// parent tree is supplied separately, so the child begins clean and
    /// only mutations in this execution set dirty bits.
    pub(crate) fn clone_for_verified_component_child(&self) -> Self {
        let mut clone = self.clone();
        clone.clear_full_state_dirty();
        clone.speculative_execution = true;
        // Mempool is node-local live input, not versioned consensus state.
        // Candidates use `proposed_tx_hashes` in CanonicalAppHook and reconcile
        // against the current canonical pool only when a block commits.
        clone.mempool.replace(Mempool::default());
        // These are per-block outputs. Replacing their shared pointers avoids
        // copying the parent's completed output merely to clear it in execute.
        clone.pending_fills.replace(Vec::new());
        clone.pending_order_updates.replace(Vec::new());
        clone.pending_liquidations.replace(Vec::new());
        clone.pending_funding.replace(Vec::new());
        clone.pending_deposits.replace(Vec::new());
        clone.pending_staking_events.replace(Vec::new());
        // Validator-set updates are consumed by consensus after canonical
        // commit.  They are not part of a speculative child and must not be
        // replayed by a sibling candidate.
        clone.pending_validator_update = None;
        clone.pending_trigger_events.replace(Vec::new());
        clone.pending_adl_events.replace(Vec::new());
        clone.last_execution_artifacts = None;
        clone
    }

    /// Clone a state for signed-transaction trial execution while preserving
    /// its current dirty mask; the trial's mutations then add to that mask.
    pub(crate) fn clone_for_transaction_trial(&self) -> Self {
        let mut clone = self.clone();
        clone.full_state_dirty = full_state_hash::DirtyTracker::from_bits(self.full_state_dirty());
        clone
    }

    /// Add a new market
    pub fn add_market(&mut self, config: MarketConfig) {
        let symbol = config.symbol.clone();
        self.orderbooks
            .insert(symbol.clone(), OrderBook::new(&symbol));
        self.configs.insert(symbol.clone(), config);
        self.mark_prices.insert(symbol, 5_000_000); // Default: $50,000
        self.mark_full_state_dirty(
            full_state_hash::COMPONENT_DIRTY_MARKET_CONFIGS
                | full_state_hash::COMPONENT_DIRTY_ORDERBOOKS
                | full_state_hash::COMPONENT_DIRTY_PRICES,
        );
    }

    /// Get orderbook for a symbol
    pub fn orderbook(&self, symbol: &str) -> Option<&OrderBook> {
        self.orderbooks.get(symbol)
    }

    /// Get mutable orderbook
    pub fn orderbook_mut(&mut self, symbol: &str) -> Option<&mut OrderBook> {
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_ORDERBOOKS);
        self.orderbooks.get_mut(symbol)
    }

    /// Submit a transaction to the mempool
    pub fn submit_tx(&mut self, tx: Transaction) -> Result<Hash, AppError> {
        if !self.allow_dev_envelopes && tx.is_user_action() {
            return Err(AppError::InvalidEnvelope(
                "unsigned user transaction rejected; submit a signed envelope".to_string(),
            ));
        }

        // Evidence is a protocol-owned action.  It may be carried by the
        // explicit system path, but admission still requires the complete
        // cryptographic proof; otherwise an external caller could fill the
        // mempool with an unauthenticated privileged action.  A repeated
        // proof is harmless and reuses the original transaction identity.
        if let Transaction::SubmitEvidence { evidence, .. } = &tx {
            self.validate_system_transaction(&tx)
                .map_err(AppError::InvalidEnvelope)?;
            if let Some(hash) = self.mempool.find_equivocation_evidence_hash(evidence) {
                return Ok(hash);
            }
            return self
                .mempool
                .add_verified_evidence(tx, self.timestamp)
                .map_err(AppError::Mempool);
        }

        self.mempool
            .add(tx, self.timestamp)
            .map_err(AppError::Mempool)
    }

    /// Submit a canonical authenticated user envelope.  User-facing API
    /// handlers must use this method; `submit_tx` is reserved for explicit
    /// system/local fixture transactions.
    pub fn submit_envelope(&mut self, envelope: SignedEnvelope) -> Result<Hash, AppError> {
        self.submit_envelope_at(envelope, self.timestamp)
    }

    /// Admit an envelope at an explicit wall-clock timestamp before putting it
    /// in the mempool. API callers pass the timestamp used for the request;
    /// consensus repeats this validation against block time.
    pub fn submit_envelope_at(
        &mut self,
        envelope: SignedEnvelope,
        admission_timestamp: u64,
    ) -> Result<Hash, AppError> {
        envelope
            .validate_for_block(
                self.chain_domain,
                admission_timestamp,
                self.allow_dev_envelopes,
            )
            .map_err(|error| AppError::InvalidEnvelope(error.to_string()))?;
        let signer = envelope.signer_address();
        // Admission is read-only: an account that has not been materialized
        // yet behaves like a fresh account at nonce zero.  Do not reserve the
        // nonce here; the mempool duplicate check below is the only pending
        // admission guard, while consensus execution owns nonce consumption.
        let nonce_result = self
            .accounts
            .get(&signer)
            .map(|account| account.validate_nonce_with_gap(envelope.nonce))
            .unwrap_or_else(|| {
                crate::app::accounts::Account::new(signer.clone())
                    .validate_nonce_with_gap(envelope.nonce)
            });
        match nonce_result {
            NonceResult::Valid | NonceResult::ValidWithGap => {}
            NonceResult::TooLow { expected } => {
                return Err(AppError::InvalidEnvelope(format!(
                    "invalid nonce: expected {}, got {}",
                    expected, envelope.nonce
                )));
            }
            NonceResult::GapTooLarge {
                expected,
                got,
                max_gap,
            } => {
                return Err(AppError::InvalidEnvelope(format!(
                    "invalid nonce: gap too large: expected {}, got {}, max gap is {}",
                    expected, got, max_gap
                )));
            }
            NonceResult::AlreadyUsed => {
                return Err(AppError::InvalidEnvelope(format!(
                    "invalid nonce: nonce already used: {}",
                    envelope.nonce
                )));
            }
            NonceResult::Exhausted => {
                return Err(AppError::InvalidEnvelope(
                    "invalid nonce: nonce counter exhausted".to_string(),
                ));
            }
        }
        if self.mempool.contains_signer_nonce(&signer, envelope.nonce) {
            return Err(AppError::InvalidEnvelope(
                "duplicate signer nonce already pending".to_string(),
            ));
        }
        self.mempool
            .add_envelope(envelope, admission_timestamp)
            .map_err(AppError::Mempool)
    }

    /// Execute one consensus payload item.  Envelope validation occurs before
    /// nonce handling and action execution.  A valid envelope consumes its
    /// nonce even if the action itself fails (Ethereum-style replay safety),
    /// while failed action mutations are discarded transactionally.
    pub fn execute_consensus_transaction(
        &mut self,
        transaction: ConsensusTransaction,
        block_timestamp: u64,
    ) -> Result<Vec<crate::app::orderbook::Fill>, AppError> {
        match transaction {
            ConsensusTransaction::System(tx) => {
                self.validate_system_transaction(&tx)
                    .map_err(AppError::InvalidEnvelope)?;
                if !self.allow_dev_envelopes && tx.is_user_action() {
                    return Err(AppError::InvalidEnvelope(
                        "unsigned user transaction rejected; submit a signed envelope".to_string(),
                    ));
                }
                self.execute_tx(tx)
            }
            ConsensusTransaction::Signed(envelope) => {
                envelope
                    .validate_for_block(
                        self.chain_domain,
                        block_timestamp,
                        self.allow_dev_envelopes,
                    )
                    .map_err(|error| AppError::InvalidEnvelope(error.to_string()))?;

                let signer = envelope.signer_address();
                let mut trial = self.clone_for_transaction_trial();
                // Block validation enforces signer-local contiguous order.
                // Consume the exact next nonce before executing the action;
                // if execution fails, only this nonce update is committed
                // below.  Gap-tolerant nonce handling is admission-only and
                // must not create pending state during consensus execution.
                trial
                    .accounts_mut()
                    .use_nonce(&signer, envelope.nonce)
                    .map_err(AppError::Account)?;
                let action_result = trial.execute_tx(envelope.action);
                match action_result {
                    Ok(fills) => {
                        *self = trial;
                        Ok(fills)
                    }
                    Err(error) => {
                        self.accounts_mut()
                            .use_nonce(&signer, envelope.nonce)
                            .map_err(AppError::Account)?;
                        Err(error)
                    }
                }
            }
        }
    }

    /// Set mark price for a symbol
    pub fn set_mark_price(&mut self, symbol: &str, price: Price) {
        self.mark_prices.insert(symbol.to_string(), price);
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_PRICES);
    }

    /// Get mark price for a symbol
    pub fn mark_price(&self, symbol: &str) -> Option<Price> {
        self.mark_prices.get(symbol).copied()
    }

    /// Get account
    pub fn account(&self, address: &str) -> Option<&crate::app::accounts::Account> {
        self.accounts.get(address)
    }

    /// Sum liquid native HYCK balances.  Perp collateral (`Account::balance`)
    /// is intentionally excluded from this ledger.
    pub fn hyck_liquid_supply(&self) -> Result<i64, String> {
        self.accounts
            .all_accounts()
            .iter()
            .try_fold(0i64, |total, account| {
                total
                    .checked_add(account.hyck_balance)
                    .ok_or_else(|| "native HYCK liquid supply overflows".to_string())
            })
    }

    /// Return the fixed native HYCK issuance in base units.
    pub const fn hyck_total_supply(&self) -> i64 {
        HYCK_TOTAL_SUPPLY
    }

    /// Validate native HYCK conservation across every authoritative bucket.
    ///
    /// Bonded stake excludes the unbonding queue (undelegation removes it from
    /// `total_staked`), so both are counted explicitly. Pending validator and
    /// delegator rewards plus the staking reward reserve remain owned HYCK and
    /// are included as well. Slashes are credited to the treasury, preserving
    /// the same invariant instead of burning stake.
    pub fn validate_hyck_supply(&self) -> Result<(), String> {
        let liquid =
            self.accounts
                .all_accounts()
                .into_iter()
                .try_fold(0i128, |total, account| {
                    if account.hyck_balance < 0 {
                        return Err(format!(
                            "account {} has negative native HYCK balance",
                            account.address
                        ));
                    }
                    total
                        .checked_add(i128::from(account.hyck_balance))
                        .ok_or_else(|| "native HYCK liquid supply overflows".to_string())
                })?;
        let bonded = i128::from(self.staking.total_staked);
        if bonded < 0 {
            return Err("native HYCK bonded supply must not be negative".to_string());
        }
        let unbonding = self
            .staking
            .unstake_queue
            .values()
            .flat_map(|requests| requests.iter())
            .try_fold(0i128, |total, request| {
                if request.amount < 0 {
                    return Err("native HYCK unbonding amount must not be negative".to_string());
                }
                total
                    .checked_add(i128::from(request.amount))
                    .ok_or_else(|| "native HYCK unbonding supply overflows".to_string())
            })?;
        let pending_rewards = self
            .staking
            .validators
            .values()
            .map(|validator| validator.pending_rewards)
            .chain(
                self.staking
                    .delegations
                    .values()
                    .map(|delegation| delegation.pending_rewards),
            )
            .try_fold(0i128, |total, reward| {
                if reward < 0 {
                    return Err("native HYCK pending reward must not be negative".to_string());
                }
                total
                    .checked_add(i128::from(reward))
                    .ok_or_else(|| "native HYCK pending reward supply overflows".to_string())
            })?;
        let emissions_reserve = i128::from(self.staking.emissions_reserve);
        if emissions_reserve < 0 {
            return Err("native HYCK reward reserve must not be negative".to_string());
        }

        let accounted = liquid
            .checked_add(bonded)
            .and_then(|total| total.checked_add(unbonding))
            .and_then(|total| total.checked_add(pending_rewards))
            .and_then(|total| total.checked_add(emissions_reserve))
            .ok_or_else(|| "native HYCK accounted supply overflows".to_string())?;
        if accounted != i128::from(HYCK_TOTAL_SUPPLY) {
            return Err(format!(
                "native HYCK supply mismatch: liquid={liquid}, bonded={bonded}, unbonding={unbonding}, pending_rewards={pending_rewards}, emissions_reserve={emissions_reserve}, total={accounted}, expected={HYCK_TOTAL_SUPPLY}"
            ));
        }
        Ok(())
    }

    /// Get mempool stats
    pub fn mempool_stats(&self) -> (usize, usize, usize) {
        self.mempool.bucket_counts()
    }

    /// Get all open orders for a specific address across all orderbooks
    pub fn orders_by_address(&self, address: &str) -> Vec<Order> {
        let mut orders = Vec::new();
        for book in self.orderbooks.values() {
            for order in book.orders_by_trader(address) {
                orders.push(order.clone());
            }
        }
        orders
    }

    /// Take pending fills (clears the list)
    pub fn take_pending_fills(&mut self) -> Vec<Fill> {
        std::mem::take(&mut self.pending_fills)
    }

    /// Take pending order updates (clears the list)
    pub fn take_pending_order_updates(&mut self) -> Vec<OrderUpdateInfo> {
        std::mem::take(&mut self.pending_order_updates)
    }

    /// Take pending liquidations (clears the list)
    pub fn take_pending_liquidations(&mut self) -> Vec<crate::app::liquidation::LiquidationResult> {
        std::mem::take(&mut self.pending_liquidations)
    }

    /// Take pending deposits (clears the list)
    pub fn take_pending_deposits(&mut self) -> Vec<DepositInfo> {
        std::mem::take(&mut self.pending_deposits)
    }

    /// Get account for position updates
    pub fn accounts(&self) -> &AccountManager {
        &self.accounts
    }

    /// Get mutable account manager (for nonce validation)
    pub fn accounts_mut(&mut self) -> &mut AccountManager {
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_ACCOUNTS);
        &mut self.accounts
    }

    /// Get market config
    pub fn market_config(&self, symbol: &str) -> Option<&MarketConfig> {
        self.configs.get(symbol)
    }

    /// Get recent trades for a symbol (most recent first)
    pub fn get_trades(&self, symbol: &str, limit: usize) -> Vec<&Fill> {
        self.trade_history
            .get(symbol)
            .map(|h| h.iter().rev().take(limit).collect())
            .unwrap_or_default()
    }

    /// Get user's fills across all symbols (where user is taker or maker)
    pub fn get_user_fills(&self, address: &str, limit: usize) -> Vec<&Fill> {
        let address_lower = address.to_lowercase();
        let mut fills: Vec<&Fill> = Vec::new();

        for trade_history in self.trade_history.values() {
            for fill in trade_history.iter() {
                if fill.taker.to_lowercase() == address_lower
                    || fill.maker.to_lowercase() == address_lower
                {
                    fills.push(fill);
                }
            }
        }

        // Sort by timestamp descending (most recent first)
        fills.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        fills.truncate(limit);
        fills
    }

    /// Get candles for a symbol and interval
    pub fn get_candles(&self, symbol: &str, interval: Interval, limit: usize) -> Vec<Candle> {
        self.candle_manager.get_candles(symbol, interval, limit)
    }

    /// Get mutable candle manager (for pause/resume/flush)
    pub fn candle_manager_mut(&mut self) -> &mut CandleManager {
        &mut self.candle_manager
    }

    /// Get all market configs (for snapshot)
    pub fn market_configs(&self) -> &HashMap<Symbol, MarketConfig> {
        &self.configs
    }

    /// Get all mark prices (for snapshot)
    pub fn mark_prices(&self) -> &HashMap<Symbol, Price> {
        &self.mark_prices
    }

    /// Get insurance fund balance
    pub fn insurance_fund_balance(&self) -> i64 {
        self.insurance_fund
    }

    /// Add to insurance fund (can be negative for losses)
    pub fn add_to_insurance_fund(&mut self, amount: i64) {
        self.insurance_fund += amount;
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_FUNDING);
    }

    /// Get current funding rate for a symbol (in bps)
    pub fn funding_rate(&self, symbol: &str) -> i64 {
        self.current_funding_rates.get(symbol).copied().unwrap_or(0)
    }

    /// Get last funding time for a symbol (ms timestamp)
    pub fn last_funding_time(&self, symbol: &str) -> u64 {
        self.last_funding_times.get(symbol).copied().unwrap_or(0)
    }

    /// Get next funding time for a symbol (ms timestamp)
    pub fn next_funding_time(&self, symbol: &str) -> u64 {
        let last = self.last_funding_time(symbol);
        let interval = self
            .configs
            .get(symbol)
            .map(|c| c.funding_interval_ms)
            .unwrap_or(3600000); // 1 hour default
        if last == 0 {
            self.timestamp + interval
        } else {
            last + interval
        }
    }

    /// Take pending funding events (clears the list)
    pub fn take_pending_funding(&mut self) -> Vec<crate::app::funding::FundingResult> {
        std::mem::take(&mut self.pending_funding)
    }

    /// Get all funding rates (for API)
    pub fn all_funding_rates(&self) -> &HashMap<Symbol, i64> {
        &self.current_funding_rates
    }

    /// Set funding rate for a symbol (for testing)
    pub fn set_funding_rate(&mut self, symbol: &str, rate: i64) {
        self.current_funding_rates.insert(symbol.to_string(), rate);
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_FUNDING);
    }

    // === Staking Accessors ===

    /// Get staking state (read-only)
    pub fn staking(&self) -> &StakingState {
        &self.staking
    }

    /// Get mutable staking state
    pub fn staking_mut(&mut self) -> &mut StakingState {
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_STAKING);
        &mut self.staking
    }

    /// Take pending staking events (clears the list)
    pub fn take_pending_staking_events(&mut self) -> Vec<crate::app::staking::StakingTxResult> {
        std::mem::take(&mut self.pending_staking_events)
    }

    /// Take pending validator set update (clears the value)
    ///
    /// Returns the validator set update from the most recent epoch transition.
    /// Used by consensus layer to update its validator configuration.
    pub fn take_pending_validator_update(
        &mut self,
    ) -> Option<crate::app::staking::ValidatorSetUpdate> {
        self.pending_validator_update.take()
    }

    /// Read-only view of a validator-set update staged by application
    /// execution. Canonical recovery uses this to fail closed before startup
    /// if a static committee state somehow contains a pending dynamic update.
    pub fn pending_validator_update(&self) -> Option<&crate::app::staking::ValidatorSetUpdate> {
        self.pending_validator_update.as_ref()
    }

    /// Get active validators for consensus
    pub fn active_validators(&self) -> Vec<crate::types::NodeId> {
        self.staking.active_validators()
    }

    /// Get current epoch
    pub fn current_epoch(&self) -> u64 {
        self.staking.current_epoch
    }

    /// Get current view
    pub fn current_view(&self) -> View {
        self.current_view
    }

    /// Get committed block height
    pub fn committed_height(&self) -> u64 {
        self.committed_height
    }

    // === Trigger Order Accessors ===

    /// Get a trigger order by ID
    pub fn trigger_order(&self, id: &str) -> Option<&TriggerOrder> {
        self.trigger_orders.get(id)
    }

    /// Get all trigger orders for a trader
    pub fn trigger_orders_by_trader(&self, trader: &str) -> Vec<&TriggerOrder> {
        self.trigger_orders_by_trader
            .get(trader)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.trigger_orders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all trigger orders for a symbol
    pub fn trigger_orders_by_symbol(&self, symbol: &str) -> Vec<&TriggerOrder> {
        self.trigger_orders_by_symbol
            .get(symbol)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.trigger_orders.get(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Take pending trigger events (clears the list)
    pub fn take_pending_trigger_events(&mut self) -> Vec<TriggerEvent> {
        std::mem::take(&mut self.pending_trigger_events)
    }

    /// Take pending ADL events (clears the list)
    pub fn take_pending_adl_events(&mut self) -> Vec<crate::app::adl::ADLResult> {
        std::mem::take(&mut self.pending_adl_events)
    }

    /// Take the ordered artifacts from the most recently executed valid block.
    ///
    /// Artifacts are transient application output.  They are not included in
    /// `AppSnapshot`, block serialization, or the state hash, so consuming
    /// them cannot change consensus state.
    pub fn take_execution_artifacts(&mut self) -> Option<BlockExecutionArtifacts> {
        self.last_execution_artifacts
            .take()
            .map(|artifacts| Arc::try_unwrap(artifacts).unwrap_or_else(|shared| (*shared).clone()))
    }

    /// Drop transient artifacts without cloning a shared candidate artifact.
    pub(crate) fn clear_execution_artifacts(&mut self) {
        self.last_execution_artifacts = None;
    }

    /// Borrow the ordered artifacts from the most recently executed valid
    /// block without consuming them.  Commitment preflight only validates the
    /// transient output, so callers should prefer this accessor when the state
    /// must remain available for a later commit.
    pub fn execution_artifacts(&self) -> Option<&BlockExecutionArtifacts> {
        self.last_execution_artifacts.as_deref()
    }

    // === Oracle Accessors ===

    /// Get index price for a symbol (oracle with mark price fallback).
    ///
    /// Used for funding rate calculation. Falls back to mark price if:
    /// - Oracle is disabled (bootstrap mode)
    /// - Oracle price is stale or unavailable
    /// - Oracle price deviates too much from mark (circuit breaker)
    pub fn index_price(&self, symbol: &str) -> Option<Price> {
        self.oracle
            .get_price(symbol, self.mark_prices.get(symbol).copied())
    }

    /// Get oracle state (read-only)
    pub fn oracle(&self) -> &OracleState {
        &self.oracle
    }

    /// Get mutable oracle state
    pub fn oracle_mut(&mut self) -> &mut OracleState {
        self.mark_full_state_dirty(full_state_hash::COMPONENT_DIRTY_ORACLE);
        &mut self.oracle
    }

    /// Get oracle price for a symbol (without mark fallback)
    pub fn oracle_price(&self, symbol: &str) -> Option<Price> {
        if !self.oracle.enabled {
            return None;
        }
        self.oracle.prices.get(symbol).map(|p| p.price)
    }

    // === Daily Stats Methods ===

    /// Reset daily stats at start of new day (UTC)
    pub fn maybe_reset_daily_stats(&mut self, timestamp: u64) {
        let day_ms = 24 * 60 * 60 * 1000;
        let current_day = timestamp / day_ms;
        let stored_day = self.day_start / day_ms;

        if current_day > stored_day || self.day_start == 0 {
            // New day - store previous close prices
            for (symbol, price) in &self.mark_prices {
                self.prev_day_prices.insert(symbol.clone(), *price);
            }
            self.day_volume.clear();
            self.day_notional_volume.clear();
            self.day_start = timestamp;
        }
    }

    /// Record volume from a fill
    pub fn record_fill_volume(&mut self, symbol: &str, size: Size, price: Price) {
        *self.day_volume.entry(symbol.to_string()).or_insert(0) += size.abs();
        let notional = (size.abs() as i128 * price as i128 / 100_000_000) as i64; // Convert to cents
        *self
            .day_notional_volume
            .entry(symbol.to_string())
            .or_insert(0) += notional;
    }

    /// Get mid price (average of best bid and ask)
    pub fn mid_price(&self, symbol: &str) -> Option<Price> {
        let book = self.orderbooks.get(symbol)?;
        let best_bid = book.best_bid()?;
        let best_ask = book.best_ask()?;
        Some((best_bid + best_ask) / 2)
    }

    /// Get premium (mark - oracle) / oracle in 1/1M units
    pub fn premium(&self, symbol: &str) -> Option<i64> {
        let mark = self.mark_prices.get(symbol)?;
        let oracle = self.oracle.get_price(symbol, Some(*mark))?;
        if oracle == 0 {
            return Some(0);
        }
        Some(((mark - oracle) * 1_000_000) / oracle)
    }

    /// Get open interest (sum of absolute position sizes)
    pub fn get_open_interest(&self, symbol: &str) -> Size {
        self.accounts
            .all_accounts()
            .iter()
            .filter_map(|a| a.positions.get(symbol))
            .filter(|p| p.size != 0)
            .map(|p| p.size.abs())
            .sum::<Size>()
            / 2 // Divide by 2 since each trade has both a long and short side
    }

    /// Get previous day price for a symbol
    pub fn prev_day_price(&self, symbol: &str) -> Option<Price> {
        self.prev_day_prices.get(symbol).copied()
    }

    /// Get day volume for a symbol (satoshis)
    pub fn day_volume(&self, symbol: &str) -> Size {
        self.day_volume.get(symbol).copied().unwrap_or(0)
    }

    /// Get day notional volume for a symbol (cents)
    pub fn day_notional_volume(&self, symbol: &str) -> i64 {
        self.day_notional_volume.get(symbol).copied().unwrap_or(0)
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// Application errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum AppError {
    #[error("mempool error: {0}")]
    Mempool(#[from] MempoolError),
    #[error("invalid transaction envelope: {0}")]
    InvalidEnvelope(String),
    #[error("account error: {0}")]
    Account(#[from] crate::app::accounts::AccountError),
    #[error("orderbook error: {0}")]
    OrderBook(#[from] crate::app::orderbook::OrderBookError),
    #[error("staking error: {0}")]
    Staking(#[from] crate::app::staking::StakingError),
    #[error("trigger order error: {0}")]
    Trigger(#[from] crate::app::trigger::TriggerError),
    #[error("oracle error: {0}")]
    Oracle(#[from] crate::app::oracle::OracleError),
    #[error("market not found")]
    MarketNotFound,
    #[error("order not found")]
    OrderNotFound,
    #[error("insufficient margin")]
    InsufficientMargin,
    #[error("reduce-only order would increase position")]
    ReduceOnlyViolation,
    #[error("position size {would_be} would exceed max {max}")]
    PositionTooLarge { max: i64, would_be: i64 },
    #[error("unauthorized: {0}")]
    Unauthorized(String),
}

#[cfg(test)]
mod cow_tests {
    use super::*;

    #[test]
    fn speculative_child_shares_untouched_components_and_isolates_mutations() {
        let mut parent = AppState::new();
        parent.accounts_mut().deposit("alice", 100).unwrap();
        let parent_root = parent.compute_state_hash();

        let mut child = parent.clone_for_verified_component_child();
        assert!(parent.accounts.ptr_eq(&child.accounts));
        assert!(!parent.mempool.ptr_eq(&child.mempool));
        assert_eq!(child.mempool_stats(), (0, 0, 0));
        assert!(parent.mark_prices.ptr_eq(&child.mark_prices));

        child.accounts_mut().deposit("alice", 25).unwrap();
        child.set_mark_price("BTC-USDT", 5_100_000);

        assert_eq!(parent.account("alice").unwrap().balance, 100);
        assert_eq!(child.account("alice").unwrap().balance, 125);
        assert_eq!(parent.mark_price("BTC-USDT"), Some(5_000_000));
        assert_eq!(child.mark_price("BTC-USDT"), Some(5_100_000));
        assert_eq!(parent.compute_state_hash(), parent_root);
        assert!(!parent.accounts.ptr_eq(&child.accounts));
        assert!(!parent.mark_prices.ptr_eq(&child.mark_prices));
        assert!(!parent.mempool.ptr_eq(&child.mempool));
    }

    #[test]
    fn speculative_child_does_not_inherit_pending_validator_update() {
        let mut parent = AppState::new();
        parent.pending_validator_update = Some(parent.staking.active_validator_set_for_consensus());

        let child = parent.clone_for_verified_component_child();
        let sibling = parent.clone_for_verified_component_child();

        assert!(parent.pending_validator_update.is_some());
        assert!(child.pending_validator_update.is_none());
        assert!(sibling.pending_validator_update.is_none());
    }

    #[test]
    fn premium_samples_clone_shares_untouched_values_and_isolates_mutations() {
        let mut parent = AppState::new();
        parent
            .premium_samples
            .insert("BTC-USDT".to_string(), VecDeque::from([1, 2]));
        parent
            .premium_samples
            .insert("ETH-USDT".to_string(), VecDeque::from([3, 4]));

        let mut child = parent.clone_for_verified_component_child();
        assert!(parent.premium_samples.ptr_eq(&child.premium_samples));
        assert!(parent
            .premium_samples
            .value_ptr_eq("ETH-USDT", &child.premium_samples));

        child
            .premium_samples
            .get_mut("BTC-USDT")
            .expect("BTC samples must exist")
            .push_back(5);

        assert_eq!(parent.premium_samples.get("BTC-USDT").unwrap().len(), 2);
        assert_eq!(child.premium_samples.get("BTC-USDT").unwrap().len(), 3);
        assert!(!parent.premium_samples.ptr_eq(&child.premium_samples));
        assert!(parent
            .premium_samples
            .value_ptr_eq("ETH-USDT", &child.premium_samples));
        assert!(!parent
            .premium_samples
            .value_ptr_eq("BTC-USDT", &child.premium_samples));
    }

    #[test]
    fn snapshots_serialize_accounts_deterministically() {
        let mut first = AppState::new();
        let mut second = AppState::new();
        first.timestamp = 42;
        second.timestamp = 42;

        for address in ["charlie", "alice", "bob"] {
            first.accounts_mut().deposit(address, 100).unwrap();
        }
        for address in ["bob", "charlie", "alice"] {
            second.accounts_mut().deposit(address, 100).unwrap();
        }

        let first_snapshot = first.create_snapshot(7);
        let second_snapshot = second.create_snapshot(7);
        assert_eq!(
            first_snapshot
                .accounts
                .iter()
                .map(|account| account.address.as_str())
                .collect::<Vec<_>>(),
            vec!["alice", "bob", "charlie", HYCK_TREASURY_ADDRESS]
        );
        assert_eq!(
            bincode::serialize(&first_snapshot).unwrap(),
            bincode::serialize(&second_snapshot).unwrap()
        );
    }

    #[test]
    fn fixed_hyck_supply_is_treasury_backed_and_root_authenticated() {
        let mut state = AppState::new_with_chain_domain([4u8; 32]);
        assert_eq!(state.hyck_total_supply(), HYCK_TOTAL_SUPPLY);
        assert_eq!(
            state.hyck_liquid_supply().unwrap(),
            HYCK_TOTAL_SUPPLY - HYCK_GENESIS_EMISSIONS_RESERVE
        );
        assert_eq!(
            state.staking.emissions_reserve,
            HYCK_GENESIS_EMISSIONS_RESERVE
        );
        state.validate_hyck_supply().unwrap();

        let root_before = state.compute_full_state_root();
        state
            .accounts_mut()
            .withdraw_hyck(HYCK_TREASURY_ADDRESS, 7)
            .unwrap();
        state.accounts_mut().deposit_hyck("alice", 7).unwrap();
        assert_eq!(state.account("alice").unwrap().balance, 0);
        assert_eq!(state.account("alice").unwrap().hyck_balance, 7);
        assert_ne!(root_before, state.compute_full_state_root());
        state.validate_hyck_supply().unwrap();
    }

    #[test]
    fn genesis_hyck_allocations_are_atomic_and_preserve_supply() {
        let mut state = AppState::new_with_chain_domain([4u8; 32]);
        state
            .apply_genesis_hyck_allocations(&[("bob".to_string(), 200), ("alice".to_string(), 100)])
            .unwrap();

        assert_eq!(state.account("alice").unwrap().hyck_balance, 100);
        assert_eq!(state.account("bob").unwrap().hyck_balance, 200);
        assert_eq!(
            state.account(HYCK_TREASURY_ADDRESS).unwrap().hyck_balance,
            HYCK_TOTAL_SUPPLY - HYCK_GENESIS_EMISSIONS_RESERVE - 300
        );
        state.validate_hyck_supply().unwrap();

        let root_before = state.compute_full_state_root();
        let error = state
            .apply_genesis_hyck_allocations(&[("carol".to_string(), HYCK_TOTAL_SUPPLY)])
            .unwrap_err();
        assert!(error.contains("treasury reserve"));
        assert!(state.account("carol").is_none());
        assert_eq!(state.compute_full_state_root(), root_before);
        state.validate_hyck_supply().unwrap();

        let error = state
            .apply_genesis_hyck_allocations(&[("ALICE".to_string(), 1)])
            .unwrap_err();
        assert!(error.contains("canonical"));
        assert_eq!(state.account("alice").unwrap().hyck_balance, 100);
    }
}

#[cfg(test)]
mod unstake_tests {
    use super::*;
    use crate::app::accounts::AccountError;
    use crate::app::staking::{UnstakeRequest, UNSTAKE_DELAY_MS};

    #[test]
    fn claim_unstaked_preserves_other_delegators_and_supports_partial_repeated_claims() {
        let mut state = AppState::new_with_chain_domain([0u8; 32]);
        let ready = UNSTAKE_DELAY_MS;
        state.staking_mut().unstake_queue.insert(
            "alice".into(),
            vec![
                UnstakeRequest {
                    delegator: "alice".into(),
                    validator: Some("validator".into()),
                    amount: 11,
                    completion_time: ready,
                },
                UnstakeRequest {
                    delegator: "alice".into(),
                    validator: Some("validator".into()),
                    amount: 22,
                    completion_time: ready + 1,
                },
            ],
        );
        state.staking_mut().unstake_queue.insert(
            "bob".into(),
            vec![
                UnstakeRequest {
                    delegator: "bob".into(),
                    validator: Some("validator".into()),
                    amount: 33,
                    completion_time: ready,
                },
                UnstakeRequest {
                    delegator: "bob".into(),
                    validator: Some("validator".into()),
                    amount: 44,
                    completion_time: ready + 1,
                },
            ],
        );
        let alice_before = state.staking().unstake_queue.get("alice").unwrap().clone();
        let bob_before = state.staking().unstake_queue.get("bob").unwrap().clone();

        state.timestamp = ready;
        state
            .execute_tx(Transaction::ClaimUnstaked {
                delegator: "alice".into(),
            })
            .unwrap();
        assert_eq!(state.account("alice").unwrap().hyck_balance, 11);
        assert_eq!(
            bincode::serialize(state.staking().unstake_queue.get("alice").unwrap()).unwrap(),
            bincode::serialize(&vec![alice_before[1].clone()]).unwrap()
        );
        assert_eq!(
            bincode::serialize(state.staking().unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );

        state
            .execute_tx(Transaction::ClaimUnstaked {
                delegator: "alice".into(),
            })
            .unwrap();
        assert_eq!(state.account("alice").unwrap().hyck_balance, 11);
        assert_eq!(
            bincode::serialize(state.staking().unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );

        state.timestamp = ready + 1;
        state
            .execute_tx(Transaction::ClaimUnstaked {
                delegator: "alice".into(),
            })
            .unwrap();
        assert_eq!(state.account("alice").unwrap().hyck_balance, 33);
        assert!(!state.staking().unstake_queue.contains_key("alice"));
        assert_eq!(
            bincode::serialize(state.staking().unstake_queue.get("bob").unwrap()).unwrap(),
            bincode::serialize(&bob_before).unwrap()
        );
    }

    #[test]
    fn claim_unstaked_balance_overflow_preserves_balance_and_queue() {
        let mut state = AppState::new_with_chain_domain([0u8; 32]);
        let ready = UNSTAKE_DELAY_MS;
        state
            .accounts_mut()
            .deposit_hyck("alice", i64::MAX - 9)
            .unwrap();
        state.staking_mut().unstake_queue.insert(
            "alice".into(),
            vec![UnstakeRequest {
                delegator: "alice".into(),
                validator: None,
                amount: 10,
                completion_time: ready,
            }],
        );
        let queue_before = state.staking().unstake_queue.get("alice").unwrap().clone();

        state.timestamp = ready;
        let error = state
            .execute_tx(Transaction::ClaimUnstaked {
                delegator: "alice".into(),
            })
            .unwrap_err();

        assert!(matches!(
            error,
            AppError::Account(AccountError::HyckBalanceOverflow)
        ));
        assert_eq!(state.account("alice").unwrap().hyck_balance, i64::MAX - 9);
        assert_eq!(
            bincode::serialize(state.staking().unstake_queue.get("alice").unwrap()).unwrap(),
            bincode::serialize(&queue_before).unwrap()
        );
    }
}
