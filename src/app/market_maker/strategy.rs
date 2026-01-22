//! Market Maker Strategies
//!
//! Implements various trading strategies for the artificial market maker.
//! Each strategy has different behavior to create realistic market dynamics.

use rand::rngs::StdRng;
use rand::Rng;

use super::types::{MarketMakerAction, MarketSnapshot, StrategyType};
use crate::app::{OrderType, Side};
use crate::types::Price;

/// Strategy trait - defines behavior for market maker accounts
pub trait Strategy: Send + Sync {
    /// Decide next action based on market state
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction;

    /// Strategy name for logging
    fn name(&self) -> &'static str;

    /// Strategy type
    fn strategy_type(&self) -> StrategyType;
}

/// Create a strategy instance by type
pub fn create_strategy(strategy_type: StrategyType) -> Box<dyn Strategy> {
    match strategy_type {
        StrategyType::TightQuoter => Box::new(TightQuoter::new(5, 10)),
        StrategyType::WideQuoter => Box::new(WideQuoter::new(30, 50)),
        StrategyType::TrendFollower => Box::new(TrendFollower::new(5)),
        StrategyType::MeanReverter => Box::new(MeanReverter::new(10)),
        StrategyType::NoiseTrader => Box::new(NoiseTrader::new(100)),
        StrategyType::AggressiveTaker => Box::new(AggressiveTaker::new()),
    }
}

// =============================================================================
// Tight Quoter - quotes tight spread around oracle price
// =============================================================================

/// Tight quoter strategy - provides tight liquidity around oracle price
pub struct TightQuoter {
    /// Minimum spread from oracle (bps)
    spread_min_bps: i64,
    /// Maximum spread from oracle (bps)
    spread_max_bps: i64,
    /// Tick counter for repositioning
    tick_count: u32,
}

impl TightQuoter {
    pub fn new(spread_min_bps: i64, spread_max_bps: i64) -> Self {
        Self {
            spread_min_bps,
            spread_max_bps,
            tick_count: 0,
        }
    }

    fn calculate_quote_price(&self, oracle: Price, side: Side, rng: &mut StdRng) -> Price {
        let spread_bps = rng.gen_range(self.spread_min_bps..=self.spread_max_bps);
        let offset = (oracle * spread_bps) / 10000;

        match side {
            Side::Bid => oracle - offset,
            Side::Ask => oracle + offset,
        }
    }
}

impl Strategy for TightQuoter {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        self.tick_count += 1;

        // Reposition every 3-5 ticks
        if self.tick_count % rng.gen_range(3..=5) != 0 {
            return MarketMakerAction::None;
        }

        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        // Small order size: 0.01-0.1 BTC (in satoshis)
        let size = rng.gen_range(1_000_000..10_000_000);

        // Quote both sides
        let bid_price = self.calculate_quote_price(oracle, Side::Bid, rng);
        let ask_price = self.calculate_quote_price(oracle, Side::Ask, rng);

        MarketMakerAction::PlaceMultiple {
            orders: vec![
                (Side::Bid, bid_price, size, OrderType::Gtc),
                (Side::Ask, ask_price, size, OrderType::Gtc),
            ],
        }
    }

    fn name(&self) -> &'static str {
        "TightQuoter"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::TightQuoter
    }
}

// =============================================================================
// Wide Quoter - quotes wide spread, patient liquidity provider
// =============================================================================

/// Wide quoter strategy - provides deep liquidity at wider spreads
pub struct WideQuoter {
    /// Minimum spread from oracle (bps)
    spread_min_bps: i64,
    /// Maximum spread from oracle (bps)
    spread_max_bps: i64,
    /// Tick counter
    tick_count: u32,
}

impl WideQuoter {
    pub fn new(spread_min_bps: i64, spread_max_bps: i64) -> Self {
        Self {
            spread_min_bps,
            spread_max_bps,
            tick_count: 0,
        }
    }
}

impl Strategy for WideQuoter {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        self.tick_count += 1;

        // Reposition less frequently (every 10-15 ticks)
        if self.tick_count % rng.gen_range(10..=15) != 0 {
            return MarketMakerAction::None;
        }

        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        // Medium order size: 0.1-0.5 BTC (in satoshis)
        let size = rng.gen_range(10_000_000..50_000_000);

        let spread_bps = rng.gen_range(self.spread_min_bps..=self.spread_max_bps);
        let offset = (oracle * spread_bps) / 10000;

        let bid_price = oracle - offset;
        let ask_price = oracle + offset;

        MarketMakerAction::PlaceMultiple {
            orders: vec![
                (Side::Bid, bid_price, size, OrderType::Gtc),
                (Side::Ask, ask_price, size, OrderType::Gtc),
            ],
        }
    }

    fn name(&self) -> &'static str {
        "WideQuoter"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::WideQuoter
    }
}

