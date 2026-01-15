//! Candle (OHLCV) Aggregation Engine
//!
//! Aggregates trade data into candlestick charts for the trading UI.
//! Supports multiple intervals (1m, 5m, 15m, 1h, 4h, 1d).
//!
//! All values use integer math:
//! - Price: i64 in cents (100 = $1.00)
//! - Size/Volume: i64 in satoshis (100_000_000 = 1 unit)

use std::collections::{HashMap, VecDeque};
use serde::Serialize;
use crate::types::{Price, Size};
use super::Symbol;

/// Maximum number of candles to keep per symbol/interval
const MAX_CANDLES: usize = 500;

/// Supported candle intervals
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interval {
    Min1,
    Min5,
    Min15,
    Hour1,
    Hour4,
    Day1,
}

impl Interval {
    /// Convert interval to milliseconds
    pub fn to_ms(&self) -> u64 {
        match self {
            Interval::Min1 => 60 * 1000,
            Interval::Min5 => 5 * 60 * 1000,
            Interval::Min15 => 15 * 60 * 1000,
            Interval::Hour1 => 60 * 60 * 1000,
            Interval::Hour4 => 4 * 60 * 60 * 1000,
            Interval::Day1 => 24 * 60 * 60 * 1000,
        }
    }

    /// Parse from string (e.g., "1m", "5m", "1h")
    pub fn from_str(s: &str) -> Option<Interval> {
        match s {
            "1m" => Some(Interval::Min1),
            "5m" => Some(Interval::Min5),
            "15m" => Some(Interval::Min15),
            "1h" => Some(Interval::Hour1),
            "4h" => Some(Interval::Hour4),
            "1d" => Some(Interval::Day1),
            _ => None,
        }
    }

    /// Convert to string for API responses
    pub fn as_str(&self) -> &'static str {
        match self {
            Interval::Min1 => "1m",
            Interval::Min5 => "5m",
            Interval::Min15 => "15m",
            Interval::Hour1 => "1h",
            Interval::Hour4 => "4h",
            Interval::Day1 => "1d",
        }
    }
}

/// A single OHLCV candle
#[derive(Debug, Clone, Serialize)]
pub struct Candle {
    /// Bucket start timestamp (ms since epoch)
    pub time: u64,
    /// Opening price (cents)
    pub open: Price,
    /// Highest price (cents)
    pub high: Price,
    /// Lowest price (cents)
    pub low: Price,
    /// Closing price (cents)
    pub close: Price,
    /// Total volume (satoshis)
    pub volume: Size,
    /// Number of trades in this candle
    pub trades: u32,
}

impl Candle {
    /// Create a new candle from a single trade
    fn new(time: u64, price: Price, size: Size) -> Self {
        Self {
            time,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
            trades: 1,
        }
    }

    /// Update candle with a new trade
    fn update(&mut self, price: Price, size: Size) {
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.close = price;
        self.volume += size;
        self.trades += 1;
    }
}

/// Manages candle aggregation for all symbols and intervals
#[derive(Debug, Default)]
pub struct CandleManager {
    /// Candles indexed by (symbol, interval)
    candles: HashMap<(Symbol, Interval), VecDeque<Candle>>,
}

impl CandleManager {
    pub fn new() -> Self {
        Self {
            candles: HashMap::new(),
        }
    }

    /// Get bucket start time for a given timestamp and interval
    fn bucket_time(timestamp: u64, interval: Interval) -> u64 {
        let interval_ms = interval.to_ms();
        (timestamp / interval_ms) * interval_ms
    }

    /// Add a trade and update all interval candles
    pub fn add_trade(&mut self, symbol: &str, price: Price, size: Size, timestamp: u64) {
        // Update candles for all intervals
        for interval in [
            Interval::Min1,
            Interval::Min5,
            Interval::Min15,
            Interval::Hour1,
            Interval::Hour4,
            Interval::Day1,
        ] {
            self.add_trade_to_interval(symbol, price, size, timestamp, interval);
        }
    }

