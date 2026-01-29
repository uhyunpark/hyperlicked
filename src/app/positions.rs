//! Position Management
//!
//! Represents a trader's position in a single market with PnL and
//! funding calculations using integer math for determinism.

use serde::{Deserialize, Serialize};

use crate::types::{Price, Size};

/// A position in a single market
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    /// Signed size: positive = long, negative = short
    pub size: Size,
    /// Average entry price (in cents)
    pub entry_price: Price,
    /// Realized PnL from closed portions (in cents)
    pub realized_pnl: i64,
    /// Cumulative funding paid (negative) or received (positive)
    #[serde(default)]
    pub cumulative_funding: i64,
    /// Timestamp when funding was last applied to this position
    #[serde(default)]
    pub last_funding_timestamp: u64,
}

impl Position {
    /// Calculate unrealized PnL at a given mark price
    ///
    /// Uses i128 intermediate to prevent overflow with large positions.
    pub fn unrealized_pnl(&self, mark_price: Price) -> i64 {
        if self.size == 0 {
            return 0;
        }
        // PnL = size * (mark - entry) / scale_factor
        // Use i128 to prevent overflow: 100M satoshis × 10B cents = 10^18
        let price_diff = mark_price - self.entry_price;
        let pnl_i128 = (self.size as i128 * price_diff as i128) / 100_000_000;
        pnl_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// Calculate notional value at mark price
    ///
    /// Uses i128 intermediate to prevent overflow.
    pub fn notional(&self, mark_price: Price) -> i64 {
        let notional_i128 = (self.size.abs() as i128 * mark_price as i128) / 100_000_000;
        notional_i128.clamp(0, i64::MAX as i128) as i64
    }

    /// Apply funding payment to this position
    /// funding_rate_bps: positive = longs pay shorts
    /// index_price: price used to calculate payment (usually mark/index price)
    /// Returns the payment amount (positive = received, negative = paid)
    ///
    /// Uses i128 intermediate to prevent overflow with large notional × funding rate.
    pub fn apply_funding(
        &mut self,
        funding_rate_bps: i64,
        index_price: Price,
        timestamp: u64,
    ) -> i64 {
        if self.size == 0 {
            return 0;
        }

        // Payment = |size| * index_price * funding_rate / 1e8 / 10000
        // Use i128 for entire calculation to prevent overflow
        let notional_i128 = (self.size.abs() as i128 * index_price as i128) / 100_000_000;
        let payment_i128 = (notional_i128 * funding_rate_bps as i128) / 10000;
        let payment = payment_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

        // Positive rate: longs pay, shorts receive
        // Negative rate: shorts pay, longs receive
        let signed_payment = if self.size > 0 {
            -payment // Long pays (or receives if rate negative)
        } else {
            payment // Short receives (or pays if rate negative)
        };

        self.cumulative_funding = self.cumulative_funding.saturating_add(signed_payment);
        self.last_funding_timestamp = timestamp;

        signed_payment
    }

    /// Calculate liquidation price given available margin
    /// Returns 0 if position is empty or margin is insufficient to calculate
    ///
    /// Uses i128 intermediate to prevent overflow in margin ratio calculations.
    pub fn liquidation_price(&self, available_margin: i64, maintenance_rate_bps: i64) -> Price {
        if self.size == 0 || self.entry_price == 0 {
            return 0;
        }

        // Notional at entry = |size| * entry_price / 1e8
        // Use i128 for the multiplication
        let notional_i128 = (self.size.abs() as i128 * self.entry_price as i128) / 100_000_000;
        if notional_i128 == 0 {
            return 0;
        }
        let notional_at_entry = notional_i128.min(i64::MAX as i128) as i64;

        // margin_ratio = available_margin * 10000 / notional_at_entry
        // Use i128 for the multiplication
        let margin_ratio_i128 = if notional_at_entry > 0 {
            (available_margin as i128 * 10000) / notional_at_entry as i128
        } else {
            0
        };
        let margin_ratio_bps = margin_ratio_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64;

        if self.size > 0 {
            // Long position: liquidated when price drops
            // liq_price = entry * (10000 - margin_ratio_bps + maintenance_rate_bps) / 10000
            let factor = 10000i64
                .saturating_sub(margin_ratio_bps)
                .saturating_add(maintenance_rate_bps);
            if factor <= 0 {
                return 0; // Well-margined, won't liquidate
            }
            // Use i128 for final multiplication
            let liq_i128 = (self.entry_price as i128 * factor as i128) / 10000;
            liq_i128.clamp(0, i64::MAX as i128) as i64
        } else {
            // Short position: liquidated when price rises
            // liq_price = entry * (10000 + margin_ratio_bps - maintenance_rate_bps) / 10000
            let factor = 10000i64
                .saturating_add(margin_ratio_bps)
                .saturating_sub(maintenance_rate_bps);
            // Use i128 for final multiplication
            let liq_i128 = (self.entry_price as i128 * factor as i128) / 10000;
            liq_i128.clamp(0, i64::MAX as i128) as i64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_pnl() {
        let mut pos = Position::default();

        // Long 1 BTC at $50,000
        pos.size = 100_000_000; // 1 BTC in satoshis
        pos.entry_price = 5_000_000; // $50,000 in cents

        // Mark at $51,000 -> $1,000 profit
        let pnl = pos.unrealized_pnl(5_100_000);
        assert_eq!(pnl, 100_000); // $1,000 in cents
    }

    #[test]
    fn test_liquidation_price_long() {
        let mut pos = Position::default();
        // Long 1 BTC at $50,000
        pos.size = 100_000_000; // 1 BTC
        pos.entry_price = 5_000_000; // $50,000

        // With $5,000 margin (10% of notional) and 5% maintenance (500 bps)
        // margin_ratio = 10%, so factor = 10000 - 1000 + 500 = 9500
        // liq_price = 50000 * 9500 / 10000 = $47,500
        let liq = pos.liquidation_price(500_000, 500);
        assert_eq!(liq, 4_750_000); // $47,500
    }

    #[test]
    fn test_liquidation_price_short() {
        let mut pos = Position::default();
        // Short 1 BTC at $50,000
        pos.size = -100_000_000; // -1 BTC
        pos.entry_price = 5_000_000; // $50,000

        // With $5,000 margin (10% of notional) and 5% maintenance (500 bps)
        // margin_ratio = 10%, so factor = 10000 + 1000 - 500 = 10500
        // liq_price = 50000 * 10500 / 10000 = $52,500
        let liq = pos.liquidation_price(500_000, 500);
        assert_eq!(liq, 5_250_000); // $52,500
    }

    #[test]
    fn test_apply_funding() {
        let mut pos = Position::default();
        pos.size = 100_000_000; // 1 BTC
        pos.entry_price = 5_000_000; // $50,000

        // Apply 1% funding rate (100 bps) - long pays
        let payment = pos.apply_funding(100, 5_000_000, 1000);

        // Payment = 1 BTC * $50k * 1% = $500, long pays
        assert_eq!(payment, -500_00); // -$500 (paid)
        assert_eq!(pos.cumulative_funding, -500_00);
        assert_eq!(pos.last_funding_timestamp, 1000);
    }
}