// =============================================================================
// Trend Follower - follows recent price movement direction
// =============================================================================

/// Trend follower strategy - trades in direction of recent price movement
pub struct TrendFollower {
    /// Number of prices to look back for trend
    lookback: usize,
    /// Tick counter
    tick_count: u32,
}

impl TrendFollower {
    pub fn new(lookback: usize) -> Self {
        Self {
            lookback,
            tick_count: 0,
        }
    }
}

impl Strategy for TrendFollower {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        self.tick_count += 1;

        // Act every 2-4 ticks
        if self.tick_count % rng.gen_range(2..=4) != 0 {
            return MarketMakerAction::None;
        }

        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        let trend = snapshot.price_trend(self.lookback);

        // Need meaningful trend (at least 0.05% move)
        let min_trend = oracle / 2000;
        if trend.abs() < min_trend {
            return MarketMakerAction::None;
        }

        // Small to medium size
        let size = rng.gen_range(5_000_000..20_000_000);

        // Trade in direction of trend, sometimes crossing spread
        let (side, price) = if trend > 0 {
            // Uptrend - buy
            let aggressive = rng.gen_bool(0.3);
            if aggressive {
                // Cross spread (IOC)
                let ask = snapshot.best_ask.unwrap_or(oracle + oracle / 1000);
                (Side::Bid, ask)
            } else {
                // Passive bid below oracle
                let offset = rng.gen_range(5..15);
                (Side::Bid, oracle - (oracle * offset) / 10000)
            }
        } else {
            // Downtrend - sell
            let aggressive = rng.gen_bool(0.3);
            if aggressive {
                // Cross spread (IOC)
                let bid = snapshot.best_bid.unwrap_or(oracle - oracle / 1000);
                (Side::Ask, bid)
            } else {
                // Passive ask above oracle
                let offset = rng.gen_range(5..15);
                (Side::Ask, oracle + (oracle * offset) / 10000)
            }
        };

        let order_type = if side == Side::Bid && price >= snapshot.best_ask.unwrap_or(i64::MAX) {
            OrderType::Ioc
        } else if side == Side::Ask && price <= snapshot.best_bid.unwrap_or(0) {
            OrderType::Ioc
        } else {
            OrderType::Gtc
        };

        MarketMakerAction::PlaceOrder {
            side,
            price,
            size,
            order_type,
        }
    }

    fn name(&self) -> &'static str {
        "TrendFollower"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::TrendFollower
    }
}

// =============================================================================
// Mean Reverter - bets on price returning to moving average
// =============================================================================

/// Mean reverter strategy - trades against deviations from MA
pub struct MeanReverter {
    /// MA period for price history
    ma_period: usize,
    /// Tick counter
    tick_count: u32,
}

impl MeanReverter {
    pub fn new(ma_period: usize) -> Self {
        Self {
            ma_period,
            tick_count: 0,
        }
    }
}

impl Strategy for MeanReverter {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        self.tick_count += 1;

        // Act every 5-8 ticks
        if self.tick_count % rng.gen_range(5..=8) != 0 {
            return MarketMakerAction::None;
        }

        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        let ma = match snapshot.moving_average(self.ma_period) {
            Some(ma) => ma,
            None => return MarketMakerAction::None,
        };

        // Calculate deviation from MA in bps
        let deviation_bps = ((oracle - ma) * 10000) / ma;

        // Need at least 20 bps deviation to act
        if deviation_bps.abs() < 20 {
            return MarketMakerAction::None;
        }

        // Larger size when deviation is higher
        let base_size = 10_000_000i64;
        let size_multiplier = (deviation_bps.abs() / 10).min(5);
        let size = base_size + base_size * size_multiplier / 2;

        // Trade against the deviation
        let (side, price) = if deviation_bps > 0 {
            // Price above MA - sell
            let offset = rng.gen_range(5..20);
            (Side::Ask, oracle + (oracle * offset) / 10000)
        } else {
            // Price below MA - buy
            let offset = rng.gen_range(5..20);
            (Side::Bid, oracle - (oracle * offset) / 10000)
        };

        MarketMakerAction::PlaceOrder {
            side,
            price,
            size,
            order_type: OrderType::Gtc,
        }
    }

    fn name(&self) -> &'static str {
        "MeanReverter"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::MeanReverter
    }
}

// =============================================================================
// Noise Trader - random orders for volume/stress testing
// =============================================================================

/// Noise trader strategy - random orders for volume generation
pub struct NoiseTrader {
    /// Max spread from oracle (bps)
    range_bps: i64,
}

impl NoiseTrader {
    pub fn new(range_bps: i64) -> Self {
        Self { range_bps }
    }
}

impl Strategy for NoiseTrader {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        // Always place orders (high frequency for stress test)
        let side = if rng.gen_bool(0.5) { Side::Bid } else { Side::Ask };

