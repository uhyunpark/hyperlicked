//! Market Maker Configuration
//!
//! Environment variable configuration and intensity presets.

use super::types::StrategyType;

/// Market maker intensity preset
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intensity {
    /// Showcase mode: 10-20 tx/tick
    Low,
    /// Default: 30-50 tx/tick
    Medium,
    /// Stress test: 100+ tx/tick
    High,
}

impl Intensity {
    /// Parse from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "low" | "showcase" => Intensity::Low,
            "high" | "stress" => Intensity::High,
            _ => Intensity::Medium, // Default
        }
    }

    /// Accounts per strategy type
    pub fn accounts_per_strategy(&self) -> usize {
        match self {
            Intensity::Low => 2,
            Intensity::Medium => 2,
            Intensity::High => 5,
        }
    }

    /// Description for logging
    pub fn description(&self) -> &'static str {
        match self {
            Intensity::Low => "low (showcase, 10-20 tx/tick)",
            Intensity::Medium => "medium (default, 30-50 tx/tick)",
            Intensity::High => "high (stress test, 100+ tx/tick)",
        }
    }
}

/// Market maker configuration
#[derive(Debug, Clone)]
pub struct MarketMakerConfig {
    /// Enable market maker
    pub enabled: bool,
    /// Tick interval in milliseconds
    pub interval_ms: u64,
    /// Intensity preset
    pub intensity: Intensity,
    /// RNG seed for deterministic addresses
    pub seed: u64,
    /// Target symbol
    pub symbol: String,
    /// Initial deposit per account (cents)
    pub initial_deposit: i64,
}

impl MarketMakerConfig {
    /// Load configuration from environment variables
    pub fn from_env() -> Self {
        Self {
            enabled: std::env::var("MM_ENABLED")
                .map(|s| s == "true" || s == "1")
                .unwrap_or(false),
            interval_ms: std::env::var("MM_INTERVAL_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100),
            intensity: std::env::var("MM_INTENSITY")
                .map(|s| Intensity::from_str(&s))
                .unwrap_or(Intensity::Medium),
            seed: std::env::var("MM_SEED")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(12345),
            symbol: "BTC-USDT".to_string(),
            initial_deposit: 100_000_000, // $1M per account
        }
    }

    /// Get total number of accounts
    pub fn total_accounts(&self) -> usize {
        StrategyType::all().len() * self.intensity.accounts_per_strategy()
    }

    /// Generate strategy distribution for logging
    pub fn strategy_summary(&self) -> Vec<(StrategyType, usize)> {
        StrategyType::all()
            .iter()
            .map(|st| (*st, self.intensity.accounts_per_strategy()))
            .collect()
    }
}

impl Default for MarketMakerConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intensity_parsing() {
        assert_eq!(Intensity::from_str("low"), Intensity::Low);
        assert_eq!(Intensity::from_str("LOW"), Intensity::Low);
        assert_eq!(Intensity::from_str("showcase"), Intensity::Low);
        assert_eq!(Intensity::from_str("medium"), Intensity::Medium);
        assert_eq!(Intensity::from_str("high"), Intensity::High);
        assert_eq!(Intensity::from_str("stress"), Intensity::High);
        assert_eq!(Intensity::from_str("unknown"), Intensity::Medium);
    }

    #[test]
    fn test_accounts_per_intensity() {
        assert_eq!(Intensity::Low.accounts_per_strategy(), 2);
        assert_eq!(Intensity::Medium.accounts_per_strategy(), 2);
        assert_eq!(Intensity::High.accounts_per_strategy(), 5);
    }

    #[test]
    fn test_total_accounts() {
        let mut config = MarketMakerConfig::default();

        config.intensity = Intensity::Low;
        assert_eq!(config.total_accounts(), 12); // 6 strategies * 2 accounts

        config.intensity = Intensity::High;
        assert_eq!(config.total_accounts(), 30); // 6 strategies * 5 accounts
    }
}
