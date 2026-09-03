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

use std::borrow::Borrow;
use std::cmp::Reverse;
use std::collections::hash_map::{Entry as HashMapEntry, RandomState};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::{Address, MarketConfig, Symbol};
use crate::types::{Price, Size};

/// A shallow, copy-on-write owner for orderbook state.  Immutable access
/// dereferences the shared allocation; any mutable access uses
/// `Arc::make_mut`, so a cloned orderbook can never mutate its parent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CowShared<T>(Arc<T>);

impl<T> Clone for CowShared<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Default> Default for CowShared<T> {
    fn default() -> Self {
        Self(Arc::new(T::default()))
    }
}

impl<T> From<T> for CowShared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Deref for CowShared<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T: Clone> DerefMut for CowShared<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.0)
    }
}

impl<'a, T> IntoIterator for &'a CowShared<T>
where
    &'a T: IntoIterator,
{
    type Item = <&'a T as IntoIterator>::Item;
    type IntoIter = <&'a T as IntoIterator>::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.as_ref().into_iter()
    }
}

impl<T: PartialEq> PartialEq<T> for CowShared<T> {
    fn eq(&self, other: &T) -> bool {
        self.0.as_ref() == other
    }
}

const DERIVED_INDEX_SHARDS: usize = 32;

/// Fixed-shard copy-on-write map for derived orderbook indexes.
///
/// The selector is cloned with the map so every speculative version routes a
/// key to the same shard.  A mutation makes only that shard unique; untouched
/// shards remain shared with the parent and sibling versions.
#[derive(Debug)]
pub(crate) struct ShardedMap<K, V> {
    shards: [CowShared<HashMap<K, V>>; DERIVED_INDEX_SHARDS],
    selector: RandomState,
}

impl<K, V> Clone for ShardedMap<K, V> {
    fn clone(&self) -> Self {
        Self {
            shards: self.shards.clone(),
            selector: self.selector.clone(),
        }
    }
}

impl<K, V> Default for ShardedMap<K, V> {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| CowShared::default()),
            selector: RandomState::new(),
        }
    }
}

impl<K, V> From<HashMap<K, V>> for ShardedMap<K, V>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    fn from(map: HashMap<K, V>) -> Self {
        Self::from_map_with_selector(map, RandomState::new())
    }
}

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    fn from_map_with_selector(map: HashMap<K, V>, selector: RandomState) -> Self {
        let mut sharded = Self {
            shards: std::array::from_fn(|_| CowShared::default()),
            selector,
        };
        for (key, value) in map {
            sharded.insert(key, value);
        }
        sharded
    }
}

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq,
{
    fn shard_index<Q>(&self, key: &Q) -> usize
    where
        Q: ?Sized + Hash,
    {
        let mut hasher = self.selector.build_hasher();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % DERIVED_INDEX_SHARDS
    }
}

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq + Clone,
    V: Clone,
{
    fn shard_mut(&mut self, index: usize) -> &mut HashMap<K, V> {
        &mut self.shards[index]
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let index = self.shard_index(&key);
        self.shard_mut(index).insert(key, value)
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Hash + Eq,
    {
        let index = self.shard_index(key);
        if !self.shards[index].contains_key(key) {
            return None;
        }
        self.shard_mut(index).remove(key)
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Hash + Eq,
    {
        let index = self.shard_index(key);
        if !self.shards[index].contains_key(key) {
            return None;
        }
        self.shard_mut(index).get_mut(key)
    }

    pub(crate) fn entry(&mut self, key: K) -> ShardedEntry<'_, K, V> {
        let index = self.shard_index(&key);
        ShardedEntry {
            inner: self.shard_mut(index).entry(key),
        }
    }
}

impl<K, V> ShardedMap<K, V>
where
    K: Hash + Eq,
{
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: ?Sized + Hash + Eq,
    {
        let index = self.shard_index(key);
        self.shards[index].get(key)
    }

    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|shard| shard.len()).sum()
    }

    #[allow(dead_code)]
    pub(crate) fn is_empty(&self) -> bool {
        self.shards.iter().all(|shard| shard.is_empty())
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.shards.iter().flat_map(|shard| shard.iter())
    }
}

pub(crate) struct ShardedEntry<'a, K, V> {
    inner: HashMapEntry<'a, K, V>,
}

impl<'a, K, V> ShardedEntry<'a, K, V> {
    pub(crate) fn or_insert(self, default: V) -> &'a mut V {
        self.inner.or_insert(default)
    }
}

impl<K, V> PartialEq for ShardedMap<K, V>
where
    K: Hash + Eq,
    V: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|other| other == value))
    }
}

impl<K, V> PartialEq<HashMap<K, V>> for ShardedMap<K, V>
where
    K: Hash + Eq,
    V: PartialEq,
{
    fn eq(&self, other: &HashMap<K, V>) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(key, value)| other.get(key).is_some_and(|other| other == value))
    }
}

