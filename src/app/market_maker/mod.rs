//! Artificial Market Maker
//!
//! Generates synthetic trading activity for testing and showcase.
//! Multiple strategy groups create realistic orderbook dynamics.
//!
//! ## Features
//!
//! - Uses oracle price as reference for fair value
//! - Multiple strategy types: TightQuoter, WideQuoter, TrendFollower, MeanReverter, NoiseTrader
//! - Configurable intensity presets (low/medium/high)
//! - Deterministic address generation from seed
//!
//! ## Runtime integration
//!
//! The strategies remain a library component. The dev-only `hl-mm` process
//! supplies signer-derived addresses and submits their actions through the
//! canonical `hl-node` HTTP transaction path.

pub mod account;
pub mod config;
pub mod strategy;
pub mod types;

pub use account::MakerAccount;
pub use config::{Intensity, MarketMakerConfig};
pub use strategy::{create_strategy, Strategy};
pub use types::{MarketMakerAction, MarketSnapshot, StrategyType};

use std::collections::VecDeque;

use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::app::{Address, Transaction};
use crate::types::Price;

/// Maximum price history length for MA calculation
const MAX_PRICE_HISTORY: usize = 50;

/// Market maker state
pub struct MarketMakerState {
    /// Configuration
    config: MarketMakerConfig,
    /// Maker accounts with strategies
    accounts: Vec<MakerAccount>,
    /// Price history for strategy decisions
    price_history: VecDeque<Price>,
    /// Random number generator
    rng: StdRng,
    /// Tick counter
    tick_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MarketMakerStateError {
    #[error("market maker address count mismatch: expected {expected}, got {actual}")]
    AddressCount { expected: usize, actual: usize },
}

impl MarketMakerState {
    /// Create new market maker state using deterministic fixture addresses.
    pub fn new(config: MarketMakerConfig) -> Self {
        let mut rng = StdRng::seed_from_u64(config.seed);
        let accounts = Self::create_accounts(&config, &mut rng);

        Self {
            config,
            accounts,
            price_history: VecDeque::with_capacity(MAX_PRICE_HISTORY),
            rng,
            tick_count: 0,
        }
    }

    /// Create market maker state with caller-supplied account addresses.
    ///
    /// The standalone `hl-mm` process owns the private keys for its accounts,
    /// so its strategy state must use those exact signer-derived addresses.
    /// Keeping this constructor separate preserves the deterministic fixture
    /// constructor above for existing unit tests.
    pub fn new_with_addresses(
        config: MarketMakerConfig,
        addresses: Vec<Address>,
    ) -> Result<Self, MarketMakerStateError> {
        let expected = config.total_accounts();
        if addresses.len() != expected {
            return Err(MarketMakerStateError::AddressCount {
                expected,
                actual: addresses.len(),
            });
        }

        let rng = StdRng::seed_from_u64(config.seed);
        let accounts = Self::create_accounts_with_addresses(&config, addresses);

        Ok(Self {
            config,
            accounts,
            price_history: VecDeque::with_capacity(MAX_PRICE_HISTORY),
            rng,
            tick_count: 0,
        })
    }

    /// Create accounts for all strategy types
    fn create_accounts(config: &MarketMakerConfig, _rng: &mut StdRng) -> Vec<MakerAccount> {
        let mut accounts = Vec::new();
        let accounts_per_strategy = config.intensity.accounts_per_strategy();

        for strategy_type in StrategyType::all() {
            let addresses = MakerAccount::generate_addresses(
                config.seed,
                *strategy_type,
                accounts_per_strategy,
            );

            for address in addresses {
                let strategy = create_strategy(*strategy_type);
                accounts.push(MakerAccount::new(address, strategy, *strategy_type));
            }
        }

        accounts
    }

    fn create_accounts_with_addresses(
        config: &MarketMakerConfig,
        addresses: Vec<Address>,
    ) -> Vec<MakerAccount> {
        let mut accounts = Vec::with_capacity(addresses.len());
        let mut addresses = addresses.into_iter();
        let accounts_per_strategy = config.intensity.accounts_per_strategy();

        for strategy_type in StrategyType::all() {
            for _ in 0..accounts_per_strategy {
                let address = addresses
                    .next()
                    .expect("address count was validated before account creation");
                accounts.push(MakerAccount::new(
                    address,
                    create_strategy(*strategy_type),
                    *strategy_type,
                ));
            }
        }

        debug_assert!(addresses.next().is_none());
        accounts
    }

