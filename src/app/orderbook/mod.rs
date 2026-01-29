//! BTreeMap-based Orderbook with Price-Time Priority
//!
//! O(log N) insert, O(log N) cancel, O(log N) best price lookup.
//! FIFO matching within each price level using VecDeque.
//!
//! ## Why BTreeMap over Heap?
//!
//! - Heap cancel: O(n log n) - must rebuild entire heap
//! - BTreeMap cancel: O(log n) - direct removal with index lookup
//!
//! For high-frequency trading where cancels vastly outnumber fills,
//! BTreeMap is significantly more efficient.

mod matching;

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{Address, MarketConfig, Symbol};
use crate::types::{Price, Size};

/// Unique order identifier
pub type OrderId = String;

/// Order side
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Bid, // Buy
    Ask, // Sell
}

/// Order type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    /// Good-til-cancel: rests on book after partial fill
    Gtc,
    /// Immediate-or-cancel: fills what it can, cancels rest
    Ioc,
    /// Add-liquidity-only: rejected if would match immediately
    Alo,
}

/// An order in the book
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub trader: Address,
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,
    pub size: Size,          // Remaining size
    pub original_size: Size, // Original size
    pub order_type: OrderType,
    pub reduce_only: bool, // Only reduce existing position
    pub timestamp: u64,
}

/// A fill (trade execution)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub taker_order_id: OrderId,
    pub maker_order_id: OrderId,
    pub taker: Address,
    pub maker: Address,
    pub symbol: Symbol,
    pub side: Side, // Taker's side
    pub price: Price,
    pub size: Size,
    pub timestamp: u64,
}

/// Price level for display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: Price,
    pub size: Size,
    pub order_count: usize,
}

/// BTreeMap-based orderbook
///
/// Uses BTreeMap for O(log n) operations:
/// - Bids: `BTreeMap<Reverse<Price>, VecDeque<Order>>` (highest price first)
/// - Asks: `BTreeMap<Price, VecDeque<Order>>` (lowest price first)
/// - Index: `HashMap<OrderId, (Side, Price)>` for O(1) lookup
pub struct OrderBook {
    pub(crate) symbol: Symbol,

    /// Bids sorted by price descending (highest = best bid)
    /// Reverse<Price> makes BTreeMap iterate highest-first
    pub(crate) bids: BTreeMap<Reverse<Price>, VecDeque<Order>>,

    /// Asks sorted by price ascending (lowest = best ask)
    pub(crate) asks: BTreeMap<Price, VecDeque<Order>>,

    /// Order index for O(1) cancel lookup: OrderId -> (Side, Price)
    pub(crate) order_index: HashMap<OrderId, (Side, Price)>,

    /// Last traded price
    pub(crate) last_price: Price,

    /// Sequence number for order IDs
    pub(crate) seq: u64,
}

