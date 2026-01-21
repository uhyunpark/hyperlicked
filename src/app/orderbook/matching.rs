//! Order Matching Logic
//!
//! Implements price-time priority matching with self-trade prevention.

use std::cmp::Reverse;

use super::{Fill, MarketConfig, Order, OrderBook, OrderBookError, OrderType, Side};

impl OrderBook {
    /// Place an order, returning fills
    pub fn place(
        &mut self,
        mut order: Order,
        config: &MarketConfig,
    ) -> Result<Vec<Fill>, OrderBookError> {
        // Validate
        self.validate_order(&order, config)?;

        // ALO check: reject if would match immediately
        if order.order_type == OrderType::Alo && self.would_match(&order) {
            return Err(OrderBookError::AloWouldMatch);
        }

        let mut fills = Vec::new();
        let now = order.timestamp;

        match order.side {
            Side::Bid => self.match_bid(&mut order, &mut fills, now),
            Side::Ask => self.match_ask(&mut order, &mut fills, now),
        }

        Ok(fills)
    }

    fn match_bid(&mut self, order: &mut Order, fills: &mut Vec<Fill>, now: u64) {
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

            // Find first non-self-trade order at this price level
            let maker_idx = level
                .iter()
                .position(|m| m.trader.to_lowercase() != order.trader.to_lowercase());
            let maker_idx = match maker_idx {
                Some(idx) => idx,
                None => {
                    // All orders at this price are from the same trader - skip level
                    break;
                }
            };

            let maker = &mut level[maker_idx];
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
                let maker_id = level.remove(maker_idx).id;
                self.order_index.remove(&maker_id);
                if level.is_empty() {
                    self.asks.remove(&best_ask);
                    self.remove_from_ask_heap(best_ask);
                }
            }
        }

        // Rest on book if GTC/ALO and remaining size
        if order.size > 0
            && (order.order_type == OrderType::Gtc || order.order_type == OrderType::Alo)
        {
            self.add_bid(order.clone());
        }
    }

    fn match_ask(&mut self, order: &mut Order, fills: &mut Vec<Fill>, now: u64) {
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

            // Find first non-self-trade order at this price level
            let maker_idx = level
                .iter()
                .position(|m| m.trader.to_lowercase() != order.trader.to_lowercase());
            let maker_idx = match maker_idx {
                Some(idx) => idx,
                None => {
                    // All orders at this price are from the same trader - skip level
                    break;
                }
            };

            let maker = &mut level[maker_idx];
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
                let maker_id = level.remove(maker_idx).id;
                self.order_index.remove(&maker_id);
                if level.is_empty() {
                    self.bids.remove(&best_bid);
                    self.remove_from_bid_heap(best_bid);
                }
            }
        }

        // Rest on book if GTC/ALO
        if order.size > 0
            && (order.order_type == OrderType::Gtc || order.order_type == OrderType::Alo)
        {
            self.add_ask(order.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Price;

    fn make_order(id: &str, side: Side, price: Price, size: i64) -> Order {
        // Use different traders for bid/ask to test matching (non-self-trade)
        let trader = if side == Side::Bid { "alice" } else { "bob" };
        Order {
            id: id.to_string(),
            trader: trader.to_string(),
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

    fn make_order_with_trader(
        id: &str,
        trader: &str,
        side: Side,
        price: Price,
        size: i64,
    ) -> Order {
        Order {
            id: id.to_string(),
            trader: trader.to_string(),
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
        book.place(make_order("bid1", Side::Bid, 50000, 100), &config)
            .unwrap();
        book.place(make_order("bid2", Side::Bid, 50000, 100), &config)
            .unwrap();

        // Ask matches first bid (FIFO)
        let ask = make_order("ask1", Side::Ask, 50000, 100);
        let fills = book.place(ask, &config).unwrap();

        assert_eq!(fills[0].maker_order_id, "bid1");
    }

    #[test]
    fn test_cancel() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        book.place(make_order("bid1", Side::Bid, 50000, 100), &config)
            .unwrap();
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

    #[test]
    fn test_self_trade_prevention() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Place bid from alice
        let bid = make_order_with_trader("bid1", "alice", Side::Bid, 50000, 100);
        let fills = book.place(bid, &config).unwrap();
        assert!(fills.is_empty());
        assert!(book.best_bid().is_some());

        // Place ask from same trader (alice) - should NOT match
        let ask = make_order_with_trader("ask1", "alice", Side::Ask, 50000, 100);
        let fills = book.place(ask, &config).unwrap();
        assert!(fills.is_empty()); // No fills due to self-trade prevention
        assert!(book.best_bid().is_some()); // Bid still on book
        assert!(book.best_ask().is_some()); // Ask rests on book

        // Now place ask from different trader (bob) - should match
        let ask2 = make_order_with_trader("ask2", "bob", Side::Ask, 50000, 50);
        let fills = book.place(ask2, &config).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50);
        assert_eq!(fills[0].maker, "alice");
        assert_eq!(fills[0].taker, "bob");
    }

    #[test]
    fn test_self_trade_case_insensitive() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Place bid with lowercase address
        let bid = make_order_with_trader("bid1", "0xabcd", Side::Bid, 50000, 100);
        book.place(bid, &config).unwrap();

        // Place ask with mixed-case address (same user)
        let ask = make_order_with_trader("ask1", "0xAbCd", Side::Ask, 50000, 100);
        let fills = book.place(ask, &config).unwrap();

        // Should NOT match (case-insensitive comparison)
        assert!(fills.is_empty());
    }

    #[test]
    fn test_alo_rejection() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Place bid
        let bid = make_order("bid1", Side::Bid, 50000, 100);
        book.place(bid, &config).unwrap();

        // ALO ask that would match - should be rejected
        let mut alo = make_order("alo1", Side::Ask, 50000, 50);
        alo.order_type = OrderType::Alo;

        let result = book.place(alo, &config);
        assert!(matches!(result, Err(OrderBookError::AloWouldMatch)));
    }

    #[test]
    fn test_partial_fill() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = MarketConfig::default();

        // Place small bid
        let bid = make_order("bid1", Side::Bid, 50000, 50);
        book.place(bid, &config).unwrap();

        // Place larger ask that partially fills
        let ask = make_order("ask1", Side::Ask, 50000, 100);
        let fills = book.place(ask, &config).unwrap();

        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].size, 50);

        // Rest of ask should be on book
        assert_eq!(book.best_ask(), Some(50000));
        assert!(book.best_bid().is_none());
    }
}