    /// Get initial deposit transactions for all accounts
    pub fn init_deposits(&self) -> Vec<Transaction> {
        self.accounts
            .iter()
            .map(|account| Transaction::Deposit {
                trader: account.address.clone(),
                amount: self.config.initial_deposit,
            })
            .collect()
    }

    /// Get all account addresses (for logging)
    pub fn addresses(&self) -> Vec<&str> {
        self.accounts.iter().map(|a| a.address.as_str()).collect()
    }

    /// Get configuration
    pub fn config(&self) -> &MarketMakerConfig {
        &self.config
    }

    /// Perform one tick of market making
    ///
    /// Returns transactions to inject into mempool
    pub fn tick(
        &mut self,
        oracle_price: Option<Price>,
        best_bid: Option<Price>,
        best_ask: Option<Price>,
        timestamp: u64,
    ) -> Vec<Transaction> {
        self.tick_count += 1;

        // Need oracle price to operate
        let oracle_price = match oracle_price {
            Some(p) if p > 0 => p,
            _ => return vec![],
        };

        // Update price history
        self.price_history.push_back(oracle_price);
        if self.price_history.len() > MAX_PRICE_HISTORY {
            self.price_history.pop_front();
        }

        // Create market snapshot
        let snapshot = MarketSnapshot {
            oracle_price,
            best_bid,
            best_ask,
            price_history: self.price_history.iter().copied().collect(),
            timestamp,
        };

        // Collect transactions from all accounts
        let mut transactions = Vec::new();

        for account in &mut self.accounts {
            let action = account.strategy.decide(&snapshot, &mut self.rng);
            let txs = action.to_transactions(&account.address, &self.config.symbol);
            transactions.extend(txs);
        }

        transactions
    }

    /// Get tick count
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Get account count by strategy
    pub fn strategy_counts(&self) -> Vec<(StrategyType, usize)> {
        let mut counts = std::collections::HashMap::new();
        for account in &self.accounts {
            *counts.entry(account.strategy_type).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }
}

impl std::fmt::Debug for MarketMakerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MarketMakerState")
            .field("accounts", &self.accounts.len())
            .field("tick_count", &self.tick_count)
            .field("intensity", &self.config.intensity)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_maker_creation() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let mm = MarketMakerState::new(config);

        // Should have 12 accounts (6 strategies * 2 accounts each)
        assert_eq!(mm.accounts.len(), 12);
    }

    #[test]
    fn signer_address_constructor_requires_exact_strategy_account_count() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let error = MarketMakerState::new_with_addresses(config, vec!["0x01".to_string()])
            .expect_err("incomplete signer address set must be rejected");
        assert_eq!(
            error,
            MarketMakerStateError::AddressCount {
                expected: 12,
                actual: 1,
            }
        );
    }

    #[test]
    fn test_init_deposits() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let mm = MarketMakerState::new(config);
        let deposits = mm.init_deposits();

        assert_eq!(deposits.len(), 12); // 6 strategies * 2 accounts
        for tx in &deposits {
            if let Transaction::Deposit { amount, .. } = tx {
                assert_eq!(*amount, 100_000_000);
            } else {
                panic!("Expected deposit transaction");
            }
        }
    }

    #[test]
    fn test_tick_without_oracle() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let mut mm = MarketMakerState::new(config);

        // No oracle price - should return empty
        let txs = mm.tick(None, None, None, 1000);
        assert!(txs.is_empty());

        // Zero oracle price - should return empty
        let txs = mm.tick(Some(0), None, None, 1000);
        assert!(txs.is_empty());
    }

    #[test]
    fn test_tick_with_oracle() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let mut mm = MarketMakerState::new(config);

        // Run a few ticks with oracle price
        let oracle_price = 10_000_000; // $100,000 in cents
        for i in 0..10 {
            let _txs = mm.tick(
                Some(oracle_price),
                Some(oracle_price - 100),
                Some(oracle_price + 100),
                1000 + i * 100,
            );
        }

        // After 10 ticks, we should have generated some transactions
        assert!(mm.tick_count >= 10);
    }

    #[test]
    fn test_deterministic_addresses() {
        let config = MarketMakerConfig {
            enabled: true,
            interval_ms: 100,
            intensity: Intensity::Low,
            seed: 12345,
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000,
        };

        let mm1 = MarketMakerState::new(config.clone());
        let mm2 = MarketMakerState::new(config);

        // Same seed should produce same addresses
        let addrs1 = mm1.addresses();
        let addrs2 = mm2.addresses();
        assert_eq!(addrs1, addrs2);
    }
}
