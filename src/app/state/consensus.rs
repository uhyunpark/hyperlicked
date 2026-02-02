//! Consensus Integration
//!
//! AppHook implementation, state hashing, and snapshot support.

use std::collections::HashMap;

use sha2::{Digest, Sha256};

use crate::app::{
    accounts::AccountManager,
    candles::CandleManager,
    mempool::Mempool,
    orderbook::OrderBook,
    staking::StakingState,
    MarketConfig, Symbol,
};
use crate::consensus::AppHook;
use crate::types::{Block, Hash};

use super::AppState;

impl AppState {
    /// Compute state hash for Byzantine detection
    ///
    /// When `incremental_hash` feature is enabled, uses O(k) incremental hashing
    /// where k is the number of changed accounts. Otherwise uses O(n) full hashing.
    ///
    /// In debug builds with incremental_hash, shadow mode verifies incremental
    /// hash matches full hash.
    #[cfg(feature = "incremental_hash")]
    pub fn compute_state_hash(&mut self) -> Hash {
        use super::incremental_hash::{GlobalState, OrderbookSnapshot, StakingSnapshot};

        // Build global state snapshot
        let globals = GlobalState {
            insurance_fund: self.insurance_fund,
            funding_rates: self.current_funding_rates.iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_funding_times: self.last_funding_times.iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            mark_prices: self.mark_prices.iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            orderbook_state: self.orderbooks.iter()
                .map(|(symbol, book)| OrderbookSnapshot {
                    symbol: symbol.clone(),
                    best_bid: book.best_bid().unwrap_or(0),
                    best_ask: book.best_ask().unwrap_or(0),
                    last_price: book.last_price(),
                })
                .collect(),
            staking_state: StakingSnapshot {
                current_epoch: self.staking.current_epoch,
                total_staked: self.staking.total_staked,
                rewards_pool: self.staking.rewards_pool,
                validator_count: self.staking.validators.len(),
                delegation_count: self.staking.delegations.len(),
            },
            trigger_count: self.trigger_orders.len(),
            oracle_enabled: self.oracle.enabled,
        };

        let accounts = self.accounts.all_accounts();
        let incremental = self.incremental_hasher.compute_root(&accounts, &globals);

        // Note: Shadow mode disabled because incremental hash uses a different
        // algorithm (bucket-based) than full hash. Both are deterministic but
        // produce different values. For cross-validator consensus, all nodes
        // must use the same hash function.
        //
        // TODO: Either make incremental hash produce identical output to full hash,
        // or migrate all validators to use incremental hash at the same time.

        incremental
    }

    /// Full state hash computation (O(n) where n = total accounts)
    ///
    /// Used as fallback when incremental_hash feature disabled,
    /// and for shadow mode verification in debug builds.
    #[cfg(not(feature = "incremental_hash"))]
    pub fn compute_state_hash(&self) -> Hash {
        self.compute_state_hash_full()
    }