impl<K, V> Eq for ShardedMap<K, V>
where
    K: Hash + Eq,
    V: Eq,
{
}

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
    #[serde(default)]
    pub locked_margin: i64,
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
    #[serde(default)]
    pub maker_locked_margin: i64,
    #[serde(default)]
    pub maker_original_size: i64,
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
/// Uses copy-on-write BTreeMaps for O(log n) operations.  Clones share the
/// outer maps, individual price queues, and derived indexes until a mutation:
/// - Bids: `BTreeMap<Reverse<Price>, VecDeque<Order>>` (highest price first)
/// - Asks: `BTreeMap<Price, VecDeque<Order>>` (lowest price first)
/// - Index: fixed-shard COW maps for O(1) lookup with per-shard detach
#[derive(Clone)]
pub struct OrderBook {
    pub(crate) symbol: Symbol,

    /// Bids sorted by price descending (highest = best bid)
    /// Reverse<Price> makes BTreeMap iterate highest-first
    pub(crate) bids: CowShared<BTreeMap<Reverse<Price>, CowShared<VecDeque<Order>>>>,

    /// Asks sorted by price ascending (lowest = best ask)
    pub(crate) asks: CowShared<BTreeMap<Price, CowShared<VecDeque<Order>>>>,

    /// Order index for O(1) cancel lookup: OrderId -> (Side, Price)
    pub(crate) order_index: ShardedMap<OrderId, (Side, Price)>,

    /// Per-trader open order count for O(1) limit checks
    pub(crate) trader_order_counts: ShardedMap<String, usize>,

    /// Last traded price
    pub(crate) last_price: Price,

    /// Sequence number for order IDs
    pub(crate) seq: u64,
}

