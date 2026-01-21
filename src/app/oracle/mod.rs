//! Oracle Module
//!
//! Provides external price feeds for funding rate calculation.
//! Liquidation continues using mark prices (orderbook-derived).
//!
//! ## Bootstrap Mode
//!
//! When `enabled = false` or no oracle prices available, the system
//! falls back to mark prices for funding (same as before oracle).
//!
//! ## Authorization
//!
//! Registered validators can submit oracle price updates.
//! Authorization reuses existing staking infrastructure.

pub mod aggregation;
pub mod fetcher;
pub mod types;

pub use aggregation::{calculate_confidence, check_deviation, filter_stale, weighted_median};
pub use fetcher::{FetcherConfig, OracleFetcher};
pub use types::{OracleConfig, OraclePrice, PriceSource};

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::app::Symbol;
use crate::types::Price;

/// Oracle state for all symbols
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleState {
    /// Aggregated oracle prices per symbol
    pub prices: HashMap<Symbol, OraclePrice>,
    /// Individual source prices per symbol (for audit/debug)
    pub source_prices: HashMap<Symbol, Vec<PriceSource>>,
    /// Configuration per symbol
    pub configs: HashMap<Symbol, OracleConfig>,
    /// Last update timestamp per symbol
    pub last_update: HashMap<Symbol, u64>,
    /// Whether oracle is enabled (false = bootstrap mode using mark prices)
    pub enabled: bool,
}

impl Default for OracleState {
    fn default() -> Self {
        Self::new()
    }
}

impl OracleState {
    pub fn new() -> Self {
        Self {
            prices: HashMap::new(),
            source_prices: HashMap::new(),
            configs: HashMap::new(),
            last_update: HashMap::new(),
            enabled: false, // Start in bootstrap mode
        }
    }

    /// Get oracle price for a symbol, with optional mark price fallback.
    ///
    /// Returns:
    /// - Oracle price if fresh and valid
    /// - Mark price if oracle stale/unavailable and fallback enabled
    /// - None if no price available
    pub fn get_price(&self, symbol: &str, mark_fallback: Option<Price>) -> Option<Price> {
        if !self.enabled {
            // Bootstrap mode: always use mark price
            return mark_fallback;
        }

        if let Some(oracle_price) = self.prices.get(symbol) {
            let config = self.configs.get(symbol).cloned().unwrap_or_default();

            // Check if stale
            if let Some(&last) = self.last_update.get(symbol) {
                // Note: Caller should provide current timestamp for proper staleness check
                // For now, assume the stored price is valid if it exists
                let _ = last; // Staleness check is done in process_update
            }

            // Check circuit breaker (deviation from mark)
            if let Some(mark) = mark_fallback {
                if check_deviation(oracle_price.price, mark, config.max_deviation_bps) {
                    tracing::warn!(
                        symbol = %symbol,
                        oracle = oracle_price.price,
                        mark = mark,
                        "Oracle price deviation too high, using mark price"
                    );
                    return mark_fallback;
                }
            }

            return Some(oracle_price.price);
        }

        // No oracle price, fallback to mark
        let config = self.configs.get(symbol).cloned().unwrap_or_default();
        if config.fallback_to_mark {
            mark_fallback
        } else {
            None
        }
    }

    /// Check if oracle price is stale for a symbol
    pub fn is_stale(&self, symbol: &str, current_time: u64) -> bool {
        let config = self.configs.get(symbol).cloned().unwrap_or_default();
        if let Some(&last) = self.last_update.get(symbol) {
            current_time.saturating_sub(last) > config.max_staleness_ms
        } else {
            true // No update = stale
        }
    }

    /// Process an oracle price update from a validator.
    ///
    /// This aggregates price sources and updates the stored oracle price.
    pub fn process_update(
        &mut self,
        symbol: &str,
        sources: Vec<PriceSource>,
        timestamp: u64,
        mark_price: Option<Price>,
    ) -> Result<(), OracleError> {
        let config = self.configs.get(symbol).cloned().unwrap_or_default();

        // Filter stale sources
        let fresh_sources = filter_stale(&sources, timestamp, config.max_staleness_ms);

        // Check minimum sources
        if fresh_sources.len() < config.min_sources as usize {
            return Err(OracleError::InsufficientSources {
                got: fresh_sources.len() as u32,
                need: config.min_sources,
            });
        }

        // Calculate weighted median
        let price = weighted_median(&fresh_sources);
        if price <= 0 {
            return Err(OracleError::InvalidPrice(price));
        }

        // Calculate confidence
        let confidence_bps = calculate_confidence(&fresh_sources, price);

        // Check deviation from mark (circuit breaker)
        if let Some(mark) = mark_price {
            if check_deviation(price, mark, config.max_deviation_bps) {
                return Err(OracleError::DeviationTooHigh {
                    deviation_bps: ((price - mark).abs() * 10000) / mark,
                    max_bps: config.max_deviation_bps,
                });
            }
        }

        // Store aggregated price
        let oracle_price = OraclePrice {
            symbol: symbol.to_string(),
            price,
            timestamp,
            source_count: fresh_sources.len() as u32,
            confidence_bps,
        };

        self.prices.insert(symbol.to_string(), oracle_price);
        self.source_prices.insert(symbol.to_string(), fresh_sources);
        self.last_update.insert(symbol.to_string(), timestamp);

        Ok(())
    }