    /// Full state hash (always available for shadow mode verification)
    pub fn compute_state_hash_full(&self) -> Hash {
        let mut hasher = Sha256::new();

        // === Accounts ===
        // Get all accounts sorted by address
        let accounts = self.accounts.all_accounts();
        let mut sorted_accounts: Vec<_> = accounts.iter().collect();
        sorted_accounts.sort_by_key(|a| &a.address);

        for account in sorted_accounts {
            hasher.update(account.address.as_bytes());
            hasher.update(account.balance.to_le_bytes());
            hasher.update(account.locked.to_le_bytes());
            hasher.update(account.nonce.to_le_bytes());

            // Hash pending_nonces (already sorted since BTreeSet)
            // Only include if non-empty to maintain backward compat with old state
            if !account.pending_nonces.is_empty() {
                hasher.update(&[1u8]); // Flag: has pending nonces
                for pending_nonce in &account.pending_nonces {
                    hasher.update(pending_nonce.to_le_bytes());
                }
            } else {
                hasher.update(&[0u8]); // Flag: no pending nonces
            }

            // Hash positions for this account (sorted by symbol)
            let mut position_symbols: Vec<_> = account.positions.keys().collect();
            position_symbols.sort();

            for symbol in position_symbols {
                if let Some(pos) = account.positions.get(symbol) {
                    hasher.update(symbol.as_bytes());
                    hasher.update(pos.size.to_le_bytes());
                    hasher.update(pos.entry_price.to_le_bytes());
                    hasher.update(pos.realized_pnl.to_le_bytes());
                    hasher.update(pos.cumulative_funding.to_le_bytes());
                }
            }
        }

        // === Orderbooks ===
        let mut symbols: Vec<_> = self.orderbooks.keys().collect();
        symbols.sort();

        for symbol in &symbols {
            if let Some(book) = self.orderbooks.get(*symbol) {
                hasher.update(symbol.as_bytes());
                hasher.update(book.best_bid().unwrap_or(0).to_le_bytes());
                hasher.update(book.best_ask().unwrap_or(0).to_le_bytes());
                hasher.update(book.last_price().to_le_bytes());
            }
        }

        // === Mark prices (sorted) ===
        let mut mark_prices: Vec<_> = self.mark_prices.iter().collect();
        mark_prices.sort_by_key(|(k, _)| *k);
        for (symbol, price) in mark_prices {
            hasher.update(symbol.as_bytes());
            hasher.update(price.to_le_bytes());
        }

        // === Insurance fund ===
        hasher.update(self.insurance_fund.to_le_bytes());

        // === Funding rates (sorted) ===
        let mut funding_rates: Vec<_> = self.current_funding_rates.iter().collect();
        funding_rates.sort_by_key(|(k, _)| *k);
        for (symbol, rate) in funding_rates {
            hasher.update(symbol.as_bytes());
            hasher.update(rate.to_le_bytes());
        }

        // === Last funding times (sorted) ===
        let mut last_funding: Vec<_> = self.last_funding_times.iter().collect();
        last_funding.sort_by_key(|(k, _)| *k);
        for (symbol, time) in last_funding {
            hasher.update(symbol.as_bytes());
            hasher.update(time.to_le_bytes());
        }

        // === Staking state ===
        hasher.update(self.staking.current_epoch.to_le_bytes());
        hasher.update(self.staking.total_staked.to_le_bytes());
        hasher.update(self.staking.rewards_pool.to_le_bytes());

        // Hash validators (sorted by operator)
        let mut validators: Vec<_> = self.staking.validators.iter().collect();
        validators.sort_by_key(|(k, _)| *k);
        for (operator, validator) in validators {
            use crate::app::staking::ValidatorStatus;

            hasher.update(operator.as_bytes());
            hasher.update(validator.self_stake.to_le_bytes());
            hasher.update(validator.total_stake.to_le_bytes());
            hasher.update(validator.commission_bps.to_le_bytes());
            hasher.update(validator.pending_rewards.to_le_bytes());
            // Use explicit values for enum without repr(u8)
            let status_byte: u8 = match validator.status {
                ValidatorStatus::Active => 0,
                ValidatorStatus::Inactive => 1,
                ValidatorStatus::Jailed => 2,
                ValidatorStatus::Tombstoned => 3,
            };
            hasher.update(&[status_byte]);
            hasher.update(validator.jail_until.to_le_bytes());
        }

        // Hash delegations (sorted by delegator, then validator)
        let mut delegations: Vec<_> = self.staking.delegations.iter().collect();
        delegations.sort_by_key(|(k, _)| *k);
        for ((delegator, validator), delegation) in delegations {
            hasher.update(delegator.as_bytes());
            hasher.update(validator.as_bytes());
            hasher.update(delegation.amount.to_le_bytes());
            hasher.update(delegation.pending_rewards.to_le_bytes());
        }

        // === Trigger orders (sorted by ID) ===
        let mut trigger_orders: Vec<_> = self.trigger_orders.iter().collect();
        trigger_orders.sort_by_key(|(k, _)| *k);
        for (id, order) in trigger_orders {
            use crate::app::trigger::{TriggerCondition, TriggerOrderStatus, TriggerType};

            hasher.update(id.as_bytes());
            hasher.update(order.trader.as_bytes());
            hasher.update(order.symbol.as_bytes());
            // Use explicit values for enums without repr(u8)
            let side_byte: u8 = match order.side {
                crate::app::Side::Bid => 0,
                crate::app::Side::Ask => 1,
            };
            hasher.update(&[side_byte]);
            hasher.update(order.size.to_le_bytes());
            let trigger_type_byte: u8 = match order.trigger_type {
                TriggerType::StopLoss => 0,
                TriggerType::TakeProfit => 1,
            };
            hasher.update(&[trigger_type_byte]);
            let condition_byte: u8 = match order.condition {
                TriggerCondition::PriceAbove => 0,
                TriggerCondition::PriceBelow => 1,
            };
            hasher.update(&[condition_byte]);
            hasher.update(order.trigger_price.to_le_bytes());
            hasher.update(order.limit_price.unwrap_or(0).to_le_bytes());
            let status_byte: u8 = match order.status {
                TriggerOrderStatus::Pending => 0,
                TriggerOrderStatus::Triggered => 1,
                TriggerOrderStatus::Cancelled => 2,
                TriggerOrderStatus::Failed => 3,
            };
            hasher.update(&[status_byte]);
            hasher.update(order.timestamp.to_le_bytes());
        }

        // === Oracle prices (sorted by symbol) ===
        if self.oracle.enabled {
            hasher.update(&[1u8]); // Oracle enabled flag
            let mut oracle_prices: Vec<_> = self.oracle.prices.iter().collect();
            oracle_prices.sort_by_key(|(k, _)| *k);
            for (symbol, oracle_price) in oracle_prices {
                hasher.update(symbol.as_bytes());
                hasher.update(oracle_price.price.to_le_bytes());
                hasher.update(oracle_price.timestamp.to_le_bytes());
                hasher.update(oracle_price.source_count.to_le_bytes());
                hasher.update(oracle_price.confidence_bps.to_le_bytes());
            }
        } else {
            hasher.update(&[0u8]); // Oracle disabled flag
        }

        hasher.finalize().into()
    }

