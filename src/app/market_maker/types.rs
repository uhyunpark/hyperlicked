//! Market Maker Types
//!
//! Core types for the artificial market maker module.

use crate::app::{OrderType, Side, Transaction};
use crate::types::Price;

/// Types of market maker strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrategyType {
    /// Quotes tight spread around oracle (5-10 bps)
    TightQuoter,
    /// Quotes wide spread, patient liquidity (30-50 bps)
    WideQuoter,
    /// Follows recent price trends
    TrendFollower,
    /// Bets on price reverting to moving average
    MeanReverter,
    /// Random orders for volume/stress testing
    NoiseTrader,
    /// Aggressive taker - always crosses spread to generate fills
    AggressiveTaker,
}

impl StrategyType {
    /// Get all strategy types
    pub fn all() -> &'static [StrategyType] {
        &[
            StrategyType::TightQuoter,
            StrategyType::WideQuoter,
            StrategyType::TrendFollower,
            StrategyType::MeanReverter,
            StrategyType::NoiseTrader,
            StrategyType::AggressiveTaker,
        ]
    }

    /// Human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            StrategyType::TightQuoter => "TightQuoter",
            StrategyType::WideQuoter => "WideQuoter",
            StrategyType::TrendFollower => "TrendFollower",
            StrategyType::MeanReverter => "MeanReverter",
            StrategyType::NoiseTrader => "NoiseTrader",
            StrategyType::AggressiveTaker => "AggressiveTaker",
        }
    }
}

/// Market maker action returned by strategies
#[derive(Debug, Clone)]
pub enum MarketMakerAction {
    /// Do nothing this tick
    None,
    /// Place a new order
    PlaceOrder {
        side: Side,
        price: Price,
        size: i64,
        order_type: OrderType,
    },
    /// Cancel an existing order
    CancelOrder {
        order_id: String,
    },
    /// Place multiple orders
    PlaceMultiple {
        orders: Vec<(Side, Price, i64, OrderType)>,
    },
    /// Cancel all orders and place new ones
    ReplaceOrders {
        cancel_ids: Vec<String>,
        new_orders: Vec<(Side, Price, i64, OrderType)>,
    },
}

impl MarketMakerAction {
    /// Convert action to transactions for a given trader
    pub fn to_transactions(
        &self,
        trader: &str,
        symbol: &str,
    ) -> Vec<Transaction> {
        match self {
            MarketMakerAction::None => vec![],

            MarketMakerAction::PlaceOrder { side, price, size, order_type } => {
                vec![Transaction::PlaceOrder {
                    trader: trader.to_string(),
                    symbol: symbol.to_string(),
                    side: *side,
                    price: *price,
                    size: *size,
                    order_type: *order_type,
                    reduce_only: false,
                }]
            }

            MarketMakerAction::CancelOrder { order_id } => {
                vec![Transaction::CancelOrder {
                    trader: trader.to_string(),
                    order_id: order_id.clone(),
                }]
            }

            MarketMakerAction::PlaceMultiple { orders } => {
                orders.iter().map(|(side, price, size, order_type)| {
                    Transaction::PlaceOrder {
                        trader: trader.to_string(),
                        symbol: symbol.to_string(),
                        side: *side,
                        price: *price,
                        size: *size,
                        order_type: *order_type,
                        reduce_only: false,
                    }
                }).collect()
            }

            MarketMakerAction::ReplaceOrders { cancel_ids, new_orders } => {
                let mut txs: Vec<Transaction> = cancel_ids.iter().map(|id| {
                    Transaction::CancelOrder {
                        trader: trader.to_string(),
                        order_id: id.clone(),
                    }
                }).collect();

                txs.extend(new_orders.iter().map(|(side, price, size, order_type)| {
                    Transaction::PlaceOrder {
                        trader: trader.to_string(),
                        symbol: symbol.to_string(),
                        side: *side,
                        price: *price,
                        size: *size,
                        order_type: *order_type,
                        reduce_only: false,
                    }
                }));

                txs
            }
        }
    }
}

/// Market state snapshot for strategy decisions
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    /// Oracle price (cents)
    pub oracle_price: Price,
    /// Best bid price (if exists)
    pub best_bid: Option<Price>,
    /// Best ask price (if exists)
    pub best_ask: Option<Price>,
    /// Recent price history for MA calculation
    pub price_history: Vec<Price>,
    /// Current timestamp (ms)
    pub timestamp: u64,
}

impl MarketSnapshot {
    /// Calculate simple moving average of price history
    pub fn moving_average(&self, period: usize) -> Option<Price> {
        if self.price_history.is_empty() {
            return None;
        }
        let count = period.min(self.price_history.len());
        let sum: i64 = self.price_history.iter().rev().take(count).sum();
        Some(sum / count as i64)
    }

    /// Get price trend (positive = rising, negative = falling)
    pub fn price_trend(&self, lookback: usize) -> i64 {
        if self.price_history.len() < 2 {
            return 0;
        }
        let recent = self.price_history.iter().rev().take(lookback).copied().collect::<Vec<_>>();
        if recent.len() < 2 {
            return 0;
        }
        recent.first().unwrap_or(&0) - recent.last().unwrap_or(&0)
    }

    /// Calculate mid price
    pub fn mid_price(&self) -> Option<Price> {
        match (self.best_bid, self.best_ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / 2),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            _ => None,
        }
    }
}