impl OrderBook {
    pub fn new(symbol: impl Into<String>) -> Self {
        Self {
            symbol: symbol.into(),
            bids: CowShared::default(),
            asks: CowShared::default(),
            order_index: ShardedMap::default(),
            trader_order_counts: ShardedMap::default(),
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

                let trader_lower = cancelled.trader.to_lowercase();
                let remove_trader_count = self
                    .trader_order_counts
                    .get_mut(&trader_lower)
                    .map(|count| {
                        *count = count.saturating_sub(1);
                        *count == 0
                    })
                    .unwrap_or(false);
                if remove_trader_count {
                    self.trader_order_counts.remove(&trader_lower);
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

                let trader_lower = cancelled.trader.to_lowercase();
                let remove_trader_count = self
                    .trader_order_counts
                    .get_mut(&trader_lower)
                    .map(|count| {
                        *count = count.saturating_sub(1);
                        *count == 0
                    })
                    .unwrap_or(false);
                if remove_trader_count {
                    self.trader_order_counts.remove(&trader_lower);
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

    /// Count open orders for a specific trader (for limit enforcement) - O(1)
    pub fn count_orders_by_trader(&self, trader: &str) -> usize {
        let trader_lower = trader.to_lowercase();
        self.trader_order_counts
            .get(&trader_lower)
            .copied()
            .unwrap_or(0)
    }

    /// Rebuild the indexes derived from the primary order queues.
    ///
    /// Queue contents are authoritative.  Both derived maps are assembled in
    /// temporary storage and are replaced only after every level and order has
    /// passed structural validation, so a failed rebuild leaves the book
    /// unchanged.
    pub fn rebuild_derived_indexes(&mut self) -> Result<(), OrderBookError> {
        let (order_index, trader_order_counts) = self.build_derived_indexes()?;

        self.order_index =
            ShardedMap::from_map_with_selector(order_index, self.order_index.selector.clone());
        self.trader_order_counts = ShardedMap::from_map_with_selector(
            trader_order_counts,
            self.trader_order_counts.selector.clone(),
        );
        Ok(())
    }

    /// Validate both derived indexes against the primary order queues without
    /// changing the book.
    pub fn validate_derived_indexes(&self) -> Result<(), OrderBookError> {
        let (order_index, trader_order_counts) = self.build_derived_indexes()?;

        if self.order_index != order_index {
            return Err(OrderBookError::DerivedOrderIndexMismatch);
        }
        if self.trader_order_counts != trader_order_counts {
            return Err(OrderBookError::DerivedTraderOrderCountsMismatch);
        }
        Ok(())
    }

    /// Validate the authoritative orderbook state without consulting or
    /// changing any derived indexes.
    ///
    /// `map_key` is supplied by the owner of the orderbook map because the
    /// map key itself is not stored in an `OrderBook`.  Queue structure is
    /// checked through temporary indexes, but the book's own indexes are
    /// deliberately ignored so this remains useful at import boundaries.
    pub fn validate_primary_state(
        &self,
        map_key: &str,
        config: &MarketConfig,
    ) -> Result<(), OrderBookError> {
        if map_key.trim().is_empty() {
            return Err(OrderBookError::EmptyOrderBookMapKey);
        }
        if self.symbol.trim().is_empty() {
            return Err(OrderBookError::EmptyOrderBookSymbol);
        }
        if self.symbol != map_key {
            return Err(OrderBookError::OrderBookSymbolMismatch {
                map_key: map_key.to_string(),
                book_symbol: self.symbol.clone(),
            });
        }

        config.validate_primary_state()?;
        if config.symbol != map_key {
            return Err(OrderBookError::MarketConfigSymbolMismatch {
                map_key: map_key.to_string(),
                config_symbol: config.symbol.clone(),
            });
        }

        if self.last_price < 0 {
            return Err(OrderBookError::InvalidLastPrice {
                last_price: self.last_price,
            });
        }
        if self.last_price > 0 && self.last_price % config.tick_size != 0 {
            return Err(OrderBookError::PriceNotAligned);
        }

        // Validate every primary order before checking queue structure.  This
        // keeps field-level errors useful even when a malformed order also has
        // a queue-level mismatch.
        for orders in self.bids.values().chain(self.asks.values()) {
            for order in orders {
                Self::validate_primary_order(order, config)?;
            }
        }

        // Build only temporary maps.  This validates empty levels, queue
        // side/price/symbol consistency, duplicate IDs, and generated-ID
        // sequence monotonicity without requiring the stored derived maps to
        // equal the rebuilt values.
        let (_, trader_order_counts) = self.build_derived_indexes()?;

        if self.bids.len() > config.max_price_levels {
            return Err(OrderBookError::DepthLimitReached {
                max: config.max_price_levels,
            });
        }
        if self.asks.len() > config.max_price_levels {
            return Err(OrderBookError::DepthLimitReached {
                max: config.max_price_levels,
            });
        }
        for count in trader_order_counts.values() {
            if *count > config.max_open_orders {
                return Err(OrderBookError::TooManyOpenOrders {
                    max: config.max_open_orders,
                });
            }
        }

        Ok(())
    }

    fn validate_primary_order(order: &Order, config: &MarketConfig) -> Result<(), OrderBookError> {
        if order.id.trim().is_empty() {
            return Err(OrderBookError::EmptyOrderField {
                order_id: order.id.clone(),
                field: "id",
            });
        }
        if order.trader.trim().is_empty() {
            return Err(OrderBookError::EmptyOrderField {
                order_id: order.id.clone(),
                field: "trader",
            });
        }
        if order.symbol.trim().is_empty() {
            return Err(OrderBookError::EmptyOrderField {
                order_id: order.id.clone(),
                field: "symbol",
            });
        }
        if order.size <= 0 {
            return Err(OrderBookError::InvalidSize);
        }
        if order.original_size < order.size {
            return Err(OrderBookError::OriginalSizeTooSmall {
                order_id: order.id.clone(),
                original_size: order.original_size,
                size: order.size,
            });
        }
        if order.price <= 0 {
            return Err(OrderBookError::InvalidPrice);
        }
        if order.locked_margin < 0 {
            return Err(OrderBookError::NegativeLockedMargin {
                order_id: order.id.clone(),
                locked_margin: order.locked_margin,
            });
        }
        if order.order_type == OrderType::Ioc {
            return Err(OrderBookError::QueuedIocOrder {
                order_id: order.id.clone(),
            });
        }

        if order.price % config.tick_size != 0 {
            return Err(OrderBookError::PriceNotAligned);
        }
        if order.size % config.lot_size != 0 || order.original_size % config.lot_size != 0 {
            return Err(OrderBookError::SizeNotAligned);
        }
        if order.size > config.max_order_size {
            return Err(OrderBookError::OrderSizeTooLarge {
                max: config.max_order_size,
                got: order.size,
            });
        }
        if order.original_size > config.max_order_size {
            return Err(OrderBookError::OrderSizeTooLarge {
                max: config.max_order_size,
                got: order.original_size,
            });
        }

        // A partially filled resting order can have a remaining notional
        // below the placement minimum.  The original size is the value that
        // was subject to the minimum-order check at placement time.
        if !order.reduce_only {
            let notional = order_notional(order.original_size, order.price);
            if notional < config.min_notional as i128 {
                return Err(OrderBookError::BelowMinNotional {
                    min: config.min_notional,
                    got: notional_to_i64(notional),
                });
            }
        }

        Ok(())
    }

    fn build_derived_indexes(
        &self,
    ) -> Result<(HashMap<OrderId, (Side, Price)>, HashMap<String, usize>), OrderBookError> {
        let mut order_index = HashMap::new();
        let mut trader_order_counts = HashMap::new();
        let mut order_count = 0usize;
        let mut max_generated_suffix = None;

        for (Reverse(level_price), orders) in &self.bids {
            if orders.is_empty() {
                return Err(OrderBookError::EmptyPriceLevel {
                    side: Side::Bid,
                    price: *level_price,
                });
            }

            for order in orders {
                Self::validate_queued_order(
                    &self.symbol,
                    order,
                    Side::Bid,
                    *level_price,
                    &mut order_index,
                    &mut trader_order_counts,
                    &mut order_count,
                    &mut max_generated_suffix,
                )?;
            }
        }

        for (level_price, orders) in &self.asks {
            if orders.is_empty() {
                return Err(OrderBookError::EmptyPriceLevel {
                    side: Side::Ask,
                    price: *level_price,
                });
            }

            for order in orders {
                Self::validate_queued_order(
                    &self.symbol,
                    order,
                    Side::Ask,
                    *level_price,
                    &mut order_index,
                    &mut trader_order_counts,
                    &mut order_count,
                    &mut max_generated_suffix,
                )?;
            }
        }

        if let Some(max_suffix) = max_generated_suffix {
            if self.seq < max_suffix {
                return Err(OrderBookError::OrderSequenceTooLow {
                    seq: self.seq,
                    suffix: max_suffix,
                });
            }
        }

        Ok((order_index, trader_order_counts))
    }

    fn validate_queued_order(
        symbol: &str,
        order: &Order,
        expected_side: Side,
        level_price: Price,
        order_index: &mut HashMap<OrderId, (Side, Price)>,
        trader_order_counts: &mut HashMap<String, usize>,
        order_count: &mut usize,
        max_generated_suffix: &mut Option<u64>,
    ) -> Result<(), OrderBookError> {
        if order.side != expected_side {
            return Err(OrderBookError::OrderSideMismatch {
                order_id: order.id.clone(),
                expected: expected_side,
                actual: order.side,
            });
        }
        if order.price != level_price {
            return Err(OrderBookError::OrderPriceMismatch {
                order_id: order.id.clone(),
                level_price,
                order_price: order.price,
            });
        }
        if order.symbol != symbol {
            return Err(OrderBookError::OrderSymbolMismatch {
                order_id: order.id.clone(),
                book_symbol: symbol.to_string(),
                order_symbol: order.symbol.clone(),
            });
        }
        if order_index
            .insert(order.id.clone(), (expected_side, level_price))
            .is_some()
        {
            return Err(OrderBookError::DuplicateOrderId(order.id.clone()));
        }

        *order_count = order_count
            .checked_add(1)
            .ok_or(OrderBookError::OrderCountOverflow)?;

        let trader = order.trader.to_lowercase();
        let count = trader_order_counts.entry(trader.clone()).or_insert(0);
        *count = count
            .checked_add(1)
            .ok_or(OrderBookError::TraderOrderCountOverflow { trader })?;

        // Runtime-generated IDs are `<symbol>_<u64>`.  Legacy/test IDs may
        // use another format, so only validate a suffix when it unambiguously
        // matches the runtime format.
        let prefix = format!("{symbol}_");
        if let Some(suffix) = order
            .id
            .strip_prefix(&prefix)
            .and_then(|suffix| suffix.parse::<u64>().ok())
        {
            *max_generated_suffix = Some(
                max_generated_suffix
                    .map(|current| current.max(suffix))
                    .unwrap_or(suffix),
            );
        }

        Ok(())
    }

    // --- Internal helpers ---

    /// Count price levels on a given side
    pub fn price_level_count(&self, side: Side) -> usize {
        match side {
            Side::Bid => self.bids.len(),
            Side::Ask => self.asks.len(),
        }
    }

    /// Check if adding a new price level on given side would exceed depth limit
    pub(crate) fn would_exceed_depth_limit(
        &self,
        side: Side,
        price: Price,
        max_levels: usize,
    ) -> bool {
        let existing = match side {
            Side::Bid => self.bids.contains_key(&Reverse(price)),
            Side::Ask => self.asks.contains_key(&price),
        };
        // If the price level already exists, we're not adding a new one
        if existing {
            return false;
        }
        // Check if we're at the limit
        self.price_level_count(side) >= max_levels
    }

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
        // Check minimum notional value (skip for reduce-only orders like trigger SL/TP
        // which use extreme sweep prices that don't reflect actual fill price)
        if !order.reduce_only {
            let notional = ((order.size as i128 * order.price as i128) / 100_000_000) as i64;
            if notional < config.min_notional {
                return Err(OrderBookError::BelowMinNotional {
                    min: config.min_notional,
                    got: notional,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn would_match(&self, order: &Order) -> bool {
        match order.side {
            Side::Bid => self.best_ask().map(|a| a <= order.price).unwrap_or(false),
            Side::Ask => self.best_bid().map(|b| b >= order.price).unwrap_or(false),
        }
    }

    /// Add a bid order to the book - O(log n)
    pub(crate) fn add_bid(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();
        *self
            .trader_order_counts
            .entry(order.trader.to_lowercase())
            .or_insert(0) += 1;

        self.bids
            .entry(Reverse(price))
            .or_insert_with(CowShared::default)
            .push_back(order);

        self.order_index.insert(id, (Side::Bid, price));
    }

    /// Add an ask order to the book - O(log n)
    pub(crate) fn add_ask(&mut self, order: Order) {
        let price = order.price;
        let id = order.id.clone();
        *self
            .trader_order_counts
            .entry(order.trader.to_lowercase())
            .or_insert(0) += 1;

        self.asks
            .entry(price)
            .or_insert_with(CowShared::default)
            .push_back(order);

        self.order_index.insert(id, (Side::Ask, price));
    }
}

fn order_notional(size: Size, price: Price) -> i128 {
    (size as i128 * price as i128) / 100_000_000
}

fn notional_to_i64(notional: i128) -> i64 {
    notional.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

impl MarketConfig {
    /// Validate market configuration values that are part of primary state.
    ///
    /// This is intentionally limited to representation and arithmetic bounds
    /// needed by execution.  It does not impose product-policy choices such as
    /// a particular fee spread or a relationship between position limits.
    pub fn validate_primary_state(&self) -> Result<(), OrderBookError> {
        if self.symbol.trim().is_empty() {
            return Err(OrderBookError::EmptyMarketConfigSymbol);
        }
        if self.tick_size <= 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "tick_size",
            });
        }
        if self.lot_size <= 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "lot_size",
            });
        }
        if self.min_notional <= 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "min_notional",
            });
        }

        // Fees and rates are basis points.  Keep them within a full 100% so
        // malformed imported state cannot turn a fee/rate into an unbounded
        // debit or credit.  Interest may be signed; funding caps are positive.
        // A negative maker fee is a bounded maker rebate. Taker rebates are
        // not supported by the current fee policy.
        if !(-10_000..=10_000).contains(&self.maker_fee) {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "maker_fee",
            });
        }
        if !(0..=10_000).contains(&self.taker_fee) {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "taker_fee",
            });
        }
        if self.funding_interval_ms == 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "funding_interval_ms",
            });
        }
        if !(-10_000..=10_000).contains(&self.interest_rate_bps) {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "interest_rate_bps",
            });
        }
        if !(0..=10_000).contains(&self.max_funding_rate_bps) {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "max_funding_rate_bps",
            });
        }
        if self.max_order_size <= 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "max_order_size",
            });
        }
        if self.max_position_size <= 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "max_position_size",
            });
        }
        if self.max_open_orders == 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "max_open_orders",
            });
        }
        if self.max_price_levels == 0 {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "max_price_levels",
            });
        }
        if !(0..=10_000).contains(&self.ema_alpha_bps) {
            return Err(OrderBookError::InvalidMarketConfigField {
                symbol: self.symbol.clone(),
                field: "ema_alpha_bps",
            });
        }

        Ok(())
    }
}