    /// Create snapshot of current state
    pub fn create_snapshot(&self, height: u64) -> crate::storage::AppSnapshot {
        crate::storage::AppSnapshot {
            height,
            timestamp: self.timestamp,
            accounts: self.accounts.all_accounts(),
            market_configs: self.configs.values().cloned().collect(),
            mark_prices: self
                .mark_prices
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            insurance_fund: self.insurance_fund,
            funding_rates: self
                .current_funding_rates
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            last_funding_times: self
                .last_funding_times
                .iter()
                .map(|(k, v)| (k.clone(), *v))
                .collect(),
            staking: Some(self.staking.clone()),
            oracle: Some(self.oracle.clone()),
            trigger_orders: self.trigger_orders.values().cloned().collect(),
            premium_samples: self
                .premium_samples
                .iter()
                .map(|(k, v)| (k.clone(), v.iter().copied().collect()))
                .collect(),
            trigger_seq: self.trigger_seq,
        }
    }

    /// Restore state from snapshot (for recovery)
    pub fn from_snapshot(snapshot: crate::storage::AppSnapshot) -> Self {
        use std::collections::VecDeque;

        use crate::app::oracle::OracleState;
        use crate::app::trigger::{Cloid, TriggerOrderId};

        // Extract fields from snapshot (consuming it)
        let mark_prices = snapshot.mark_prices_map();
        let timestamp = snapshot.timestamp;
        let insurance_fund = snapshot.insurance_fund;
        let funding_rates = snapshot.funding_rates_map();
        let last_funding_times = snapshot.last_funding_times_map();

        // Restore staking state if present
        let mut staking = snapshot.staking.unwrap_or_else(StakingState::new);
        staking.rebuild_index(); // Rebuild transient indexes

        // Restore oracle state if present
        let oracle = snapshot.oracle.unwrap_or_else(OracleState::new);

        // Restore trigger orders with indexes
        let mut trigger_orders = HashMap::new();
        let mut trigger_orders_by_trader: HashMap<String, Vec<TriggerOrderId>> = HashMap::new();
        let mut trigger_orders_by_symbol: HashMap<Symbol, Vec<TriggerOrderId>> = HashMap::new();
        let mut trigger_orders_by_cloid: HashMap<(String, Symbol, Cloid), TriggerOrderId> =
            HashMap::new();

        for order in snapshot.trigger_orders {
            let id = order.id.clone();
            trigger_orders_by_trader
                .entry(order.trader.clone())
                .or_default()
                .push(id.clone());
            trigger_orders_by_symbol
                .entry(order.symbol.clone())
                .or_default()
                .push(id.clone());
            if let Some(ref cloid) = order.cloid {
                trigger_orders_by_cloid.insert(
                    (order.trader.clone(), order.symbol.clone(), cloid.clone()),
                    id.clone(),
                );
            }
            trigger_orders.insert(id, order);
        }

        // Restore premium samples
        let premium_samples: HashMap<Symbol, VecDeque<i64>> = snapshot
            .premium_samples
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();

        let mut state = Self {
            orderbooks: HashMap::new(),
            accounts: AccountManager::from_accounts(snapshot.accounts),
            mempool: Mempool::default(),
            configs: HashMap::new(),
            mark_prices,
            timestamp,
            pending_fills: Vec::new(),
            pending_order_updates: Vec::new(),
            trade_history: HashMap::new(),
            insurance_fund,
            pending_liquidations: Vec::new(),
            premium_samples,
            current_funding_rates: funding_rates,
            last_funding_times,
            pending_funding: Vec::new(),
            pending_deposits: Vec::new(),
            candle_manager: CandleManager::new(), // Candles are rebuilt from trades
            staking,
            pending_staking_events: Vec::new(),
            pending_validator_update: None,
            current_view: 0,
            trigger_orders,
            trigger_orders_by_trader,
            trigger_orders_by_symbol,
            trigger_orders_by_cloid,
            trigger_seq: snapshot.trigger_seq,
            pending_trigger_events: Vec::new(),
            pending_adl_events: Vec::new(),
            oracle,
            committed_height: 0, // Will be set by consensus after replay
            prev_day_prices: HashMap::new(),
            day_start: 0,
            day_volume: HashMap::new(),
            day_notional_volume: HashMap::new(),
            #[cfg(feature = "incremental_hash")]
            incremental_hasher: crate::app::state::incremental_hash::IncrementalHasher::new(),
        };

        // Restore market configs and create orderbooks
        for config in snapshot.market_configs {
            let symbol = config.symbol.clone();
            state
                .orderbooks
                .insert(symbol.clone(), OrderBook::new(&symbol));
            state.configs.insert(symbol, config);
        }

        state
    }
}