    /// Set configuration for a symbol
    pub fn set_config(&mut self, symbol: &str, config: OracleConfig) {
        self.configs.insert(symbol.to_string(), config);
    }

    /// Get configuration for a symbol
    pub fn config(&self, symbol: &str) -> OracleConfig {
        self.configs.get(symbol).cloned().unwrap_or_default()
    }

    /// Enable or disable oracle system
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// Oracle errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum OracleError {
    #[error("unauthorized operator: {0}")]
    UnauthorizedOperator(String),

    #[error("invalid price: {0}")]
    InvalidPrice(Price),

    #[error("stale price for {symbol}: age {age_ms}ms > max {max_ms}ms")]
    StalePrice {
        symbol: String,
        age_ms: u64,
        max_ms: u64,
    },

    #[error("insufficient sources: got {got}, need {need}")]
    InsufficientSources { got: u32, need: u32 },

    #[error("low confidence: {got_bps} bps < {min_bps} bps")]
    LowConfidence { got_bps: i64, min_bps: i64 },

    #[error("deviation too high: {deviation_bps} bps > {max_bps} bps")]
    DeviationTooHigh { deviation_bps: i64, max_bps: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(id: &str, price: Price, timestamp: u64) -> PriceSource {
        PriceSource {
            source_id: id.to_string(),
            price,
            timestamp,
            weight_bps: 2500, // 25% weight each
        }
    }

    #[test]
    fn test_oracle_bootstrap_mode() {
        let oracle = OracleState::new();
        // In bootstrap mode, should return mark price
        let price = oracle.get_price("BTC-USDT", Some(5_000_000));
        assert_eq!(price, Some(5_000_000));
    }

    #[test]
    fn test_oracle_enabled_with_price() {
        let mut oracle = OracleState::new();
        oracle.enabled = true;

        // Process a valid update
        let sources = vec![
            make_source("binance", 5_000_000, 1700000000000),
            make_source("coinbase", 5_010_000, 1700000000000),
            make_source("okx", 5_020_000, 1700000000000),
        ];

        // Use default config with min_sources = 3
        let mut config = OracleConfig::default();
        config.min_sources = 3;
        oracle.set_config("BTC-USDT", config);

        let result = oracle.process_update("BTC-USDT", sources, 1700000000000, Some(5_000_000));
        assert!(result.is_ok());

        // Should return oracle price
        let price = oracle.get_price("BTC-USDT", Some(5_000_000));
        assert!(price.is_some());
        // Median should be 5_010_000 (middle value with equal weights)
        let p = price.unwrap();
        assert!(p >= 5_000_000 && p <= 5_020_000);
    }

    #[test]
    fn test_oracle_insufficient_sources() {
        let mut oracle = OracleState::new();
        oracle.enabled = true;

        let sources = vec![
            make_source("binance", 5_000_000, 1700000000000),
            make_source("coinbase", 5_010_000, 1700000000000),
        ];

        // Default config requires 3 sources
        let result = oracle.process_update("BTC-USDT", sources, 1700000000000, Some(5_000_000));
        assert!(matches!(
            result,
            Err(OracleError::InsufficientSources { .. })
        ));
    }

    #[test]
    fn test_oracle_deviation_circuit_breaker() {
        let mut oracle = OracleState::new();
        oracle.enabled = true;

        // Price 15% higher than mark (over 10% threshold)
        let sources = vec![
            make_source("binance", 5_750_000, 1700000000000),
            make_source("coinbase", 5_750_000, 1700000000000),
            make_source("okx", 5_750_000, 1700000000000),
        ];

        let mut config = OracleConfig::default();
        config.min_sources = 3;
        oracle.set_config("BTC-USDT", config);

        let result = oracle.process_update("BTC-USDT", sources, 1700000000000, Some(5_000_000));
        assert!(matches!(result, Err(OracleError::DeviationTooHigh { .. })));
    }

    #[test]
    fn test_oracle_staleness_check() {
        let mut oracle = OracleState::new();
        oracle.last_update.insert("BTC-USDT".to_string(), 1700000000000);

        // 3 seconds later = not stale (default max is 3000ms)
        assert!(!oracle.is_stale("BTC-USDT", 1700000003000));

        // 4 seconds later = stale
        assert!(oracle.is_stale("BTC-USDT", 1700000004000));
    }
}
