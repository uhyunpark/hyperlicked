//! Oracle Price Fetcher
//!
//! Fetches prices from external CEXs (Binance, OKX, Bybit) and aggregates them.
//! Runs as a background task, polling at configurable intervals.

use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::app::oracle::{OraclePrice, PriceSource};
use crate::types::Price;

/// Price fetcher configuration
#[derive(Debug, Clone)]
pub struct FetcherConfig {
    /// Polling interval in milliseconds
    pub poll_interval_ms: u64,
    /// Request timeout in milliseconds
    pub timeout_ms: u64,
    /// Symbols to fetch (e.g., ["BTC-USDT"])
    pub symbols: Vec<String>,
}

impl Default for FetcherConfig {
    fn default() -> Self {
        Self {
            poll_interval_ms: 3000, // 3 seconds (like Hyperliquid validators)
            timeout_ms: 5000,       // 5 second timeout
            symbols: vec!["BTC-USDT".to_string()],
        }
    }
}

/// Oracle price fetcher
pub struct OracleFetcher {
    client: Client,
    config: FetcherConfig,
}

impl OracleFetcher {
    pub fn new(config: FetcherConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .build()
            .expect("Failed to create HTTP client");

        Self { client, config }
    }

    /// Fetch prices from all sources for a symbol
    pub async fn fetch_prices(&self, symbol: &str) -> Vec<PriceSource> {
        let mut sources = Vec::new();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        // Convert symbol format: "BTC-USDT" -> exchange-specific format
        let (binance_sym, okx_sym, bybit_sym) = Self::convert_symbol(symbol);

        // Fetch from all exchanges in parallel
        let (binance, okx, bybit) = tokio::join!(
            self.fetch_binance(&binance_sym, timestamp),
            self.fetch_okx(&okx_sym, timestamp),
            self.fetch_bybit(&bybit_sym, timestamp),
        );

        if let Some(src) = binance {
            sources.push(src);
        }
        if let Some(src) = okx {
            sources.push(src);
        }
        if let Some(src) = bybit {
            sources.push(src);
        }

        sources
    }

    /// Convert symbol to exchange-specific formats
    fn convert_symbol(symbol: &str) -> (String, String, String) {
        // "BTC-USDT" -> "BTCUSDT" (Binance), "BTC-USDT" (OKX), "BTCUSDT" (Bybit)
        let base = symbol.replace("-", "");
        (
            base.clone(),       // Binance: BTCUSDT
            symbol.to_string(), // OKX: BTC-USDT
            base,               // Bybit: BTCUSDT
        )
    }

    /// Fetch from Binance
    async fn fetch_binance(&self, symbol: &str, timestamp: u64) -> Option<PriceSource> {
        // https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT
        let url = format!(
            "https://api.binance.com/api/v3/ticker/price?symbol={}",
            symbol
        );

        match self.client.get(&url).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!(exchange = "binance", status = %resp.status(), "Failed to fetch price");
                    return None;
                }