impl OrderBook {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            order_index: HashMap::new(),
            last_price: 0,
            seq: 0,
        }
    }

    /// Generate a new order ID
    pub fn next_order_id(&mut self) -> OrderId {
        self.seq += 1;
        format!("{}_{}", self.symbol, self.seq)
    }

    /// Cancel an order, returning the cancelled order if found
    ///
    /// Complexity: O(log n) for BTreeMap access + O(k) for VecDeque search
    /// where k is orders at that price level (typically small)
    pub fn cancel(&mut self, order_id: &str) -> Option<Order> {
        // O(1) lookup in index
        let (side, price) = self.order_index.remove(order_id)?;

        match side {
            Side::Bid => {
                let level = self.bids.get_mut(&Reverse(price))?;
                let pos = level.iter().position(|o| o.id == order_id)?;
                let cancelled = level.remove(pos)?;

                // Remove empty price level - O(log n)
                if level.is_empty() {
                    self.bids.remove(&Reverse(price));
                }

                Some(cancelled)
            }
            Side::Ask => {
                let level = self.asks.get_mut(&price)?;
                let pos = level.iter().position(|o| o.id == order_id)?;
                let cancelled = level.remove(pos)?;

                // Remove empty price level - O(log n)
                if level.is_empty() {
                    self.asks.remove(&price);
                }

                Some(cancelled)
            }
        }
    }

    /// Get best bid price - O(log n) amortized O(1) due to BTreeMap caching
    pub fn best_bid(&self) -> Option<Price> {
        self.bids.keys().next().map(|Reverse(p)| *p)
    }

    /// Get best ask price - O(log n) amortized O(1) due to BTreeMap caching
    pub fn best_ask(&self) -> Option<Price> {
        self.asks.keys().next().copied()
    }

    /// Get mid price
    pub fn mid_price(&self) -> Option<Price> {
        Some((self.best_bid()? + self.best_ask()?) / 2)
    }

    /// Get last traded price
    pub fn last_price(&self) -> Price {
        self.last_price
    }

    /// Get bid levels (sorted high to low)
    pub fn bid_levels(&self, limit: usize) -> Vec<PriceLevel> {
        self.bids
            .iter()
            .take(limit)
            .map(|(Reverse(price), orders)| PriceLevel {
                price: *price,
                size: orders.iter().map(|o| o.size).sum(),
                order_count: orders.len(),
            })
            .collect()
    }

    /// Get ask levels (sorted low to high)
    pub fn ask_levels(&self, limit: usize) -> Vec<PriceLevel> {
        self.asks
            .iter()
            .take(limit)
            .map(|(price, orders)| PriceLevel {
                price: *price,
                size: orders.iter().map(|o| o.size).sum(),
                order_count: orders.len(),
            })
            .collect()
    }

    /// Get symbol
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Get all orders for a specific trader
    pub fn orders_by_trader(&self, trader: &str) -> Vec<&Order> {
        let trader_lower = trader.to_lowercase();
        self.bids
            .values()
            .chain(self.asks.values())
            .flat_map(|orders| orders.iter())
            .filter(|order| order.trader.to_lowercase() == trader_lower)
            .collect()
    }

    /// Count open orders for a specific trader (for limit enforcement)
    pub fn count_orders_by_trader(&self, trader: &str) -> usize {
        let trader_lower = trader.to_lowercase();
        self.bids
            .values()
            .chain(self.asks.values())
            .flat_map(|orders| orders.iter())
            .filter(|order| order.trader.to_lowercase() == trader_lower)
            .count()
    }

    // --- Internal helpers ---

    pub(crate) fn validate_order(
        &self,
        order: &Order,
        config: &MarketConfig,
    ) -> Result<(), OrderBookError> {
        if order.price <= 0 {
            return Err(OrderBookError::InvalidPrice);
        }
        if order.price % config.tick_size != 0 {
            return Err(OrderBookError::PriceNotAligned);
        }
        if order.size <= 0 {
            return Err(OrderBookError::InvalidSize);
        }
        if order.size % config.lot_size != 0 {
            return Err(OrderBookError::SizeNotAligned);
        }
        // CRITICAL-3: Check max order size to prevent OOM
        if order.size > config.max_order_size {
            return Err(OrderBookError::OrderSizeTooLarge {
                max: config.max_order_size,
                got: order.size,
            });
        }
        Ok(())
    }

    pub(crate) fn would_match(&self, order: &Order) -> bool {
        match order.side {
            Side::Bid => self
                .best_ask()
                .map(|a| a <= order.price)
                .unwrap_or(false),
            Side::Ask => self
                .best_bid()
                .map(|b| b >= order.price)
                .unwrap_or(false),
        }
    }

    /// Add a bid order to the book - O(log n)
    pub(crate) fn add_bid(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();

        self.bids
            .entry(Reverse(price))
            .or_insert_with(VecDeque::new)
            .push_back(order);

        self.order_index.insert(id, (Side::Bid, price));
    }

    /// Add an ask order to the book - O(log n)
    pub(crate) fn add_ask(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();

        self.asks
            .entry(price)
            .or_insert_with(VecDeque::new)
            .push_back(order);

        self.order_index.insert(id, (Side::Ask, price));
    }
}

/// Orderbook errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum OrderBookError {
    #[error("invalid price")]
    InvalidPrice,
    #[error("price not aligned to tick size")]
    PriceNotAligned,
    #[error("invalid size")]
    InvalidSize,
    #[error("size not aligned to lot size")]
    SizeNotAligned,
    #[error("ALO order would match immediately")]
    AloWouldMatch,
    #[error("order size {got} exceeds max {max}")]
    OrderSizeTooLarge { max: i64, got: i64 },
    #[error("too many open orders (max: {max})")]
    TooManyOpenOrders { max: usize },
}