impl AppHook for AppState {
    fn take_validator_update(&mut self) -> Option<crate::app::staking::ValidatorSetUpdate> {
        self.pending_validator_update.take()
    }

    fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
        // Peek pending transactions (without removing them yet)
        // They get drained after block commit via commit_proposal()
        // Note: We use peek_block_txs() here since we don't have the view yet.
        // The actual two-phase commit happens in execute().
        let txs = self.mempool.peek_block_txs(1000);
        if txs.is_empty() {
            return vec![];
        }
        // Serialize transactions for propagation to followers
        bincode::serialize(&txs).unwrap_or_default()
    }

    fn execute(&mut self, block: &Block) -> Hash {
        self.timestamp = block.timestamp;
        self.current_view = block.view;
        self.committed_height = block.height;

        // Prune stale transactions from mempool (age-based eviction)
        self.mempool.prune_stale(block.timestamp);

        // Clear pending events from previous block
        self.pending_fills.clear();
        self.pending_order_updates.clear();
        self.pending_liquidations.clear();
        self.pending_funding.clear();
        self.pending_staking_events.clear();
        self.pending_trigger_events.clear();
        self.pending_adl_events.clear();

        // === Staking: Epoch Transition ===
        if self.staking.enabled && self.staking.should_transition_epoch(block.view) {
            let result = self.staking.transition_epoch(block.view, block.timestamp);
            tracing::info!(
                epoch = result.epoch,
                active_validators = result.new_active_set.len(),
                jailed = result.jailed.len(),
                "Epoch transition"
            );

            // Process unstake completions - return funds to accounts
            for (delegator, amount) in result.unstake_completions {
                if let Err(e) = self.accounts.deposit(&delegator, amount) {
                    tracing::warn!(error = %e, "Failed to return unstaked funds");
                }
            }

            // Store validator set update for consensus layer to consume
            if let Some(update) = result.validator_set_update {
                tracing::debug!(
                    validators = update.len(),
                    "Storing validator set update for consensus"
                );
                self.pending_validator_update = Some(update);
            }
        }

        // === Staking: Add Block Reward ===
        if self.staking.enabled {
            self.staking.add_block_reward();
        }

        // Get transactions for this block and track their hashes for two-phase commit
        // In multi-node: use payload (propagated from leader)
        // In single-node or if payload is empty: use local mempool
        let (txs, tx_hashes): (Vec<crate::app::Transaction>, Vec<crate::types::Hash>) =
            if !block.payload.is_empty() {
                // Deserialize transactions from block payload (multi-node mode)
                let txs: Vec<crate::app::Transaction> =
                    bincode::deserialize(&block.payload).unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Failed to deserialize payload, using mempool");
                        self.mempool.prepare_block(1000)
                    });
                // Compute hashes for two-phase commit
                let hashes: Vec<_> = txs
                    .iter()
                    .map(|tx| crate::types::hash(&tx.to_bytes()))
                    .collect();
                (txs, hashes)
            } else {
                // Single-node mode or empty payload: drain from local mempool
                let txs = self.mempool.prepare_block(1000);
                // Hashes were already computed during add()
                let hashes: Vec<_> = txs
                    .iter()
                    .map(|tx| crate::types::hash(&tx.to_bytes()))
                    .collect();
                (txs, hashes)
            };

        // Execute transactions (parallel when feature enabled)
        let fills = self.execute_transactions_parallel(txs);
        self.pending_fills.extend(fills);

        // Commit the proposal (two-phase: finalize removal from mempool)
        // This replaces the old drain_block() approach
        // Use unchecked commit since in execute() we're committing the block,
        // view checking happens in the consensus layer
        if !tx_hashes.is_empty() {
            self.mempool.commit_proposal_unchecked(&tx_hashes);
        }

        // Check and execute liquidations after all transactions (with circuit breaker)
        let max_liquidations = crate::config::Config::global().max_liquidations_per_block;
        let liquidation_batch = crate::app::liquidation::check_and_liquidate_limited(
            &mut self.accounts,
            &self.mark_prices,
            max_liquidations,
        );
        self.pending_liquidations = liquidation_batch.results;

        // Process liquidation results with ADL if needed
        self.process_liquidations_with_adl();

        // === Funding Rate Logic ===
        self.process_funding();

        // === Trigger Order Processing ===
        // Check and execute trigger orders after all transactions are processed
        let trigger_fills = self.process_triggers();
        self.pending_fills.extend(trigger_fills);

        // Return state hash for Byzantine detection
        self.compute_state_hash()
    }
}