                match resp.json::<BinancePrice>().await {
                    Ok(data) => {
                        let price = Self::parse_price(&data.price)?;
                        debug!(exchange = "binance", price, "Fetched price");
                        Some(PriceSource {
                            source_id: "binance".to_string(),
                            price,
                            timestamp,
                            weight_bps: 4286, // Weight 3/7 (Hyperliquid uses 3,2,2 for Binance,OKX,Bybit)
                        })
                    }
                    Err(e) => {
                        warn!(exchange = "binance", error = %e, "Failed to parse response");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(exchange = "binance", error = %e, "Request failed");
                None
            }
        }
    }

    /// Fetch from OKX
    async fn fetch_okx(&self, symbol: &str, timestamp: u64) -> Option<PriceSource> {
        // https://www.okx.com/api/v5/market/ticker?instId=BTC-USDT
        let url = format!("https://www.okx.com/api/v5/market/ticker?instId={}", symbol);

        match self.client.get(&url).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!(exchange = "okx", status = %resp.status(), "Failed to fetch price");
                    return None;
                }

                match resp.json::<OkxResponse>().await {
                    Ok(data) => {
                        if data.data.is_empty() {
                            return None;
                        }
                        let price = Self::parse_price(&data.data[0].last)?;
                        debug!(exchange = "okx", price, "Fetched price");
                        Some(PriceSource {
                            source_id: "okx".to_string(),
                            price,
                            timestamp,
                            weight_bps: 2857, // Weight 2/7 (Hyperliquid uses 3,2,2 for Binance,OKX,Bybit)
                        })
                    }
                    Err(e) => {
                        warn!(exchange = "okx", error = %e, "Failed to parse response");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(exchange = "okx", error = %e, "Request failed");
                None
            }
        }
    }

    /// Fetch from Bybit
    async fn fetch_bybit(&self, symbol: &str, timestamp: u64) -> Option<PriceSource> {
        // https://api.bybit.com/v5/market/tickers?category=spot&symbol=BTCUSDT
        let url = format!(
            "https://api.bybit.com/v5/market/tickers?category=spot&symbol={}",
            symbol
        );

        match self.client.get(&url).send().await {
            Ok(resp) => {
                if !resp.status().is_success() {
                    warn!(exchange = "bybit", status = %resp.status(), "Failed to fetch price");
                    return None;
                }

                match resp.json::<BybitResponse>().await {
                    Ok(data) => {
                        if data.result.list.is_empty() {
                            return None;
                        }
                        let price = Self::parse_price(&data.result.list[0].last_price)?;
                        debug!(exchange = "bybit", price, "Fetched price");
                        Some(PriceSource {
                            source_id: "bybit".to_string(),
                            price,
                            timestamp,
                            weight_bps: 2857, // Weight 2/7 (Hyperliquid uses 3,2,2 for Binance,OKX,Bybit)
                        })
                    }
                    Err(e) => {
                        warn!(exchange = "bybit", error = %e, "Failed to parse response");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(exchange = "bybit", error = %e, "Request failed");
                None
            }
        }
    }

    /// Parse price string to cents (i64)
    fn parse_price(price_str: &str) -> Option<Price> {
        let price_f64: f64 = price_str.parse().ok()?;
        // Convert to cents: $50000.50 -> 5000050
        Some((price_f64 * 100.0).round() as Price)
    }

    /// Aggregate prices into a single oracle price
    pub fn aggregate(&self, symbol: &str, sources: &[PriceSource]) -> Option<OraclePrice> {
        if sources.is_empty() {
            return None;
        }

        let timestamp = sources.iter().map(|s| s.timestamp).max().unwrap_or(0);

        // Weighted median calculation
        let price = crate::app::oracle::weighted_median(sources);
        if price <= 0 {
            return None;
        }

        let confidence_bps = crate::app::oracle::calculate_confidence(sources, price);

        Some(OraclePrice {
            symbol: symbol.to_string(),
            price,
            timestamp,
            source_count: sources.len() as u32,
            confidence_bps,
        })
    }

    /// Get config
    pub fn config(&self) -> &FetcherConfig {
        &self.config
    }
}

// === Exchange API Response Types ===

#[derive(Debug, Deserialize)]
struct BinancePrice {
    price: String,
}

#[derive(Debug, Deserialize)]
struct OkxResponse {
    data: Vec<OkxTicker>,
}

#[derive(Debug, Deserialize)]
struct OkxTicker {
    last: String,
}

#[derive(Debug, Deserialize)]
struct BybitResponse {
    result: BybitResult,
}

#[derive(Debug, Deserialize)]
struct BybitResult {
    list: Vec<BybitTicker>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BybitTicker {
    last_price: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price() {
        assert_eq!(OracleFetcher::parse_price("50000.00"), Some(5_000_000));
        assert_eq!(OracleFetcher::parse_price("50000.50"), Some(5_000_050));
        assert_eq!(OracleFetcher::parse_price("123.456"), Some(12346)); // Rounds
    }

    #[test]
    fn test_convert_symbol() {
        let (binance, okx, bybit) = OracleFetcher::convert_symbol("BTC-USDT");
        assert_eq!(binance, "BTCUSDT");
        assert_eq!(okx, "BTC-USDT");
        assert_eq!(bybit, "BTCUSDT");
    }

    #[tokio::test]
    async fn test_fetcher_creation() {
        let fetcher = OracleFetcher::new(FetcherConfig::default());
        assert_eq!(fetcher.config.poll_interval_ms, 3000);
    }
}