    /// Add a trade to a specific interval's candles
    fn add_trade_to_interval(
        &mut self,
        symbol: &str,
        price: Price,
        size: Size,
        timestamp: u64,
        interval: Interval,
    ) {
        let bucket = Self::bucket_time(timestamp, interval);
        let key = (symbol.to_string(), interval);
        let candles = self.candles.entry(key).or_insert_with(VecDeque::new);

        // Check if we should update the last candle or create a new one
        if let Some(last) = candles.back_mut() {
            if last.time == bucket {
                // Same bucket - update existing candle
                last.update(price, size);
                return;
            }
        }

        // New bucket - create new candle
        candles.push_back(Candle::new(bucket, price, size));

        // Evict old candles if needed
        while candles.len() > MAX_CANDLES {
            candles.pop_front();
        }
    }

    /// Get candles for a symbol and interval
    pub fn get_candles(&self, symbol: &str, interval: Interval, limit: usize) -> Vec<Candle> {
        let key = (symbol.to_string(), interval);
        self.candles
            .get(&key)
            .map(|candles| {
                let start = candles.len().saturating_sub(limit);
                candles.iter().skip(start).cloned().collect()
            })
            .unwrap_or_default()
    }

    /// Get the latest candle for a symbol and interval (for real-time updates)
    pub fn get_latest_candle(&self, symbol: &str, interval: Interval) -> Option<Candle> {
        let key = (symbol.to_string(), interval);
        self.candles.get(&key).and_then(|c| c.back().cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bucket_time() {
        // 1m interval: 60000ms buckets
        assert_eq!(CandleManager::bucket_time(60000, Interval::Min1), 60000);
        assert_eq!(CandleManager::bucket_time(60001, Interval::Min1), 60000);
        assert_eq!(CandleManager::bucket_time(119999, Interval::Min1), 60000);
        assert_eq!(CandleManager::bucket_time(120000, Interval::Min1), 120000);

        // 1h interval: 3600000ms buckets
        assert_eq!(CandleManager::bucket_time(3600000, Interval::Hour1), 3600000);
        assert_eq!(CandleManager::bucket_time(3600001, Interval::Hour1), 3600000);
    }

    #[test]
    fn test_add_trade_creates_candle() {
        let mut manager = CandleManager::new();

        manager.add_trade("BTC-USDT", 5_000_000, 100_000_000, 60000);

        let candles = manager.get_candles("BTC-USDT", Interval::Min1, 10);
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].open, 5_000_000);
        assert_eq!(candles[0].close, 5_000_000);
        assert_eq!(candles[0].volume, 100_000_000);
        assert_eq!(candles[0].trades, 1);
    }

    #[test]
    fn test_add_trade_updates_candle() {
        let mut manager = CandleManager::new();

        // First trade
        manager.add_trade("BTC-USDT", 5_000_000, 100_000_000, 60000);
        // Second trade in same bucket (higher price)
        manager.add_trade("BTC-USDT", 5_100_000, 50_000_000, 60500);
        // Third trade in same bucket (lower price)
        manager.add_trade("BTC-USDT", 4_900_000, 25_000_000, 61000);

        let candles = manager.get_candles("BTC-USDT", Interval::Min1, 10);
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].open, 5_000_000);   // First trade
        assert_eq!(candles[0].high, 5_100_000);   // Highest
        assert_eq!(candles[0].low, 4_900_000);    // Lowest
        assert_eq!(candles[0].close, 4_900_000);  // Last trade
        assert_eq!(candles[0].volume, 175_000_000); // Sum
        assert_eq!(candles[0].trades, 3);
    }

    #[test]
    fn test_add_trade_new_bucket() {
        let mut manager = CandleManager::new();

        // Trade in bucket 1 (0-60000)
        manager.add_trade("BTC-USDT", 5_000_000, 100_000_000, 30000);
        // Trade in bucket 2 (60000-120000)
        manager.add_trade("BTC-USDT", 5_100_000, 50_000_000, 90000);

        let candles = manager.get_candles("BTC-USDT", Interval::Min1, 10);
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].close, 5_000_000);
        assert_eq!(candles[1].close, 5_100_000);
    }

    #[test]
    fn test_interval_parsing() {
        assert_eq!(Interval::from_str("1m"), Some(Interval::Min1));
        assert_eq!(Interval::from_str("5m"), Some(Interval::Min5));
        assert_eq!(Interval::from_str("1h"), Some(Interval::Hour1));
        assert_eq!(Interval::from_str("invalid"), None);
    }
}
