//! Oracle Price Aggregation
//!
//! Weighted median and confidence calculation for oracle prices.

use crate::types::Price;

use super::types::PriceSource;

/// Calculate weighted median price from multiple sources.
///
/// Uses weighted median (not average) to be resistant to outliers.
/// Finds the price where cumulative weight reaches 50%.
pub fn weighted_median(sources: &[PriceSource]) -> Price {
    if sources.is_empty() {
        return 0;
    }
    if sources.len() == 1 {
        return sources[0].price;
    }

    // Sort by price
    let mut sorted: Vec<_> = sources.iter().collect();
    sorted.sort_by_key(|s| s.price);

    // Calculate total weight
    let total_weight: i64 = sorted.iter().map(|s| s.weight_bps).sum();
    if total_weight == 0 {
        // Fallback to simple median if no weights
        return sorted[sorted.len() / 2].price;
    }

    // Find price where cumulative weight >= 50%
    let half_weight = total_weight / 2;
    let mut cumulative = 0i64;

    for source in &sorted {
        cumulative += source.weight_bps;
        if cumulative >= half_weight {
            return source.price;
        }
    }

    // Shouldn't reach here, but return last price as fallback
    sorted.last().map(|s| s.price).unwrap_or(0)
}

/// Calculate confidence based on price spread among sources.
///
/// Returns confidence in basis points (0-10000).
/// Higher confidence = less spread among sources.
///
/// Formula: 10000 - (max_deviation_from_median_bps)
/// Clamped to [0, 10000]
pub fn calculate_confidence(sources: &[PriceSource], median_price: Price) -> i64 {
    if sources.is_empty() || median_price == 0 {
        return 0;
    }
    if sources.len() == 1 {
        return 10000; // Single source = full confidence
    }

    // Find max deviation from median in basis points
    // SECURITY: Use i128 to prevent overflow with large price differences
    let max_deviation_bps = sources
        .iter()
        .map(|s| {
            let diff = (s.price - median_price).abs();
            // deviation_bps = (diff / median_price) * 10000
            ((diff as i128 * 10000) / median_price as i128).clamp(0, i64::MAX as i128) as i64
        })
        .max()
        .unwrap_or(0);

    // Confidence = 10000 - max_deviation, clamped to [0, 10000]
    (10000 - max_deviation_bps).clamp(0, 10000)
}

/// Filter out stale price sources.
///
/// Returns only sources whose timestamp is within max_age_ms of current_time.
pub fn filter_stale(
    sources: &[PriceSource],
    current_time: u64,
    max_age_ms: u64,
) -> Vec<PriceSource> {
    sources
        .iter()
        .filter(|s| current_time.saturating_sub(s.timestamp) <= max_age_ms)
        .cloned()
        .collect()
}

/// Check if oracle price deviates too much from reference price.
///
/// Returns true if deviation exceeds max_bps (circuit breaker should trigger).
pub fn check_deviation(oracle_price: Price, reference_price: Price, max_bps: i64) -> bool {
    if oracle_price == 0 || reference_price == 0 {
        return false; // Can't check deviation with zero prices
    }

    let diff = (oracle_price - reference_price).abs();
    // SECURITY: Use i128 to prevent overflow with large price differences
    // deviation_bps = (diff / reference_price) * 10000
    let deviation_bps =
        ((diff as i128 * 10000) / reference_price as i128).clamp(0, i64::MAX as i128) as i64;

    deviation_bps > max_bps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(id: &str, price: Price, weight: i64) -> PriceSource {
        PriceSource {
            source_id: id.to_string(),
            price,
            timestamp: 1700000000000,
            weight_bps: weight,
        }
    }

    #[test]
    fn test_weighted_median_single_source() {
        let sources = vec![make_source("binance", 5_000_000, 2500)];
        assert_eq!(weighted_median(&sources), 5_000_000);
    }

    #[test]
    fn test_weighted_median_equal_weights() {
        let sources = vec![
            make_source("binance", 5_000_000, 2500),
            make_source("coinbase", 5_010_000, 2500),
            make_source("okx", 5_020_000, 2500),
            make_source("kraken", 5_030_000, 2500),
        ];
        // With equal weights, median is at 50% cumulative = 5_010_000
        let median = weighted_median(&sources);
        assert!(median == 5_010_000 || median == 5_000_000);
    }

    #[test]
    fn test_weighted_median_unequal_weights() {
        let sources = vec![
            make_source("binance", 5_000_000, 5000),  // 50% weight
            make_source("coinbase", 5_100_000, 2500), // 25% weight
            make_source("okx", 5_200_000, 2500),      // 25% weight
        ];
        // Binance has 50% weight, so median should be at binance price
        assert_eq!(weighted_median(&sources), 5_000_000);
    }

    #[test]
    fn test_weighted_median_empty() {
        let sources: Vec<PriceSource> = vec![];
        assert_eq!(weighted_median(&sources), 0);
    }

    #[test]
    fn test_calculate_confidence_tight_spread() {
        let sources = vec![
            make_source("binance", 5_000_000, 2500),
            make_source("coinbase", 5_001_000, 2500), // 0.02% off
            make_source("okx", 4_999_000, 2500),      // 0.02% off
        ];
        let median = weighted_median(&sources);
        let confidence = calculate_confidence(&sources, median);
        // Max deviation is ~0.02%, so confidence should be ~9998
        assert!(confidence > 9900);
    }

    #[test]
    fn test_calculate_confidence_wide_spread() {
        let sources = vec![
            make_source("binance", 5_000_000, 2500),
            make_source("coinbase", 5_500_000, 2500), // 10% off
            make_source("okx", 4_500_000, 2500),      // 10% off
        ];
        let median = weighted_median(&sources);
        let confidence = calculate_confidence(&sources, median);
        // Max deviation is 10% (1000 bps), so confidence should be ~9000
        assert!(confidence < 9100 && confidence > 8900);
    }

    #[test]
    fn test_filter_stale() {
        let sources = vec![
            PriceSource {
                source_id: "fresh".to_string(),
                price: 5_000_000,
                timestamp: 1700000000000,
                weight_bps: 2500,
            },
            PriceSource {
                source_id: "stale".to_string(),
                price: 4_900_000,
                timestamp: 1700000000000 - 5000, // 5 seconds old
                weight_bps: 2500,
            },
        ];

        let filtered = filter_stale(&sources, 1700000000000, 3000);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].source_id, "fresh");
    }

    #[test]
    fn test_check_deviation() {
        // 5% deviation (500 bps)
        assert!(!check_deviation(5_250_000, 5_000_000, 1000)); // Under 10% threshold
        assert!(check_deviation(5_600_000, 5_000_000, 1000)); // Over 10% threshold

        // Edge case: zero prices
        assert!(!check_deviation(0, 5_000_000, 1000));
        assert!(!check_deviation(5_000_000, 0, 1000));
    }
}
