//! Heap-based Orderbook with Price-Time Priority
//!
//! O(log N) insert, O(1) best price lookup.
//! FIFO matching within each price level.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use serde::{Deserialize, Serialize};

use crate::types::{Price, Size};
use super::{Address, MarketConfig, Symbol};

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
    pub size: Size,           // Remaining size
    pub original_size: Size,  // Original size
    pub order_type: OrderType,
    pub reduce_only: bool,    // Only reduce existing position
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
    pub side: Side,           // Taker's side
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

/// Aggregate price levels from order map
fn aggregate_levels(
    levels: &HashMap<Price, Vec<Order>>,
    limit: usize,
    descending: bool,
) -> Vec<PriceLevel> {
    let mut result: Vec<_> = levels
        .iter()
        .filter(|(_, orders)| !orders.is_empty())
        .map(|(price, orders)| PriceLevel {
            price: *price,
            size: orders.iter().map(|o| o.size).sum(),
            order_count: orders.len(),
        })
        .collect();
    if descending {
        result.sort_by(|a, b| b.price.cmp(&a.price));
    } else {
        result.sort_by(|a, b| a.price.cmp(&b.price));
    }
    result.truncate(limit);
    result
}

/// Heap-based orderbook
pub struct OrderBook {
    symbol: Symbol,

    // Heaps for O(1) best price
    bid_heap: BinaryHeap<Price>,           // Max-heap
    ask_heap: BinaryHeap<Reverse<Price>>,  // Min-heap

    // Price level queues (FIFO)
    bids: HashMap<Price, Vec<Order>>,
    asks: HashMap<Price, Vec<Order>>,

    // Order index for O(1) cancel
    order_index: HashMap<OrderId, (Side, Price)>,

    // Last traded price
    last_price: Price,

    // Sequence number for order IDs
    seq: u64,
}

impl OrderBook {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            bid_heap: BinaryHeap::new(),
            ask_heap: BinaryHeap::new(),
            bids: HashMap::new(),
            asks: HashMap::new(),
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

    /// Place an order, returning fills
    pub fn place(&mut self, mut order: Order, config: &MarketConfig) -> Result<Vec<Fill>, OrderBookError> {
        // Validate
        self.validate_order(&order, config)?;

        // ALO check: reject if would match immediately
        if order.order_type == OrderType::Alo {
            if self.would_match(&order) {
                return Err(OrderBookError::AloWouldMatch);
            }
        }

        let mut fills = Vec::new();
        let now = order.timestamp;

        match order.side {
            Side::Bid => {
                // Match against asks
                while order.size > 0 {
                    let best_ask = match self.ask_heap.peek() {
                        Some(Reverse(p)) => *p,
                        None => break,
                    };

                    if best_ask > order.price {
                        break; // Price doesn't cross
                    }

                    let level = match self.asks.get_mut(&best_ask) {
                        Some(orders) if !orders.is_empty() => orders,
                        _ => {
                            self.asks.remove(&best_ask);
                            self.ask_heap.pop();
                            continue;
                        }
                    };

                    let maker = &mut level[0];
                    let match_size = order.size.min(maker.size);

                    fills.push(Fill {
                        taker_order_id: order.id.clone(),
                        maker_order_id: maker.id.clone(),
                        taker: order.trader.clone(),
                        maker: maker.trader.clone(),
                        symbol: self.symbol.clone(),
                        side: Side::Bid,
                        price: best_ask,
                        size: match_size,
                        timestamp: now,
                    });

                    order.size -= match_size;
                    maker.size -= match_size;
                    self.last_price = best_ask;

                    if maker.size == 0 {
                        let maker_id = level.remove(0).id;
                        self.order_index.remove(&maker_id);
                        if level.is_empty() {
                            self.asks.remove(&best_ask);
                            self.remove_from_ask_heap(best_ask);
                        }
                    }
                }

                // Rest on book if GTC and remaining size
                if order.size > 0 && order.order_type == OrderType::Gtc {
                    self.add_bid(order);
                }
            }

            Side::Ask => {
                // Match against bids
                while order.size > 0 {
                    let best_bid = match self.bid_heap.peek() {
                        Some(p) => *p,
                        None => break,
                    };

                    if best_bid < order.price {
                        break; // Price doesn't cross
                    }

                    let level = match self.bids.get_mut(&best_bid) {
                        Some(orders) if !orders.is_empty() => orders,
                        _ => {
                            self.bids.remove(&best_bid);
                            self.bid_heap.pop();
                            continue;
                        }
                    };

                    let maker = &mut level[0];
                    let match_size = order.size.min(maker.size);

                    fills.push(Fill {
                        taker_order_id: order.id.clone(),
                        maker_order_id: maker.id.clone(),
                        taker: order.trader.clone(),
                        maker: maker.trader.clone(),
                        symbol: self.symbol.clone(),
                        side: Side::Ask,
                        price: best_bid,
                        size: match_size,
                        timestamp: now,
                    });

                    order.size -= match_size;
                    maker.size -= match_size;
                    self.last_price = best_bid;

                    if maker.size == 0 {
                        let maker_id = level.remove(0).id;
                        self.order_index.remove(&maker_id);
                        if level.is_empty() {
                            self.bids.remove(&best_bid);
                            self.remove_from_bid_heap(best_bid);
                        }
                    }
                }

                // Rest on book if GTC
                if order.size > 0 && order.order_type == OrderType::Gtc {
                    self.add_ask(order);
                }
            }
        }

        Ok(fills)
    }