        // 50% chance to cross spread for actual fills
        let cross_spread = rng.gen_bool(0.5);

        let (price, order_type) = if cross_spread {
            // Cross the spread - bid at ask, ask at bid
            let aggressive_price = match side {
                Side::Bid => snapshot.best_ask.unwrap_or(oracle + oracle / 1000),
                Side::Ask => snapshot.best_bid.unwrap_or(oracle - oracle / 1000),
            };
            (aggressive_price, OrderType::Ioc)
        } else {
            // Passive: random price within range
            let offset_bps = rng.gen_range(1..=self.range_bps);
            let offset = (oracle * offset_bps) / 10000;
            let passive_price = match side {
                Side::Bid => oracle - offset,
                Side::Ask => oracle + offset,
            };
            let typ = if rng.gen_bool(0.7) { OrderType::Gtc } else { OrderType::Ioc };
            (passive_price, typ)
        };

        // Random size: 0.005-0.2 BTC
        let size = rng.gen_range(500_000..20_000_000);

        MarketMakerAction::PlaceOrder {
            side,
            price,
            size,
            order_type,
        }
    }

    fn name(&self) -> &'static str {
        "NoiseTrader"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::NoiseTrader
    }
}

// =============================================================================
// Aggressive Taker - always crosses spread to generate fills
// =============================================================================

/// Aggressive taker strategy - always crosses the spread to generate trades
pub struct AggressiveTaker {
    /// Tick counter for timing
    tick_count: u32,
}

impl AggressiveTaker {
    pub fn new() -> Self {
        Self { tick_count: 0 }
    }
}

impl Default for AggressiveTaker {
    fn default() -> Self {
        Self::new()
    }
}

impl Strategy for AggressiveTaker {
    fn decide(&mut self, snapshot: &MarketSnapshot, rng: &mut StdRng) -> MarketMakerAction {
        self.tick_count += 1;

        // Act every 2-3 ticks to create steady trade flow
        if self.tick_count % rng.gen_range(2..=3) != 0 {
            return MarketMakerAction::None;
        }

        let oracle = snapshot.oracle_price;
        if oracle == 0 {
            return MarketMakerAction::None;
        }

        // Randomly buy or sell
        let side = if rng.gen_bool(0.5) { Side::Bid } else { Side::Ask };

        // Cross the spread - bid at ask price, ask at bid price
        let price = match side {
            Side::Bid => snapshot.best_ask.unwrap_or(oracle + oracle / 1000),
            Side::Ask => snapshot.best_bid.unwrap_or(oracle - oracle / 1000),
        };

        // Small size to keep it reasonable: 0.01-0.05 BTC
        let size = rng.gen_range(1_000_000..5_000_000);

        MarketMakerAction::PlaceOrder {
            side,
            price,
            size,
            order_type: OrderType::Ioc, // Immediate-or-cancel
        }
    }

    fn name(&self) -> &'static str {
        "AggressiveTaker"
    }

    fn strategy_type(&self) -> StrategyType {
        StrategyType::AggressiveTaker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn test_snapshot() -> MarketSnapshot {
        MarketSnapshot {
            oracle_price: 10_000_000, // $100,000 in cents
            best_bid: Some(9_999_000),
            best_ask: Some(10_001_000),
            price_history: vec![
                9_990_000, 9_995_000, 10_000_000, 10_005_000, 10_010_000,
            ],
            timestamp: 1_000_000,
        }
    }

    #[test]
    fn test_tight_quoter_places_both_sides() {
        let mut strategy = TightQuoter::new(5, 10);
        let mut rng = StdRng::seed_from_u64(12345);
        let snapshot = test_snapshot();

        // Run until we get an action
        let mut action = MarketMakerAction::None;
        for _ in 0..10 {
            action = strategy.decide(&snapshot, &mut rng);
            if !matches!(action, MarketMakerAction::None) {
                break;
            }
        }

        if let MarketMakerAction::PlaceMultiple { orders } = action {
            assert_eq!(orders.len(), 2);
            assert!(orders.iter().any(|(s, _, _, _)| *s == Side::Bid));
            assert!(orders.iter().any(|(s, _, _, _)| *s == Side::Ask));
        }
    }

    #[test]
    fn test_noise_trader_always_acts() {
        let mut strategy = NoiseTrader::new(100);
        let mut rng = StdRng::seed_from_u64(12345);
        let snapshot = test_snapshot();

        // Should always produce an order
        let action = strategy.decide(&snapshot, &mut rng);
        assert!(matches!(action, MarketMakerAction::PlaceOrder { .. }));
    }

    #[test]
    fn test_create_strategy() {
        let strategy = create_strategy(StrategyType::TightQuoter);
        assert_eq!(strategy.name(), "TightQuoter");
        assert_eq!(strategy.strategy_type(), StrategyType::TightQuoter);
    }
}
