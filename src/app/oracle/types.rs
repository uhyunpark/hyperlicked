//! Oracle Types
//!
//! Core types for the oracle price feed system.

use serde::{Deserialize, Serialize};

use crate::app::Symbol;
use crate::types::Price;

/// A price update from a single source (e.g., "binance", "coinbase")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PriceSource {
    /// Source identifier (e.g., "binance", "coinbase", "okx")
    pub source_id: String,
    /// Price in cents (1 USD = 100)
    pub price: Price,
    /// Timestamp in milliseconds
    pub timestamp: u64,
    /// Weight in basis points (0-10000). Higher = more influence in aggregation.
    pub weight_bps: i64,
}

/// Aggregated oracle price for a symbol
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OraclePrice {
    /// Trading symbol (e.g., "BTC-USDT")
    pub symbol: Symbol,
    /// Aggregated price in cents
    pub price: Price,
    /// Timestamp of aggregation
    pub timestamp: u64,
    /// Number of sources used in aggregation
    pub source_count: u32,
    /// Confidence in basis points (higher = less spread among sources)
    pub confidence_bps: i64,
}

/// Oracle configuration per symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleConfig {
    /// Maximum staleness before price is considered stale (ms)
    /// Default: 3000 (3 seconds)
    pub max_staleness_ms: u64,
    /// Minimum number of sources required for valid aggregation
    /// Default: 3
    pub min_sources: u32,
    /// Maximum deviation from mark price before circuit breaker triggers (bps)
    /// Default: 1000 (10%)
    pub max_deviation_bps: i64,
    /// Whether to fall back to mark price if oracle is unavailable
    /// Default: true
    pub fallback_to_mark: bool,
}

impl Default for OracleConfig {
    fn default() -> Self {
        Self {
            max_staleness_ms: 3000,      // 3 seconds
            min_sources: 3,               // Need 3+ sources
            max_deviation_bps: 1000,     // 10% circuit breaker
            fallback_to_mark: true,      // Bootstrap mode
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_source_serialization() {
        let source = PriceSource {
            source_id: "binance".to_string(),
            price: 5_000_000,
            timestamp: 1700000000000,
            weight_bps: 2500,
        };

        let json = serde_json::to_string(&source).unwrap();
        let parsed: PriceSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, parsed);
    }

    #[test]
    fn test_oracle_price_serialization() {
        let price = OraclePrice {
            symbol: "BTC-USDT".to_string(),
            price: 5_000_000,
            timestamp: 1700000000000,
            source_count: 5,
            confidence_bps: 9500,
        };

        let json = serde_json::to_string(&price).unwrap();
        let parsed: OraclePrice = serde_json::from_str(&json).unwrap();
        assert_eq!(price, parsed);
    }

    #[test]
    fn test_oracle_config_defaults() {
        let config = OracleConfig::default();
        assert_eq!(config.max_staleness_ms, 3000);
        assert_eq!(config.min_sources, 3);
        assert_eq!(config.max_deviation_bps, 1000);
        assert!(config.fallback_to_mark);
    }
}