    /// Cancel an order
    pub fn cancel(&mut self, order_id: &str) -> bool {
        let (side, price) = match self.order_index.remove(order_id) {
            Some(info) => info,
            None => return false,
        };

        let orders = match side {
            Side::Bid => self.bids.get_mut(&price),
            Side::Ask => self.asks.get_mut(&price),
        };

        if let Some(level) = orders {
            if let Some(pos) = level.iter().position(|o| o.id == order_id) {
                level.remove(pos);
                if level.is_empty() {
                    match side {
                        Side::Bid => {
                            self.bids.remove(&price);
                            self.remove_from_bid_heap(price);
                        }
                        Side::Ask => {
                            self.asks.remove(&price);
                            self.remove_from_ask_heap(price);
                        }
                    }
                }
                return true;
            }
        }

        false
    }

    /// Get best bid price
    pub fn best_bid(&self) -> Option<Price> {
        self.bid_heap.peek().copied()
    }

    /// Get best ask price
    pub fn best_ask(&self) -> Option<Price> {
        self.ask_heap.peek().map(|Reverse(p)| *p)
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
        aggregate_levels(&self.bids, limit, true)
    }

    /// Get ask levels (sorted low to high)
    pub fn ask_levels(&self, limit: usize) -> Vec<PriceLevel> {
        aggregate_levels(&self.asks, limit, false)
    }

    /// Get symbol
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Get all orders for a specific trader
    pub fn orders_by_trader(&self, trader: &str) -> Vec<&Order> {
        let trader_lower = trader.to_lowercase();
        self.bids.values()
            .chain(self.asks.values())
            .flat_map(|orders| orders.iter())
            .filter(|order| order.trader.to_lowercase() == trader_lower)
            .collect()
    }

    // --- Private helpers ---

    fn validate_order(&self, order: &Order, config: &MarketConfig) -> Result<(), OrderBookError> {
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
        Ok(())
    }

    fn would_match(&self, order: &Order) -> bool {
        match order.side {
            Side::Bid => self.best_ask().map(|a| a <= order.price).unwrap_or(false),
            Side::Ask => self.best_bid().map(|b| b >= order.price).unwrap_or(false),
        }
    }

    fn add_bid(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();
        if !self.bids.contains_key(&price) {
            self.bid_heap.push(price);
        }
        self.bids.entry(price).or_default().push(order);
        self.order_index.insert(id, (Side::Bid, price));
    }

    fn add_ask(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();
        if !self.asks.contains_key(&price) {
            self.ask_heap.push(Reverse(price));
        }
        self.asks.entry(price).or_default().push(order);
        self.order_index.insert(id, (Side::Ask, price));
    }

    fn remove_from_bid_heap(&mut self, price: Price) {
        let heap_vec: Vec<_> = self.bid_heap.drain().filter(|&p| p != price).collect();
        self.bid_heap = heap_vec.into_iter().collect();
    }

    fn remove_from_ask_heap(&mut self, price: Price) {
        let heap_vec: Vec<_> = self.ask_heap.drain().filter(|Reverse(p)| *p != price).collect();
        self.ask_heap = heap_vec.into_iter().collect();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_order(id: &str, side: Side, price: Price, size: Size) -> Order {
        Order {
            id: id.to_string(),
            trader: "trader".to_string(),
            symbol: "BTC-USDT".to_string(),
            side,
            price,
            size,
            original_size: size,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 0,
        }
    }

    #[test]
    fn test_basic_matching() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Place bid
        let bid = make_order("bid1", Side::Bid, 50000, 100);
        let fills = book.place(bid, &config).unwrap();
        assert!(fills.is_empty());

        // Place ask that crosses
        let ask = make_order("ask1", Side::Ask, 49000, 50);
        let fills = book.place(ask, &config).unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50);
        assert_eq!(fills[0].price, 50000); // Matches at bid price
    }

    #[test]
    fn test_price_time_priority() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Two bids at same price
        book.place(make_order("bid1", Side::Bid, 50000, 100), &config).unwrap();
        book.place(make_order("bid2", Side::Bid, 50000, 100), &config).unwrap();

        // Ask matches first bid (FIFO)
        let ask = make_order("ask1", Side::Ask, 50000, 100);
        let fills = book.place(ask, &config).unwrap();

        assert_eq!(fills[0].maker_order_id, "bid1");
    }

    #[test]
    fn test_cancel() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        book.place(make_order("bid1", Side::Bid, 50000, 100), &config).unwrap();
        assert!(book.best_bid().is_some());

        assert!(book.cancel("bid1"));
        assert!(book.best_bid().is_none());

        assert!(!book.cancel("nonexistent"));
    }

    #[test]
    fn test_ioc_no_rest() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        let mut ioc = make_order("ioc1", Side::Bid, 50000, 100);
        ioc.order_type = OrderType::Ioc;

        book.place(ioc, &config).unwrap();
        assert!(book.best_bid().is_none()); // IOC doesn't rest
    }
}
