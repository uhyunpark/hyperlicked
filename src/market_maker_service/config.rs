use std::time::Duration;

use anyhow::{bail, Result};

use crate::app::market_maker::Intensity;

use super::client::validate_loopback_url;

/// Configuration for the standalone development market maker.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub node_url: String,
    pub symbol: String,
    pub seed: u64,
    pub intensity: Intensity,
    pub interval_ms: u64,
    pub target_balance: i64,
    pub reference_price: Option<i64>,
    pub max_orders_per_tick: usize,
    pub max_open_orders_per_account: usize,
    pub max_submissions_per_minute: usize,
    pub request_timeout_ms: u64,
    pub finality_timeout_ms: u64,
    pub receipt_poll_ms: u64,
    pub max_retries: u8,
    pub max_consecutive_failures: u32,
    pub ticks: Option<u64>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            node_url: "http://127.0.0.1:8080".to_string(),
            symbol: "BTC-USDT".to_string(),
            seed: 12345,
            intensity: Intensity::Low,
            interval_ms: 1_000,
            target_balance: 100_000_000,
            reference_price: None,
            max_orders_per_tick: 4,
            max_open_orders_per_account: 4,
            max_submissions_per_minute: 60,
            request_timeout_ms: 5_000,
            finality_timeout_ms: 15_000,
            receipt_poll_ms: 100,
            max_retries: 2,
            max_consecutive_failures: 5,
            ticks: None,
        }
    }
}

impl ServiceConfig {
    pub fn validate(&self) -> Result<()> {
        validate_loopback_url(&self.node_url)?;
        if self.symbol.trim().is_empty() {
            bail!("market-maker symbol must not be empty");
        }
        if self.interval_ms == 0 {
            bail!("market-maker interval must be greater than zero");
        }
        if self.target_balance <= 0 {
            bail!("market-maker target balance must be positive");
        }
        if self.max_orders_per_tick == 0 {
            bail!("max orders per tick must be positive");
        }
        if self.max_open_orders_per_account == 0 {
            bail!("max open orders per account must be positive");
        }
        if self.max_submissions_per_minute == 0 || self.max_submissions_per_minute > 60 {
            bail!("max submissions per minute must be between 1 and 60");
        }
        if self.request_timeout_ms == 0
            || self.finality_timeout_ms == 0
            || self.receipt_poll_ms == 0
        {
            bail!("request, finality, and receipt poll timeouts must be positive");
        }
        if self.max_retries > 5 {
            bail!("max retries must not exceed 5");
        }
        if self.max_consecutive_failures == 0 {
            bail!("max consecutive failures must be positive");
        }
        if self.reference_price.is_some_and(|price| price <= 0) {
            bail!("reference price must be positive when supplied");
        }
        Ok(())
    }

    pub(crate) fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }

    pub(crate) fn finality_timeout(&self) -> Duration {
        Duration::from_millis(self.finality_timeout_ms)
    }

    pub(crate) fn receipt_poll(&self) -> Duration {
        Duration::from_millis(self.receipt_poll_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_config_rejects_unbounded_or_invalid_limits() {
        let mut config = ServiceConfig::default();
        config.max_submissions_per_minute = 61;
        assert!(config.validate().is_err());
        config.max_submissions_per_minute = 60;
        config.max_retries = 6;
        assert!(config.validate().is_err());
    }
}