impl AppState {
    /// Mark an account as dirty for incremental hashing
    #[cfg(feature = "incremental_hash")]
    pub(crate) fn mark_dirty_account(&mut self, address: &str) {
        self.incremental_hasher.mark_dirty_account(address);
    }

    /// Mark all accounts as dirty (e.g., after funding)
    #[cfg(feature = "incremental_hash")]
    pub(crate) fn mark_all_accounts_dirty(&mut self) {
        self.incremental_hasher.mark_all_accounts_dirty();
    }

    /// Mark global state as dirty
    #[cfg(feature = "incremental_hash")]
    pub(crate) fn mark_globals_dirty(&mut self) {
        self.incremental_hasher.mark_globals_dirty();
    }

    /// No-op when incremental_hash disabled
    #[cfg(not(feature = "incremental_hash"))]
    pub(crate) fn mark_dirty_account(&mut self, _address: &str) {}

    #[cfg(not(feature = "incremental_hash"))]
    pub(crate) fn mark_all_accounts_dirty(&mut self) {}

    #[cfg(not(feature = "incremental_hash"))]
    pub(crate) fn mark_globals_dirty(&mut self) {}

    /// Process liquidation results with ADL (auto-deleveraging) if needed
    ///
    /// For each liquidation:
    /// 1. Calculate total loss (position PnL + underwater amount)
    /// 2. If loss, check if ADL is needed (insurance fund insufficient)
    /// 3. ADL absorbs losses from profitable counter-parties
    /// 4. Remaining loss goes to insurance fund
    /// 5. Remaining balance from liquidated account goes to insurance fund
    fn process_liquidations_with_adl(&mut self) {
        // Take ownership of pending_liquidations to avoid borrow issues
        let liquidations = std::mem::take(&mut self.pending_liquidations);

        for liq in &liquidations {
            // Calculate total loss for ADL consideration:
            // - Position PnL (negative = loss)
            // - Negative insurance_fund_delta means account went underwater
            // Both should be considered for ADL since they represent losses to cover
            let underwater_loss = if liq.insurance_fund_delta < 0 {
                liq.insurance_fund_delta
            } else {
                0
            };
            let total_loss = liq.pnl.saturating_add(underwater_loss);

            if total_loss < 0 {
                // Loss - check if ADL is needed
                let mark_price = self.mark_prices.get(&liq.symbol).copied().unwrap_or(0);

                if let Some(adl_summary) = crate::app::adl::process_adl_if_needed(
                    &mut self.accounts,
                    &liq.symbol,
                    total_loss, // Include underwater amount in ADL calculation
                    self.insurance_fund,
                    mark_price,
                    liq.was_long,
                    &liq.address,
                    self.timestamp,
                ) {
                    // ADL absorbed (part of) the loss
                    // Insurance fund takes remaining loss after ADL
                    let remaining_loss = total_loss.saturating_add(adl_summary.total_absorbed);
                    self.insurance_fund = self.insurance_fund.saturating_add(remaining_loss);
                    self.pending_adl_events.extend(adl_summary.events);
                } else {
                    // No ADL needed - insurance fund takes the total loss
                    self.insurance_fund = self.insurance_fund.saturating_add(total_loss);
                }

                // Positive remaining balance still goes to insurance fund
                if liq.insurance_fund_delta > 0 {
                    self.insurance_fund = self.insurance_fund.saturating_add(liq.insurance_fund_delta);
                }
            } else {
                // No position loss - handle insurance_fund_delta
                // Positive = remaining balance goes to insurance fund
                // Negative = should not happen in profit case, but handle defensively
                if liq.insurance_fund_delta != 0 {
                    self.insurance_fund = self.insurance_fund.saturating_add(liq.insurance_fund_delta);
                }
                // Profit from position also goes to insurance fund
                if liq.pnl > 0 {
                    self.insurance_fund = self.insurance_fund.saturating_add(liq.pnl);
                }
            }
        }

        // Restore the liquidations for event emission
        self.pending_liquidations = liquidations;

        // CRITICAL-5: Ensure insurance fund never goes negative after ADL processing.
        // If ADL couldn't fully cover losses, cap fund at zero to prevent negative balance.
        // A negative fund would cause incorrect accounting in subsequent liquidations.
        if self.insurance_fund < 0 {
            tracing::warn!(
                fund = self.insurance_fund,
                "Insurance fund went negative after liquidations, flooring at zero"
            );
            self.insurance_fund = 0;
        }

        // CRITICAL-5: Warn when fund drops below warning threshold
        if self.insurance_fund < super::INSURANCE_FUND_WARNING_THRESHOLD && self.insurance_fund > 0 {
            tracing::warn!(
                fund = self.insurance_fund,
                threshold = super::INSURANCE_FUND_WARNING_THRESHOLD,
                "Insurance fund below warning threshold"
            );
        }
    }