/// Orderbook errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum OrderBookError {
    #[error("orderbook map key must not be empty")]
    EmptyOrderBookMapKey,
    #[error("orderbook symbol must not be empty")]
    EmptyOrderBookSymbol,
    #[error("orderbook map key {map_key} does not match book symbol {book_symbol}")]
    OrderBookSymbolMismatch {
        map_key: Symbol,
        book_symbol: Symbol,
    },
    #[error("market config symbol {config_symbol} does not match orderbook map key {map_key}")]
    MarketConfigSymbolMismatch {
        map_key: Symbol,
        config_symbol: Symbol,
    },
    #[error("market config symbol must not be empty")]
    EmptyMarketConfigSymbol,
    #[error("market config {symbol} has invalid {field}")]
    InvalidMarketConfigField { symbol: Symbol, field: &'static str },
    #[error("order {order_id} has an empty {field}")]
    EmptyOrderField {
        order_id: OrderId,
        field: &'static str,
    },
    #[error("order {order_id} has negative locked margin {locked_margin}")]
    NegativeLockedMargin {
        order_id: OrderId,
        locked_margin: i64,
    },
    #[error("IOC order {order_id} must not remain in an orderbook queue")]
    QueuedIocOrder { order_id: OrderId },
    #[error("order {order_id} has original size {original_size} below remaining size {size}")]
    OriginalSizeTooSmall {
        order_id: OrderId,
        original_size: Size,
        size: Size,
    },
    #[error("orderbook last price must be non-negative, got {last_price}")]
    InvalidLastPrice { last_price: Price },
    #[error("empty {side:?} price level at {price}")]
    EmptyPriceLevel { side: Side, price: Price },
    #[error("order {order_id} has side {actual:?}, but is in the {expected:?} queue")]
    OrderSideMismatch {
        order_id: OrderId,
        expected: Side,
        actual: Side,
    },
    #[error("order {order_id} has price {order_price}, but its level is {level_price}")]
    OrderPriceMismatch {
        order_id: OrderId,
        level_price: Price,
        order_price: Price,
    },
    #[error("order {order_id} has symbol {order_symbol}, but the book is {book_symbol}")]
    OrderSymbolMismatch {
        order_id: OrderId,
        book_symbol: Symbol,
        order_symbol: Symbol,
    },
    #[error("duplicate order id: {0}")]
    DuplicateOrderId(OrderId),
    #[error("order count overflow")]
    OrderCountOverflow,
    #[error("open-order count overflow for trader {trader}")]
    TraderOrderCountOverflow { trader: String },
    #[error("order sequence {seq} is below generated order id suffix {suffix}")]
    OrderSequenceTooLow { seq: u64, suffix: u64 },
    #[error("derived order index does not match the primary order queues")]
    DerivedOrderIndexMismatch,
    #[error("derived trader order counts do not match the primary order queues")]
    DerivedTraderOrderCountsMismatch,
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
    #[error("orderbook depth limit reached (max: {max} price levels)")]
    DepthLimitReached { max: usize },
    #[error("order notional {got} below minimum {min}")]
    BelowMinNotional { min: i64, got: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn primary_config() -> MarketConfig {
        let mut config = MarketConfig::default();
        config.min_notional = 1;
        config
    }

    fn primary_order(id: &str, trader: &str, price: Price) -> Order {
        Order {
            id: id.to_string(),
            trader: trader.to_string(),
            symbol: "BTC-USDT".to_string(),
            side: Side::Bid,
            price,
            size: 100,
            original_size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 0,
            locked_margin: 0,
        }
    }

    fn order(id: &str, trader: &str, symbol: &str, side: Side, price: Price) -> Order {
        Order {
            id: id.to_string(),
            trader: trader.to_string(),
            symbol: symbol.to_string(),
            side,
            price,
            size: 1,
            original_size: 1,
            order_type: OrderType::Gtc,
            reduce_only: false,
            timestamp: 0,
            locked_margin: 0,
        }
    }

    #[test]
    fn rebuild_derived_indexes_from_valid_queues() {
        let mut book = OrderBook::new("BTC-USDT");
        book.seq = 2;
        book.bids.entry(Reverse(100)).or_default().push_back(order(
            "BTC-USDT_1",
            "Alice",
            "BTC-USDT",
            Side::Bid,
            100,
        ));
        book.asks.entry(110).or_default().push_back(order(
            "BTC-USDT_2",
            "alice",
            "BTC-USDT",
            Side::Ask,
            110,
        ));

        book.rebuild_derived_indexes().unwrap();
        book.validate_derived_indexes().unwrap();

        assert_eq!(book.order_index.get("BTC-USDT_1"), Some(&(Side::Bid, 100)));
        assert_eq!(book.order_index.get("BTC-USDT_2"), Some(&(Side::Ask, 110)));
        assert_eq!(book.trader_order_counts.get("alice"), Some(&2));
    }

    #[test]
    fn rebuild_repairs_stale_derived_indexes() {
        let mut book = OrderBook::new("BTC-USDT");
        book.bids.entry(Reverse(100)).or_default().push_back(order(
            "bid-1",
            "Alice",
            "BTC-USDT",
            Side::Bid,
            100,
        ));
        book.order_index.insert("stale".into(), (Side::Ask, 999));
        book.trader_order_counts.insert("alice".into(), 99);
        book.trader_order_counts.insert("stale".into(), 1);

        assert!(matches!(
            book.validate_derived_indexes(),
            Err(OrderBookError::DerivedOrderIndexMismatch)
        ));
        book.rebuild_derived_indexes().unwrap();

        assert_eq!(book.order_index.len(), 1);
        assert_eq!(book.order_index.get("bid-1"), Some(&(Side::Bid, 100)));
        assert_eq!(book.trader_order_counts.len(), 1);
        assert_eq!(book.trader_order_counts.get("alice"), Some(&1));
    }

    #[test]
    fn duplicate_order_id_is_rejected_without_partial_mutation() {
        let mut book = OrderBook::new("BTC-USDT");
        book.bids.entry(Reverse(100)).or_default().push_back(order(
            "duplicate",
            "Alice",
            "BTC-USDT",
            Side::Bid,
            100,
        ));
        book.asks.entry(110).or_default().push_back(order(
            "duplicate",
            "Bob",
            "BTC-USDT",
            Side::Ask,
            110,
        ));
        book.order_index.insert("existing".into(), (Side::Bid, 1));
        book.trader_order_counts.insert("existing".into(), 7);
        let original_index = book.order_index.clone();
        let original_counts = book.trader_order_counts.clone();

        assert!(matches!(
            book.rebuild_derived_indexes(),
            Err(OrderBookError::DuplicateOrderId(id)) if id == "duplicate"
        ));
        assert_eq!(book.order_index, original_index);
        assert_eq!(book.trader_order_counts, original_counts);
    }

    #[test]
    fn order_mismatch_is_rejected_without_partial_mutation() {
        let mut book = OrderBook::new("BTC-USDT");
        book.bids.entry(Reverse(100)).or_default().push_back(order(
            "bad",
            "Alice",
            "BTC-USDT",
            Side::Ask,
            100,
        ));
        book.order_index.insert("existing".into(), (Side::Ask, 2));
        book.trader_order_counts.insert("existing".into(), 3);
        let original_index = book.order_index.clone();
        let original_counts = book.trader_order_counts.clone();

        assert!(matches!(
            book.rebuild_derived_indexes(),
            Err(OrderBookError::OrderSideMismatch {
                order_id,
                expected: Side::Bid,
                actual: Side::Ask,
            }) if order_id == "bad"
        ));
        assert_eq!(book.order_index, original_index);
        assert_eq!(book.trader_order_counts, original_counts);
    }

    #[test]
    fn primary_validation_ignores_stale_derived_indexes() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = primary_config();
        book.seq = 1;
        book.bids
            .entry(Reverse(100_000_000))
            .or_default()
            .push_back(primary_order("BTC-USDT_1", "Alice", 100_000_000));
        book.order_index.insert("stale".into(), (Side::Ask, 1));
        book.trader_order_counts.insert("alice".into(), 99);

        book.validate_primary_state("BTC-USDT", &config).unwrap();
        assert_eq!(book.order_index.get("stale"), Some(&(Side::Ask, 1)));
        assert_eq!(book.trader_order_counts.get("alice"), Some(&99));
    }

    #[test]
    fn primary_validation_rejects_bad_order_without_mutating_indexes() {
        let mut book = OrderBook::new("BTC-USDT");
        let config = primary_config();
        let mut bad = primary_order("BTC-USDT_1", "Alice", 100_000_000);
        bad.id.clear();
        book.bids
            .entry(Reverse(100_000_000))
            .or_default()
            .push_back(bad);
        book.order_index.insert("existing".into(), (Side::Bid, 1));
        book.trader_order_counts.insert("alice".into(), 7);
        let original_index = book.order_index.clone();
        let original_counts = book.trader_order_counts.clone();

        assert!(matches!(
            book.validate_primary_state("BTC-USDT", &config),
            Err(OrderBookError::EmptyOrderField { field: "id", .. })
        ));
        assert_eq!(book.order_index, original_index);
        assert_eq!(book.trader_order_counts, original_counts);
    }

    #[test]
    fn primary_validation_enforces_depth_and_open_order_limits() {
        let mut book = OrderBook::new("BTC-USDT");
        let mut config = primary_config();
        config.max_price_levels = 1;
        book.seq = 2;
        book.bids
            .entry(Reverse(100_000_000))
            .or_default()
            .push_back(primary_order("BTC-USDT_1", "Alice", 100_000_000));
        book.bids
            .entry(Reverse(101_000_000))
            .or_default()
            .push_back(primary_order("BTC-USDT_2", "Alice", 101_000_000));

        assert!(matches!(
            book.validate_primary_state("BTC-USDT", &config),
            Err(OrderBookError::DepthLimitReached { max: 1 })
        ));

        config.max_price_levels = 2;
        config.max_open_orders = 1;
        assert!(matches!(
            book.validate_primary_state("BTC-USDT", &config),
            Err(OrderBookError::TooManyOpenOrders { max: 1 })
        ));
    }

    #[test]
    fn primary_validation_checks_market_config_bounds() {
        let mut config = primary_config();
        config.tick_size = 0;
        assert!(matches!(
            config.validate_primary_state(),
            Err(OrderBookError::InvalidMarketConfigField {
                field: "tick_size",
                ..
            })
        ));

        let mut config = primary_config();
        config.maker_fee = -10_001;
        assert!(matches!(
            config.validate_primary_state(),
            Err(OrderBookError::InvalidMarketConfigField {
                field: "maker_fee",
                ..
            })
        ));

        let mut config = primary_config();
        config.max_open_orders = 0;
        assert!(matches!(
            config.validate_primary_state(),
            Err(OrderBookError::InvalidMarketConfigField {
                field: "max_open_orders",
                ..
            })
        ));
    }

    #[test]
    fn partial_order_is_checked_against_original_minimum_notional() {
        let mut book = OrderBook::new("BTC-USDT");
        let mut config = primary_config();
        config.min_notional = 100;
        book.seq = 1;
        let mut partial = primary_order("BTC-USDT_1", "Alice", 100_000_000);
        partial.size = 1;
        partial.original_size = 100_000_000;
        book.bids
            .entry(Reverse(100_000_000))
            .or_default()
            .push_back(partial);

        book.validate_primary_state("BTC-USDT", &config).unwrap();
    }

    #[test]
    fn primary_validation_rejects_ioc_queue_and_unaligned_last_price() {
        let mut book = OrderBook::new("BTC-USDT");
        let mut config = primary_config();
        config.tick_size = 10;
        book.seq = 1;
        let mut ioc = primary_order("BTC-USDT_1", "Alice", 100_000_000);
        ioc.order_type = OrderType::Ioc;
        book.bids
            .entry(Reverse(100_000_000))
            .or_default()
            .push_back(ioc);
        assert!(matches!(
            book.validate_primary_state("BTC-USDT", &config),
            Err(OrderBookError::QueuedIocOrder { .. })
        ));

        book.bids.clear();
        book.last_price = 101;
        assert!(matches!(
            book.validate_primary_state("BTC-USDT", &config),
            Err(OrderBookError::PriceNotAligned)
        ));
    }

    #[test]
    fn cloned_orderbook_detaches_same_and_new_price_levels() {
        let mut parent = OrderBook::new("BTC-USDT");
        parent.add_bid(order("bid-100", "alice", "BTC-USDT", Side::Bid, 100));
        parent.add_bid(order("bid-200", "alice", "BTC-USDT", Side::Bid, 200));

        let mut child = parent.clone();
        assert_eq!(Arc::strong_count(&parent.bids.0), 2);
        assert!(Arc::ptr_eq(&parent.bids.0, &child.bids.0));

        // Mutating an existing level detaches only that queue and the outer
        // map; the parent's queue and its derived maps remain unchanged.
        child.add_bid(order("bid-100-child", "bob", "BTC-USDT", Side::Bid, 100));
        child.add_bid(order("bid-300-child", "bob", "BTC-USDT", Side::Bid, 300));

        assert_eq!(parent.bid_levels(3)[0].order_count, 1);
        assert_eq!(parent.bid_levels(3).len(), 2);
        assert_eq!(child.bid_levels(3)[0].price, 300);
        assert_eq!(child.bid_levels(3).len(), 3);
        assert_eq!(child.bid_levels(3)[2].order_count, 2);
        assert_eq!(parent.count_orders_by_trader("bob"), 0);
        assert_eq!(child.count_orders_by_trader("bob"), 2);
        assert!(parent.order_index.get("bid-100-child").is_none());
        assert!(child.order_index.get("bid-100-child").is_some());
        parent.validate_derived_indexes().unwrap();
        child.validate_derived_indexes().unwrap();
    }

    #[test]
    fn cloned_orderbook_cancel_and_match_do_not_mutate_parent_or_sibling() {
        let mut parent = OrderBook::new("BTC-USDT");
        parent.add_bid(order("bid-100", "alice", "BTC-USDT", Side::Bid, 100));
        let mut child = parent.clone();
        let mut sibling = parent.clone();

        assert!(child.cancel("bid-100").is_some());
        assert!(parent.order_index.get("bid-100").is_some());
        assert!(sibling.order_index.get("bid-100").is_some());
        assert_eq!(parent.best_bid(), Some(100));
        assert_eq!(sibling.best_bid(), Some(100));

        let mut config = MarketConfig::default();
        config.min_notional = 0;
        let fills = sibling
            .place(order("ask-100", "bob", "BTC-USDT", Side::Ask, 100), &config)
            .unwrap();
        assert_eq!(fills.len(), 1);
        assert!(sibling.best_bid().is_none());
        assert_eq!(parent.best_bid(), Some(100));
        parent.validate_derived_indexes().unwrap();
        sibling.validate_derived_indexes().unwrap();
    }

    #[test]
    fn cloned_orderbook_detaches_only_changed_derived_index_shards() {
        let mut parent = OrderBook::new("BTC-USDT");
        for index in 0..128 {
            parent.add_bid(order(
                &format!("order-{index}"),
                &format!("trader-{index}"),
                "BTC-USDT",
                Side::Bid,
                100 + index,
            ));
        }

        let child_order_id = "child-order";
        let child_trader = "child-trader";
        let changed_order_shard = parent.order_index.shard_index(child_order_id);
        let untouched_order_shard = (0..DERIVED_INDEX_SHARDS)
            .find(|&shard| shard != changed_order_shard)
            .unwrap();
        let changed_trader_shard = parent.trader_order_counts.shard_index(child_trader);
        let untouched_trader_shard = (0..DERIVED_INDEX_SHARDS)
            .find(|&shard| shard != changed_trader_shard)
            .unwrap();

        let mut child = parent.clone();
        let sibling = parent.clone();
        assert_eq!(
            Arc::strong_count(&parent.order_index.shards[untouched_order_shard].0),
            3
        );
        assert_eq!(
            Arc::strong_count(&parent.trader_order_counts.shards[untouched_trader_shard].0),
            3
        );

        child.add_bid(order(
            child_order_id,
            child_trader,
            "BTC-USDT",
            Side::Bid,
            1_000,
        ));

        assert!(!Arc::ptr_eq(
            &parent.order_index.shards[changed_order_shard].0,
            &child.order_index.shards[changed_order_shard].0
        ));
        assert!(Arc::ptr_eq(
            &parent.order_index.shards[untouched_order_shard].0,
            &child.order_index.shards[untouched_order_shard].0
        ));
        assert!(!Arc::ptr_eq(
            &parent.trader_order_counts.shards[changed_trader_shard].0,
            &child.trader_order_counts.shards[changed_trader_shard].0
        ));
        assert!(Arc::ptr_eq(
            &parent.trader_order_counts.shards[untouched_trader_shard].0,
            &child.trader_order_counts.shards[untouched_trader_shard].0
        ));

        assert!(child.order_index.get(child_order_id).is_some());
        assert!(parent.order_index.get(child_order_id).is_none());
        assert!(sibling.order_index.get(child_order_id).is_none());
        assert_eq!(child.count_orders_by_trader(child_trader), 1);
        assert_eq!(parent.count_orders_by_trader(child_trader), 0);
        assert_eq!(sibling.count_orders_by_trader(child_trader), 0);
    }
}
