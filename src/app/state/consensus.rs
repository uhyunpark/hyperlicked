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
    /// Includes all deterministic state for cross-validator consistency:
    /// - Accounts (balance, locked, nonce)
    /// - Positions (size, entry_price, realized_pnl, cumulative_funding)
    /// - Orderbooks (best bid/ask, last price)
    /// - Mark prices
    /// - Insurance fund
    /// - Funding rates and last funding times
    /// - Staking state (validators, delegations, epochs)
    /// - Trigger orders
    ///
    /// All collections are sorted by key for determinism.
    pub fn compute_state_hash(&self) -> Hash {
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
        }
    }

    /// Restore state from snapshot (for recovery)
    pub fn from_snapshot(snapshot: crate::storage::AppSnapshot) -> Self {
        use crate::app::oracle::OracleState;

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
            premium_samples: HashMap::new(), // Premium samples are recalculated
            current_funding_rates: funding_rates,
            last_funding_times,
            pending_funding: Vec::new(),
            pending_deposits: Vec::new(),
            candle_manager: CandleManager::new(), // Candles are rebuilt from trades
            staking,
            pending_staking_events: Vec::new(),
            current_view: 0,
            // Trigger orders are restored from snapshot if present
            trigger_orders: HashMap::new(),
            trigger_orders_by_trader: HashMap::new(),
            trigger_orders_by_symbol: HashMap::new(),
            trigger_orders_by_cloid: HashMap::new(),
            trigger_seq: 0,
            pending_trigger_events: Vec::new(),
            pending_adl_events: Vec::new(),
            oracle,
            committed_height: 0, // Will be set by consensus after replay
            prev_day_prices: HashMap::new(),
            day_start: 0,
            day_volume: HashMap::new(),
            day_notional_volume: HashMap::new(),
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

        // Execute each transaction
        for tx in txs {
            match self.execute_tx(tx) {
                Ok(fills) => {
                    // Collect fills for event emission
                    self.pending_fills.extend(fills);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Transaction failed");
                }
            }
        }

        // Commit the proposal (two-phase: finalize removal from mempool)
        // This replaces the old drain_block() approach
        if !tx_hashes.is_empty() {
            self.mempool.commit_proposal(&tx_hashes);
        }

        // Check and execute liquidations after all transactions
        let liquidations = crate::app::liquidation::check_and_liquidate(
            &mut self.accounts,
            &self.mark_prices,
        );

        // Process liquidation results with ADL check
        for liq in &liquidations {
            if liq.pnl < 0 {
                // Loss - check if ADL is needed
                let mark_price = self.mark_prices.get(&liq.symbol).copied().unwrap_or(0);

                if let Some(adl_summary) = crate::app::adl::process_adl_if_needed(
                    &mut self.accounts,
                    &liq.symbol,
                    liq.pnl,
                    self.insurance_fund,
                    mark_price,
                    liq.was_long,
                    &liq.address,
                    self.timestamp,
                ) {
                    // ADL absorbed (part of) the loss
                    // Insurance fund takes remaining loss after ADL
                    let remaining_loss = liq.pnl + adl_summary.total_absorbed;
                    self.insurance_fund += remaining_loss;
                    self.pending_adl_events.extend(adl_summary.events);
                } else {
                    // No ADL needed - insurance fund takes the loss
                    self.insurance_fund += liq.pnl;
                }
            } else {
                // Profit - goes to insurance fund
                self.insurance_fund += liq.pnl;
            }
        }
        self.pending_liquidations = liquidations;

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
