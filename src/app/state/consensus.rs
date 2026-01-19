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
    pub(crate) fn compute_state_hash(&self) -> Hash {
        let mut hasher = Sha256::new();

        // Hash all orderbooks (sorted by symbol for determinism)
        let mut symbols: Vec<_> = self.orderbooks.keys().collect();
        symbols.sort();

        for symbol in symbols {
            if let Some(book) = self.orderbooks.get(symbol) {
                hasher.update(symbol.as_bytes());
                hasher.update(book.best_bid().unwrap_or(0).to_le_bytes());
                hasher.update(book.best_ask().unwrap_or(0).to_le_bytes());
                hasher.update(book.last_price().to_le_bytes());
            }
        }

        // Hash mark prices
        for (symbol, price) in &self.mark_prices {
            hasher.update(symbol.as_bytes());
            hasher.update(price.to_le_bytes());
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
        }
    }

    /// Restore state from snapshot (for recovery)
    pub fn from_snapshot(snapshot: crate::storage::AppSnapshot) -> Self {
        // Extract fields from snapshot (consuming it)
        let mark_prices = snapshot.mark_prices_map();
        let timestamp = snapshot.timestamp;
        let insurance_fund = snapshot.insurance_fund;
        let funding_rates = snapshot.funding_rates_map();
        let last_funding_times = snapshot.last_funding_times_map();

        // Restore staking state if present
        let mut staking = snapshot.staking.unwrap_or_else(StakingState::new);
        staking.rebuild_index(); // Rebuild transient indexes

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
        // Get pending transactions (without removing them yet)
        // This is called before propose, actual removal happens on commit
        // For now, just return empty - real impl would serialize pending txs
        vec![]
    }

    fn execute(&mut self, block: &Block) -> Hash {
        self.timestamp = block.timestamp;
        self.current_view = block.view;

        // Clear pending events from previous block
        self.pending_fills.clear();
        self.pending_order_updates.clear();
        self.pending_liquidations.clear();
        self.pending_funding.clear();
        self.pending_staking_events.clear();

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

        // Get transactions for this block from mempool
        let txs = self.mempool.prepare_block(1000);

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

        // Check and execute liquidations after all transactions
        let liquidations = crate::app::liquidation::check_and_liquidate(
            &mut self.accounts,
            &self.mark_prices,
        );

        // Process liquidation results
        for liq in &liquidations {
            self.insurance_fund += liq.pnl;
        }
        self.pending_liquidations = liquidations;

        // === Funding Rate Logic ===
        self.process_funding();

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
            // Get index price (using mark price as bootstrap index)
            let index_price = self.mark_prices.get(&symbol).copied().unwrap_or(0);
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
                let index_price = self.mark_prices.get(&symbol).copied().unwrap_or(0);
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
