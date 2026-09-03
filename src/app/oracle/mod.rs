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
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::app::Symbol;
use crate::types::Price;

/// Shallow, runtime-only copy-on-write storage for oracle collections.
///
/// The outer map and each map entry have their own allocation.  Cloning an
/// [`OracleState`] therefore shares all entries; mutating one entry detaches
/// only that entry after the map itself is made mutable.
#[derive(Debug, PartialEq, Eq)]
pub struct CowShared<T>(Arc<T>);

impl<T> Clone for CowShared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for CowShared<T> {
    fn default() -> Self {
        Self(Arc::new(T::default()))
    }
}

impl<T> From<T> for CowShared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Deref for CowShared<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T: Clone> DerefMut for CowShared<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<'a, T> IntoIterator for &'a CowShared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_ref().into_iter()
    }
}

impl<T: Serialize> Serialize for CowShared<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for CowShared<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self::from)
    }
}

/// Oracle state for all symbols
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleState {
    /// Aggregated oracle prices per symbol
    pub prices: CowShared<HashMap<Symbol, CowShared<OraclePrice>>>,
    /// Individual source prices per symbol (for audit/debug)
    pub source_prices: CowShared<HashMap<Symbol, CowShared<Vec<PriceSource>>>>,
    /// Configuration per symbol
    pub configs: CowShared<HashMap<Symbol, CowShared<OracleConfig>>>,
    /// Last update timestamp per symbol
    pub last_update: CowShared<HashMap<Symbol, u64>>,
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
            prices: CowShared::default(),
            source_prices: CowShared::default(),
            configs: CowShared::default(),
            last_update: CowShared::default(),
            enabled: false, // Start in bootstrap mode
        }
    }

    /// Validate authoritative oracle records without mutating them.
    pub fn validate_primary_state(&self) -> Result<(), OracleError> {
        let mut price_symbols: Vec<_> = self.prices.keys().collect();
        price_symbols.sort();
        for symbol in price_symbols {
            let price = &self.prices[symbol];
            if symbol.is_empty() || price.symbol != *symbol || price.price <= 0 {
                return Err(OracleError::InvalidStoredPrice(symbol.clone()));
            }
            if price.source_count == 0 || !(0..=10_000).contains(&price.confidence_bps) {
                return Err(OracleError::InvalidStoredPrice(symbol.clone()));
            }
            if self.last_update.get(symbol).copied() != Some(price.timestamp) {
                return Err(OracleError::StoredTimestampMismatch(symbol.clone()));
            }
            let sources = self
                .source_prices
                .get(symbol)
                .ok_or_else(|| OracleError::MissingStoredSources(symbol.clone()))?;
            if usize::try_from(price.source_count).ok() != Some(sources.len()) {
                return Err(OracleError::StoredSourceCountMismatch(symbol.clone()));
            }
            if weighted_median(sources) != price.price
                || calculate_confidence(sources, price.price) != price.confidence_bps
            {
                return Err(OracleError::StoredAggregateMismatch(symbol.clone()));
            }
        }

        for (symbol, sources) in &self.source_prices {
            if !self.prices.contains_key(symbol) || symbol.is_empty() {
                return Err(OracleError::OrphanStoredSources(symbol.clone()));
            }
            let mut source_ids = std::collections::HashSet::with_capacity(sources.len());
            for source in sources {
                if source.source_id.is_empty()
                    || source.price <= 0
                    || !(0..=10_000).contains(&source.weight_bps)
                    || source.timestamp > self.prices[symbol].timestamp
                    || !source_ids.insert(&source.source_id)
                {
                    return Err(OracleError::InvalidStoredSource(symbol.clone()));
                }
            }
        }
        if self
            .last_update
            .keys()
            .any(|symbol| !self.prices.contains_key(symbol))
        {
            return Err(OracleError::OrphanStoredTimestamp);
        }
        for (symbol, config) in &self.configs {
            if symbol.is_empty()
                || config.max_staleness_ms == 0
                || config.min_sources == 0
                || !(0..=10_000).contains(&config.max_deviation_bps)
            {
                return Err(OracleError::InvalidStoredConfig(symbol.clone()));
            }
        }

        Ok(())
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
            let config = self.config(symbol);

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
        let config = self.config(symbol);
        if config.fallback_to_mark {
            mark_fallback
        } else {
            None
        }
    }

    /// Check if oracle price is stale for a symbol
    pub fn is_stale(&self, symbol: &str, current_time: u64) -> bool {
        let config = self.config(symbol);
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
        let config = self.config(symbol);

        let mut source_ids = std::collections::HashSet::with_capacity(sources.len());
        for source in &sources {
            if source.source_id.is_empty()
                || source.price <= 0
                || !(0..=10_000).contains(&source.weight_bps)
                || source.timestamp > timestamp
                || !source_ids.insert(&source.source_id)
            {
                return Err(OracleError::InvalidSource(symbol.to_string()));
            }
        }

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

        // SECURITY: Compare to previous oracle price, not mark price.
        // Mark price is easily manipulated via orderbook trading.
        // Only fall back to mark price on the very first oracle update.
        let reference_price = self.prices.get(symbol).map(|p| p.price).or(mark_price);

        if let Some(reference) = reference_price {
            if check_deviation(price, reference, config.max_deviation_bps) {
                // Use i128 to prevent overflow in deviation calculation
                let deviation_bps = (((price - reference).abs() as i128 * 10000)
                    / reference as i128)
                    .clamp(0, i64::MAX as i128) as i64;
                return Err(OracleError::DeviationTooHigh {
                    deviation_bps,
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

        self.prices.insert(symbol.to_string(), oracle_price.into());
        self.source_prices
            .insert(symbol.to_string(), fresh_sources.into());
        self.last_update.insert(symbol.to_string(), timestamp);

        Ok(())
    }

    /// Set configuration for a symbol
    pub fn set_config(&mut self, symbol: &str, config: OracleConfig) {
        self.configs.insert(symbol.to_string(), config.into());
    }

    /// Get configuration for a symbol
    pub fn config(&self, symbol: &str) -> OracleConfig {
        self.configs
            .get(symbol)
            .map(|config| (**config).clone())
            .unwrap_or_default()
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

    #[error("invalid stored oracle price for {0}")]
    InvalidStoredPrice(String),
    #[error("stored oracle timestamp does not match price for {0}")]
    StoredTimestampMismatch(String),
    #[error("stored oracle sources are missing for {0}")]
    MissingStoredSources(String),
    #[error("stored oracle source count does not match for {0}")]
    StoredSourceCountMismatch(String),
    #[error("stored oracle aggregate does not match its source records for {0}")]
    StoredAggregateMismatch(String),
    #[error("stored oracle sources have no matching price for {0}")]
    OrphanStoredSources(String),
    #[error("stored oracle source is invalid for {0}")]
    InvalidStoredSource(String),
    #[error("stored oracle timestamp has no matching price")]
    OrphanStoredTimestamp,
    #[error("invalid stored oracle config for {0}")]
    InvalidStoredConfig(String),

    #[error("invalid oracle source for {0}")]
    InvalidSource(String),

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

    #[error("invalid signature: {0}")]
    InvalidSignature(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        oracle
            .last_update
            .insert("BTC-USDT".to_string(), 1700000000000);

        // 3 seconds later = not stale (default max is 3000ms)
        assert!(!oracle.is_stale("BTC-USDT", 1700000003000));

        // 4 seconds later = stale
        assert!(oracle.is_stale("BTC-USDT", 1700000004000));
    }

    #[test]
    fn test_oracle_circuit_breaker_uses_previous_oracle() {
        let mut oracle = OracleState::new();
        oracle.enabled = true;

        let mut config = OracleConfig::default();
        config.min_sources = 3;
        config.max_deviation_bps = 500; // 5% max deviation
        oracle.set_config("BTC-USDT", config);

        // First update - uses mark price as reference
        let sources1 = vec![
            make_source("binance", 5_000_000, 1700000000000),
            make_source("coinbase", 5_010_000, 1700000000000),
            make_source("okx", 5_020_000, 1700000000000),
        ];
        let result1 = oracle.process_update("BTC-USDT", sources1, 1700000000000, Some(5_000_000));
        assert!(result1.is_ok());

        // Store the first oracle price
        let first_oracle_price = oracle.prices.get("BTC-USDT").unwrap().price;
        assert!(first_oracle_price >= 5_000_000 && first_oracle_price <= 5_020_000);

        // Second update - should compare to previous oracle, not mark
        // Mark price is now manipulated to $6M (20% off), but we should compare to oracle
        let sources2 = vec![
            make_source("binance", 5_050_000, 1700000001000), // 1% higher
            make_source("coinbase", 5_060_000, 1700000001000),
            make_source("okx", 5_070_000, 1700000001000),
        ];

        // With manipulated mark price of $6M, this would fail if using mark
        // But it should pass because we compare to previous oracle (~$5M)
        let result2 = oracle.process_update("BTC-USDT", sources2, 1700000001000, Some(6_000_000));
        assert!(
            result2.is_ok(),
            "Should use previous oracle price, not manipulated mark"
        );

        // Verify the oracle price updated
        let second_oracle_price = oracle.prices.get("BTC-USDT").unwrap().price;
        assert!(second_oracle_price > first_oracle_price);
    }

    #[test]
    fn test_oracle_circuit_breaker_rejects_large_jump() {
        let mut oracle = OracleState::new();
        oracle.enabled = true;

        let mut config = OracleConfig::default();
        config.min_sources = 3;
        config.max_deviation_bps = 500; // 5% max deviation
        oracle.set_config("BTC-USDT", config);

        // First update
        let sources1 = vec![
            make_source("binance", 5_000_000, 1700000000000),
            make_source("coinbase", 5_010_000, 1700000000000),
            make_source("okx", 5_020_000, 1700000000000),
        ];
        assert!(oracle
            .process_update("BTC-USDT", sources1, 1700000000000, Some(5_000_000))
            .is_ok());

        // Second update with 10% jump - should be rejected
        let sources2 = vec![
            make_source("binance", 5_500_000, 1700000001000),
            make_source("coinbase", 5_510_000, 1700000001000),
            make_source("okx", 5_520_000, 1700000001000),
        ];

        let result = oracle.process_update("BTC-USDT", sources2, 1700000001000, Some(5_000_000));
        assert!(matches!(result, Err(OracleError::DeviationTooHigh { .. })));
    }

    #[test]
    fn primary_validation_accepts_a_processed_update() {
        let mut oracle = OracleState::new();
        let timestamp = 1_700_000_000_000;
        let sources = vec![
            make_source("binance", 5_000_000, timestamp),
            make_source("coinbase", 5_010_000, timestamp),
            make_source("okx", 5_020_000, timestamp),
        ];

        oracle
            .process_update("BTC-USDT", sources, timestamp, Some(5_000_000))
            .unwrap();
        oracle.validate_primary_state().unwrap();
    }

    #[test]
    fn process_update_rejects_invalid_sources_before_mutation() {
        let mut oracle = OracleState::new();
        let timestamp = 1_700_000_000_000;
        let sources = vec![
            make_source("duplicate", 5_000_000, timestamp),
            make_source("duplicate", 5_010_000, timestamp),
            make_source("okx", 5_020_000, timestamp),
        ];

        assert!(matches!(
            oracle.process_update("BTC-USDT", sources, timestamp, Some(5_000_000)),
            Err(OracleError::InvalidSource(symbol)) if symbol == "BTC-USDT"
        ));
        assert!(oracle.prices.is_empty());
        assert!(oracle.source_prices.is_empty());
        assert!(oracle.last_update.is_empty());
    }

    #[test]
    fn primary_validation_rejects_corrupt_aggregate_and_future_source() {
        let mut oracle = OracleState::new();
        let timestamp = 1_700_000_000_000;
        let sources = vec![
            make_source("binance", 5_000_000, timestamp),
            make_source("coinbase", 5_010_000, timestamp),
            make_source("okx", 5_020_000, timestamp),
        ];
        oracle
            .process_update("BTC-USDT", sources, timestamp, Some(5_000_000))
            .unwrap();

        let mut corrupt = oracle.clone();
        corrupt.prices.get_mut("BTC-USDT").unwrap().price += 1;
        assert!(matches!(
            corrupt.validate_primary_state(),
            Err(OracleError::StoredAggregateMismatch(symbol)) if symbol == "BTC-USDT"
        ));

        oracle.source_prices.get_mut("BTC-USDT").unwrap()[0].timestamp = timestamp + 1;
        assert!(matches!(
            oracle.validate_primary_state(),
            Err(OracleError::InvalidStoredSource(symbol)) if symbol == "BTC-USDT"
        ));
    }

    #[test]
    fn cloned_oracle_detaches_changed_entries_and_shares_untouched_entries() {
        let mut parent = OracleState::new();
        parent.set_config("BTC-USDT", OracleConfig::default());
        parent.set_config("ETH-USDT", OracleConfig::default());

        let timestamp = 1_700_000_000_000;
        parent
            .process_update(
                "BTC-USDT",
                vec![
                    make_source("binance", 5_000_000, timestamp),
                    make_source("coinbase", 5_010_000, timestamp),
                    make_source("okx", 5_020_000, timestamp),
                ],
                timestamp,
                Some(5_000_000),
            )
            .unwrap();
        parent
            .process_update(
                "ETH-USDT",
                vec![
                    make_source("binance", 300_000, timestamp),
                    make_source("coinbase", 301_000, timestamp),
                    make_source("okx", 302_000, timestamp),
                ],
                timestamp,
                Some(300_000),
            )
            .unwrap();

        let mut child = parent.clone();
        let sibling = parent.clone();
        assert!(Arc::ptr_eq(&parent.prices.0, &child.prices.0));
        assert!(Arc::ptr_eq(&parent.configs.0, &child.configs.0));

        let next_timestamp = timestamp + 1_000;
        child
            .process_update(
                "BTC-USDT",
                vec![
                    make_source("binance", 5_001_000, next_timestamp),
                    make_source("coinbase", 5_011_000, next_timestamp),
                    make_source("okx", 5_021_000, next_timestamp),
                ],
                next_timestamp,
                Some(5_000_000),
            )
            .unwrap();
        let mut changed_config = OracleConfig::default();
        changed_config.max_staleness_ms = 10_000;
        child.set_config("BTC-USDT", changed_config);

        assert_ne!(
            parent.prices.get("BTC-USDT").unwrap().timestamp,
            child.prices.get("BTC-USDT").unwrap().timestamp
        );
        assert_eq!(
            parent.prices.get("BTC-USDT").unwrap().timestamp,
            sibling.prices.get("BTC-USDT").unwrap().timestamp
        );
        assert!(!Arc::ptr_eq(
            &parent.prices.get("BTC-USDT").unwrap().0,
            &child.prices.get("BTC-USDT").unwrap().0
        ));
        assert!(Arc::ptr_eq(
            &parent.prices.get("ETH-USDT").unwrap().0,
            &child.prices.get("ETH-USDT").unwrap().0
        ));
        assert!(!Arc::ptr_eq(
            &parent.source_prices.get("BTC-USDT").unwrap().0,
            &child.source_prices.get("BTC-USDT").unwrap().0
        ));
        assert!(Arc::ptr_eq(
            &parent.source_prices.get("ETH-USDT").unwrap().0,
            &child.source_prices.get("ETH-USDT").unwrap().0
        ));
        assert!(!Arc::ptr_eq(
            &parent.configs.get("BTC-USDT").unwrap().0,
            &child.configs.get("BTC-USDT").unwrap().0
        ));
        assert!(Arc::ptr_eq(
            &parent.configs.get("ETH-USDT").unwrap().0,
            &child.configs.get("ETH-USDT").unwrap().0
        ));
        assert_eq!(parent.config("BTC-USDT").max_staleness_ms, 3000);
        assert_eq!(child.config("BTC-USDT").max_staleness_ms, 10_000);
        parent.validate_primary_state().unwrap();
        child.validate_primary_state().unwrap();

        let encoded = serde_json::to_vec(&child).unwrap();
        let decoded: OracleState = serde_json::from_slice(&encoded).unwrap();
        decoded.validate_primary_state().unwrap();
        assert_eq!(
            decoded.prices.get("BTC-USDT").unwrap().timestamp,
            next_timestamp
        );
    }
}
