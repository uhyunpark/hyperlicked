# Orderbook & Matching Engine

Deep dive into the heap-based orderbook and order matching patterns.

## Table of Contents

- [Architecture](#architecture)
- [Data Structures](#data-structures)
- [Order Types](#order-types)
- [Matching Algorithm](#matching-algorithm)
- [3-Bucket Mempool](#3-bucket-mempool)
- [Fill Processing](#fill-processing)

---

## Architecture

```
src/app/
├── orderbook.rs    # Heap-based matching engine
├── mempool.rs      # 3-bucket transaction ordering
├── accounts.rs     # Position & margin tracking
├── state.rs        # AppState (implements AppHook)
├── candles.rs      # OHLCV aggregation
├── funding.rs      # Funding rate payments
└── liquidation.rs  # Liquidation engine
```

---

## Data Structures

### OrderBook

```rust
pub struct OrderBook {
    symbol: Symbol,
    bid_heap: BinaryHeap<Price>,           // Max-heap for bids
    ask_heap: BinaryHeap<Reverse<Price>>,  // Min-heap for asks
    bids: HashMap<Price, Vec<Order>>,      // FIFO queue at each price
    asks: HashMap<Price, Vec<Order>>,
    order_index: HashMap<OrderId, (Side, Price)>,  // O(1) cancel lookup
    last_price: Price,
    seq: u64,
}
```

### Complexity

| Operation | Complexity |
|-----------|------------|
| Insert | O(log N) |
| Best bid/ask | O(1) |
| Cancel | O(1) |
| Match | O(log N) per fill |

### Order Structure

```rust
pub struct Order {
    pub id: OrderId,
    pub trader: Address,
    pub symbol: Symbol,
    pub side: Side,
    pub price: Price,           // i64 in cents
    pub size: Size,             // i64 in satoshis
    pub filled_size: Size,
    pub order_type: OrderType,
    pub reduce_only: bool,
    pub timestamp: u64,
}
```

---

## Order Types

```rust
pub enum OrderType {
    Gtc,  // Good-til-cancelled: rest on book
    Ioc,  // Immediate-or-cancel: fill or cancel
    Alo,  // Add-liquidity-only: reject if would match
}
```

### GTC (Good-til-Cancelled)

```rust
// Match what we can, rest on book
if order.size > 0 && order.order_type == OrderType::Gtc {
    self.add_to_book(order);
}
```

### IOC (Immediate-or-Cancel)

```rust
// Match what we can, cancel the rest
if order.size > 0 && order.order_type == OrderType::Ioc {
    // Don't add to book - remaining is cancelled
}
```

### ALO (Add-Liquidity-Only)

```rust
// Reject if would match immediately
if order.order_type == OrderType::Alo && self.would_match(&order) {
    return Err(OrderBookError::AloWouldMatch);
}
```

---

## Matching Algorithm

### Main Flow

```rust
impl OrderBook {
    pub fn place(&mut self, mut order: Order, config: &MarketConfig)
        -> Result<Vec<Fill>>
    {
        // 1. Validate order
        self.validate_order(&order, config)?;

        // 2. ALO check
        if order.order_type == OrderType::Alo && self.would_match(&order) {
            return Err(OrderBookError::AloWouldMatch);
        }

        let mut fills = Vec::new();

        // 3. Matching loop (FIFO at each price level)
        while order.size > 0 && self.can_match(&order) {
            let (matching_orders, price) = self.best_opposite_level(&order);

            for maker_order in matching_orders {
                let fill_size = order.size.min(maker_order.size);

                fills.push(Fill {
                    taker_id: order.id,
                    maker_id: maker_order.id,
                    price,
                    size: fill_size,
                    taker_side: order.side,
                    timestamp: current_time(),
                });

                order.size -= fill_size;
                // Update maker order...

                if order.size == 0 {
                    break;
                }
            }
        }

        // 4. Rest on book or cancel
        if order.size > 0 && order.order_type == OrderType::Gtc {
            self.add_to_book(order);
        }

        Ok(fills)
    }
}
```

### Can Match

```rust
fn can_match(&self, order: &Order) -> bool {
    match order.side {
        Side::Bid => {
            if let Some(&best_ask) = self.ask_heap.peek() {
                order.price >= best_ask.0  // Bid crosses or equals best ask
            } else {
                false
            }
        }
        Side::Ask => {
            if let Some(&best_bid) = self.bid_heap.peek() {
                order.price <= best_bid  // Ask crosses or equals best bid
            } else {
                false
            }
        }
    }
}
```

### FIFO Within Price Level

Orders at the same price execute in arrival order:

```rust
// HashMap<Price, Vec<Order>> - Vec maintains insertion order
fn add_to_book(&mut self, order: Order) {
    let orders = match order.side {
        Side::Bid => self.bids.entry(order.price).or_default(),
        Side::Ask => self.asks.entry(order.price).or_default(),
    };
    orders.push(order);  // Append to end (FIFO)
}
```

---

## 3-Bucket Mempool

### Bucket Priority

```rust
impl Transaction {
    pub fn bucket(&self) -> u8 {
        match self {
            Transaction::Deposit { .. } |
            Transaction::Withdraw { .. } => 0,  // Highest priority
            Transaction::CancelOrder { .. } => 1,
            Transaction::PlaceOrder { .. } => 2,  // Lowest priority
        }
    }
}
```

### Ordering Rationale

1. **Deposits/Withdraws first**: Ensure margin is available
2. **Cancels second**: Allow users to cancel before matching
3. **Orders last**: Match with updated state

### Payload Preparation

```rust
impl AppState {
    fn prepare_payload(&self, _parent: &Block) -> Vec<u8> {
        self.mempool.get_transactions_by_bucket()
            .into_iter()
            .flat_map(|tx| tx.to_bytes())
            .collect()
    }
}
```

---

## Fill Processing

### Fill Structure

```rust
pub struct Fill {
    pub id: FillId,
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
```

### Processing Fills

```rust
fn process_fill(&mut self, fill: &Fill) {
    // 1. Calculate notional value
    let notional = (fill.size * fill.price) / 100_000_000;

    // 2. Update positions
    self.accounts.apply_fill(
        &fill.taker,
        &fill.symbol,
        fill.side,
        fill.size,
        fill.price
    );
    self.accounts.apply_fill(
        &fill.maker,
        &fill.symbol,
        fill.side.opposite(),
        fill.size,
        fill.price
    );

    // 3. Update last price
    self.orderbooks.get_mut(&fill.symbol)
        .map(|ob| ob.set_last_price(fill.price));

    // 4. Emit event (for WebSocket)
    self.pending_fills.push(fill.clone());
}
```

---

## Order Validation

```rust
fn validate_order(&self, order: &Order, config: &MarketConfig)
    -> Result<(), OrderBookError>
{
    // 1. Price must be positive
    if order.price <= 0 {
        return Err(OrderBookError::InvalidPrice);
    }

    // 2. Price must align to tick size
    if order.price % config.tick_size != 0 {
        return Err(OrderBookError::PriceNotAligned);
    }

    // 3. Size must be positive
    if order.size <= 0 {
        return Err(OrderBookError::InvalidSize);
    }

    // 4. Size must align to lot size
    if order.size % config.lot_size != 0 {
        return Err(OrderBookError::SizeNotAligned);
    }

    // 5. Notional must meet minimum
    let notional = (order.size * order.price) / 100_000_000;
    if notional < config.min_notional {
        return Err(OrderBookError::NotionalTooSmall);
    }

    Ok(())
}
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [TYPES.md](TYPES.md) - Core type definitions
- [CONSENSUS.md](CONSENSUS.md) - How blocks execute orders