    /// Process funding rate sampling and application
    fn process_funding(&mut self) {
        // Collect symbols to process (avoid borrow issues)
        let symbols: Vec<(Symbol, MarketConfig)> = self
            .configs
            .iter()
            .map(|(s, c)| (s.clone(), c.clone()))
            .collect();

        for (symbol, config) in symbols {
            // Get index price (oracle with mark price fallback)
            let index_price = self.index_price(&symbol).unwrap_or(0);
            if index_price == 0 {
                continue;
            }

            // Sample premium from orderbook
            if let Some(book) = self.orderbooks.get(&symbol) {
                let premium = crate::app::funding::sample_premium(book, index_price);
                self.premium_samples
                    .entry(symbol.clone())
                    .or_default()
                    .push_back(premium);

                // Keep only samples for the funding interval (~1 hour of blocks)
                // At 100ms blocks, 1 hour = 36000 blocks
                let max_samples = (config.funding_interval_ms / 100).max(1) as usize;
                let samples = self.premium_samples.get_mut(&symbol).unwrap();
                while samples.len() > max_samples {
                    samples.pop_front();
                }
            }

            // Check if funding interval has elapsed
            let last_funding = self.last_funding_times.get(&symbol).copied().unwrap_or(0);
            if self.timestamp >= last_funding + config.funding_interval_ms {
                // Calculate average premium
                let samples: Vec<i64> = self
                    .premium_samples
                    .get(&symbol)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                let avg_premium = crate::app::funding::average_premium(&samples);

                // Calculate funding rate
                let funding_rate = crate::app::funding::calculate_funding_rate(
                    avg_premium,
                    config.interest_rate_bps,
                    config.max_funding_rate_bps,
                );

                // Apply funding to all positions
                let index_price = self.index_price(&symbol).unwrap_or(0);
                let result = crate::app::funding::apply_funding(
                    &mut self.accounts,
                    &symbol,
                    funding_rate,
                    index_price,
                    self.timestamp,
                );

                // Funding affects all accounts with positions - mark all dirty
                self.mark_all_accounts_dirty();
                self.mark_globals_dirty();

                // Update state
                self.current_funding_rates
                    .insert(symbol.clone(), funding_rate);
                self.last_funding_times
                    .insert(symbol.clone(), self.timestamp);
                self.pending_funding.push(result);

                // Clear premium samples for next period
                self.premium_samples.remove(&symbol);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{orderbook::Side, OrderType, Transaction};

    #[test]
    fn test_deposit_and_order() {
        let mut state = AppState::new();

        // Deposit
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000, // $1M in cents
            })
            .unwrap();

        assert_eq!(state.account("alice").unwrap().balance, 100_000_000);

        // Place order
        let fills = state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000, // $50,000
                size: 100_000_000, // 1 BTC
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        assert!(fills.is_empty()); // No counterparty
        assert!(state.orderbook("BTC-USDT").unwrap().best_bid().is_some());
    }

    #[test]
    fn test_matching() {
        let mut state = AppState::new();

        // Alice deposits and bids
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // Bob deposits and asks (should match)
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            })
            .unwrap();

        let fills = state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 4_900_000, // Below bid
                size: 50_000_000, // 0.5 BTC
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50_000_000);
        assert_eq!(fills[0].price, 5_000_000); // At bid price

        // Check positions
        let alice_pos = state.account("alice").unwrap().position("BTC-USDT");
        assert_eq!(alice_pos.size, 50_000_000); // Long 0.5 BTC

        let bob_pos = state.account("bob").unwrap().position("BTC-USDT");
        assert_eq!(bob_pos.size, -50_000_000); // Short 0.5 BTC
    }

    #[test]
    fn test_state_hash_deterministic() {
        let mut state = AppState::new();

        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();

        let hash1 = state.compute_state_hash();
        let hash2 = state.compute_state_hash();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_trade_history() {
        let mut state = AppState::new();

        // Setup: Alice bids, Bob asks -> should match and create trade
        state
            .execute_tx(Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            })
            .unwrap();
        state
            .execute_tx(Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            })
            .unwrap();

        // Alice places bid
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // No trades yet
        assert!(state.get_trades("BTC-USDT", 10).is_empty());

        // Bob places ask (matches Alice's bid)
        state
            .execute_tx(Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 4_900_000,
                size: 50_000_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            })
            .unwrap();

        // Now we should have 1 trade
        let trades = state.get_trades("BTC-USDT", 10);
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].price, 5_000_000);
        assert_eq!(trades[0].size, 50_000_000);

        // Unknown symbol returns empty
        assert!(state.get_trades("ETH-USDT", 10).is_empty());
    }
}
