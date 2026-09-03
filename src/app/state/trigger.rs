//! Trigger Order Execution
//!
//! Handles placement, cancellation, and processing of trigger orders.

use crate::app::{
    orderbook::{Fill, Side},
    trigger::{
        determine_trigger_condition, determine_trigger_side, validate_trigger_price, Cloid,
        TriggerError, TriggerEvent, TriggerEventType, TriggerOrder, TriggerOrderId,
        TriggerOrderStatus, TriggerOrderValidationError, TriggerType,
    },
    Address, OrderType, Symbol, Transaction,
};
use crate::types::Price;

use super::{AppState, CowMap, TriggerIndexError};

type TriggerIndexes = (
    CowMap<Address, Vec<TriggerOrderId>>,
    CowMap<Symbol, Vec<TriggerOrderId>>,
    CowMap<(Address, Symbol, Cloid), TriggerOrderId>,
);

fn sequence_from_trigger_id(id: &str) -> Option<u64> {
    let suffix = id.strip_prefix('T')?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

fn strict_sequence_from_trigger_id(id: &str) -> Option<u64> {
    let suffix = id.strip_prefix('T')?;
    if suffix.is_empty()
        || suffix == "0"
        || suffix.starts_with('0')
        || !suffix.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    suffix.parse().ok()
}

fn compare_trigger_ids(left: &str, right: &str) -> std::cmp::Ordering {
    match (
        sequence_from_trigger_id(left),
        sequence_from_trigger_id(right),
    ) {
        (Some(left_sequence), Some(right_sequence)) => left_sequence
            .cmp(&right_sequence)
            .then_with(|| left.cmp(right)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => left.cmp(right),
    }
}

impl AppState {
    /// Validate the primary trigger-order records without changing state.
    ///
    /// Only pending trigger orders are retained in `trigger_orders`; triggered,
    /// cancelled, and failed orders are removed by `cleanup_trigger_order`.
    /// Position existence is intentionally not checked here because a position
    /// may disappear between placement and trigger processing; that path is
    /// handled deterministically by `execute_trigger`.
    pub fn validate_trigger_orders(&self) -> Result<(), TriggerOrderValidationError> {
        let mut seen_cloids = std::collections::HashSet::new();
        let mut orders: Vec<_> = self.trigger_orders.iter().collect();
        orders.sort_by(|left, right| compare_trigger_ids(left.0, right.0));

        for (map_id, order) in orders {
            if map_id != &order.id {
                return Err(TriggerOrderValidationError::OrderIdMismatch {
                    map_key: map_id.clone(),
                    order_id: order.id.clone(),
                });
            }

            let sequence = strict_sequence_from_trigger_id(&order.id).ok_or_else(|| {
                TriggerOrderValidationError::InvalidId {
                    order_id: order.id.clone(),
                }
            })?;
            if sequence > self.trigger_seq {
                return Err(TriggerOrderValidationError::SequenceBehind {
                    order_id: order.id.clone(),
                    sequence,
                    trigger_seq: self.trigger_seq,
                });
            }

            if order.trader.trim().is_empty() {
                return Err(TriggerOrderValidationError::EmptyTrader {
                    order_id: order.id.clone(),
                });
            }
            if order.symbol.trim().is_empty() {
                return Err(TriggerOrderValidationError::EmptySymbol {
                    order_id: order.id.clone(),
                });
            }
            if !self.configs.contains_key(&order.symbol) {
                return Err(TriggerOrderValidationError::MarketNotFound {
                    order_id: order.id.clone(),
                    symbol: order.symbol.clone(),
                });
            }
            if !self.orderbooks.contains_key(&order.symbol) {
                return Err(TriggerOrderValidationError::OrderbookNotFound {
                    order_id: order.id.clone(),
                    symbol: order.symbol.clone(),
                });
            }

            if let Some(cloid) = &order.cloid {
                if cloid.trim().is_empty() {
                    return Err(TriggerOrderValidationError::EmptyCloid {
                        order_id: order.id.clone(),
                    });
                }
                let key = (order.trader.clone(), order.symbol.clone(), cloid.clone());
                if !seen_cloids.insert(key) {
                    return Err(TriggerOrderValidationError::DuplicateCloid {
                        trader: order.trader.clone(),
                        symbol: order.symbol.clone(),
                        cloid: cloid.clone(),
                    });
                }
            }

            if order.size <= 0 {
                return Err(TriggerOrderValidationError::InvalidSize {
                    order_id: order.id.clone(),
                    size: order.size,
                });
            }
            if order.trigger_price <= 0 {
                return Err(TriggerOrderValidationError::InvalidTriggerPrice {
                    order_id: order.id.clone(),
                    price: order.trigger_price,
                });
            }
            if let Some(limit_price) = order.limit_price {
                if limit_price <= 0 {
                    return Err(TriggerOrderValidationError::InvalidLimitPrice {
                        order_id: order.id.clone(),
                        price: limit_price,
                    });
                }
            }
            if !order.reduce_only {
                return Err(TriggerOrderValidationError::NotReduceOnly {
                    order_id: order.id.clone(),
                });
            }

            let position_sign = match order.side {
                Side::Ask => 1,
                Side::Bid => -1,
            };
            let expected_condition = determine_trigger_condition(position_sign, order.trigger_type);
            if order.condition != expected_condition {
                return Err(TriggerOrderValidationError::ConditionMismatch {
                    order_id: order.id.clone(),
                    expected: expected_condition,
                    actual: order.condition,
                });
            }
            if order.status != TriggerOrderStatus::Pending {
                return Err(TriggerOrderValidationError::InvalidStatus {
                    order_id: order.id.clone(),
                    status: order.status,
                });
            }
        }

        Ok(())
    }

    /// Rebuild all transient trigger-order indexes from the primary order map.
    ///
    /// The indexes are replaced only after every order has passed validation,
    /// so a malformed order cannot leave a partially rebuilt state behind.
    pub fn rebuild_trigger_indexes(&mut self) -> Result<(), TriggerIndexError> {
        let (by_trader, by_symbol, by_cloid) = self.build_trigger_indexes()?;
        self.trigger_orders_by_trader.replace(by_trader);
        self.trigger_orders_by_symbol.replace(by_symbol);
        self.trigger_orders_by_cloid.replace(by_cloid);
        Ok(())
    }

    /// Validate all trigger-order indexes without mutating application state.
    pub fn validate_trigger_indexes(&self) -> Result<(), TriggerIndexError> {
        let (expected_by_trader, expected_by_symbol, expected_by_cloid) =
            self.build_trigger_indexes()?;

        if self.trigger_orders_by_trader != expected_by_trader
            || self.trigger_orders_by_symbol != expected_by_symbol
            || self.trigger_orders_by_cloid != expected_by_cloid
        {
            return Err(TriggerIndexError::IndexMismatch);
        }
        Ok(())
    }

    fn build_trigger_indexes(&self) -> Result<TriggerIndexes, TriggerIndexError> {
        let mut by_trader = CowMap::new();
        let mut by_symbol = CowMap::new();
        let mut by_cloid = CowMap::new();
        let mut orders: Vec<_> = self.trigger_orders.iter().collect();
        orders.sort_by(|left, right| compare_trigger_ids(left.0, right.0));

        for (id, order) in orders {
            if id != &order.id {
                return Err(TriggerIndexError::OrderIdMismatch);
            }

            let id = id.clone();
            by_trader
                .entry(order.trader.clone())
                .or_insert_with(Vec::new)
                .push(id.clone());
            by_symbol
                .entry(order.symbol.clone())
                .or_insert_with(Vec::new)
                .push(id.clone());

            if let Some(cloid) = &order.cloid {
                let key = (order.trader.clone(), order.symbol.clone(), cloid.clone());
                if by_cloid.insert(key, id).is_some() {
                    return Err(TriggerIndexError::DuplicateCloid);
                }
            }
        }

        if self
            .trigger_orders
            .keys()
            .filter_map(|id| sequence_from_trigger_id(id))
            .max()
            .is_some_and(|max_id| max_id > self.trigger_seq)
        {
            return Err(TriggerIndexError::TriggerSequenceBehind);
        }

        Ok((by_trader, by_symbol, by_cloid))
    }

    /// Generate next trigger order ID
    fn next_trigger_id(&mut self) -> TriggerOrderId {
        self.trigger_seq += 1;
        format!("T{}", self.trigger_seq)
    }

    /// Remove a trigger order from all indexes
    fn cleanup_trigger_order(&mut self, id: &str) -> Option<TriggerOrder> {
        let order = self.trigger_orders.remove(id)?;

        let remove_trader_index =
            if let Some(ids) = self.trigger_orders_by_trader.get_mut(&order.trader) {
                ids.retain(|i| i != id);
                ids.is_empty()
            } else {
                false
            };
        if remove_trader_index {
            self.trigger_orders_by_trader.remove(&order.trader);
        }

        let remove_symbol_index =
            if let Some(ids) = self.trigger_orders_by_symbol.get_mut(&order.symbol) {
                ids.retain(|i| i != id);
                ids.is_empty()
            } else {
                false
            };
        if remove_symbol_index {
            self.trigger_orders_by_symbol.remove(&order.symbol);
        }
        if let Some(ref cloid) = order.cloid {
            let key = (order.trader.clone(), order.symbol.clone(), cloid.clone());
            self.trigger_orders_by_cloid.remove(&key);
        }

        Some(order)
    }

    /// Place a new trigger order (Stop Loss or Take Profit)
    pub(crate) fn execute_place_trigger_order(
        &mut self,
        trader: Address,
        symbol: Symbol,
        trigger_type: TriggerType,
        trigger_price: Price,
        size: i64,
        limit_price: Option<Price>,
        cloid: Option<Cloid>,
    ) -> Result<TriggerOrderId, TriggerError> {
        // Validate market exists
        if !self.configs.contains_key(&symbol) {
            return Err(TriggerError::MarketNotFound);
        }
        if size <= 0 {
            return Err(TriggerError::InvalidSize);
        }
        if limit_price.is_some_and(|price| price <= 0) {
            return Err(TriggerError::InvalidLimitPrice);
        }
        if cloid.as_ref().is_some_and(|cloid| cloid.trim().is_empty()) {
            return Err(TriggerError::InvalidCloid);
        }

        // Check for duplicate cloid
        if let Some(ref cloid) = cloid {
            let key = (trader.clone(), symbol.clone(), cloid.clone());
            if self.trigger_orders_by_cloid.contains_key(&key) {
                return Err(TriggerError::DuplicateCloid);
            }
        }

        // Get trader's position
        let account = self.accounts.get(&trader).ok_or(TriggerError::NoPosition)?;
        let position = account.position(&symbol);

        // Must have an open position
        if position.size == 0 {
            return Err(TriggerError::NoPosition);
        }

        // Validate size doesn't exceed position
        if size > position.size.abs() {
            return Err(TriggerError::SizeExceedsPosition);
        }

        // Get mark price for validation
        let mark_price = self.mark_prices.get(&symbol).copied().unwrap_or(0);

        // Validate trigger price is in the correct direction
        validate_trigger_price(position.size, trigger_type, trigger_price, mark_price)?;

        // Determine trigger condition and side based on position
        let condition = determine_trigger_condition(position.size, trigger_type);
        let side = determine_trigger_side(position.size);

        // Generate ID and create order
        let id = self.next_trigger_id();

        let trigger_order = TriggerOrder {
            id: id.clone(),
            cloid: cloid.clone(),
            trader: trader.clone(),
            symbol: symbol.clone(),
            side,
            size,
            trigger_type,
            condition,
            trigger_price,
            limit_price,
            reduce_only: true, // SL/TP are always reduce-only
            timestamp: self.timestamp,
            status: TriggerOrderStatus::Pending,
        };

        // Store in indexes
        self.trigger_orders.insert(id.clone(), trigger_order);

        self.trigger_orders_by_trader
            .entry(trader.clone())
            .or_default()
            .push(id.clone());

        self.trigger_orders_by_symbol
            .entry(symbol.clone())
            .or_default()
            .push(id.clone());

        if let Some(cloid) = cloid {
            self.trigger_orders_by_cloid
                .insert((trader.clone(), symbol.clone(), cloid), id.clone());
        }

        // Emit placed event
        self.pending_trigger_events.push(TriggerEvent {
            id: id.clone(),
            trader,
            symbol,
            event_type: TriggerEventType::Placed,
            timestamp: self.timestamp,
        });

        Ok(id)
    }

    /// Cancel a trigger order by ID
    pub(crate) fn execute_cancel_trigger_order(
        &mut self,
        trader: Address,
        trigger_order_id: TriggerOrderId,
    ) -> Result<(), TriggerError> {
        // Get the order
        let order = self
            .trigger_orders
            .get_mut(&trigger_order_id)
            .ok_or(TriggerError::NotFound)?;

        // Verify ownership
        if order.trader != trader {
            return Err(TriggerError::NotFound);
        }

        // Check if already processed
        if order.status != TriggerOrderStatus::Pending {
            return Err(TriggerError::AlreadyProcessed);
        }

        // Mark as cancelled
        order.status = TriggerOrderStatus::Cancelled;

        // Save fields for event before cleanup removes the order
        let order_trader = order.trader.clone();
        let order_symbol = order.symbol.clone();

        // Emit cancelled event
        self.pending_trigger_events.push(TriggerEvent {
            id: trigger_order_id.clone(),
            trader: order_trader,
            symbol: order_symbol,
            event_type: TriggerEventType::Cancelled,
            timestamp: self.timestamp,
        });

        // Clean up indexes
        self.cleanup_trigger_order(&trigger_order_id);

        Ok(())
    }

    /// Cancel a trigger order by client order ID
    pub(crate) fn execute_cancel_trigger_order_by_cloid(
        &mut self,
        trader: Address,
        symbol: Symbol,
        cloid: Cloid,
    ) -> Result<(), TriggerError> {
        // Look up the trigger order ID by cloid
        let key = (trader.clone(), symbol, cloid);
        let trigger_order_id = self
            .trigger_orders_by_cloid
            .get(&key)
            .cloned()
            .ok_or(TriggerError::NotFound)?;

        // Cancel by ID
        self.execute_cancel_trigger_order(trader, trigger_order_id)
    }

    /// Process all pending triggers against current mark prices
    ///
    /// Called after all transactions are executed in a block.
    /// Converts triggered orders to IOC reduce-only orders.
    pub(crate) fn process_triggers(&mut self) -> Vec<Fill> {
        let mut all_fills = Vec::new();

        // Collect symbols to check (avoid borrow issues)
        let mut symbols: Vec<Symbol> = self.mark_prices.keys().cloned().collect();
        // Mark prices are stored in a HashMap.  Canonical symbol order keeps
        // trigger-generated fills/events independent of insertion order.
        symbols.sort();

        for symbol in symbols {
            let mark_price = match self.mark_prices.get(&symbol) {
                Some(&p) if p > 0 => p,
                _ => continue,
            };

            // Get trigger orders for this symbol that should fire
            let mut trigger_ids: Vec<TriggerOrderId> = self
                .trigger_orders_by_symbol
                .get(&symbol)
                .map(|ids| {
                    ids.iter()
                        .filter(|id| {
                            self.trigger_orders
                                .get(*id)
                                .map(|o| o.should_trigger(mark_price))
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            // Snapshot/recovery or equivalent state construction can rebuild
            // this index in a different insertion order. Trigger IDs are
            // stable protocol sequence identifiers, so sort before mutation.
            trigger_ids.sort_by(|left, right| compare_trigger_ids(left, right));

            // Process each triggered order
            for trigger_id in trigger_ids {
                self.mark_full_state_dirty(super::full_state_hash::COMPONENT_DIRTY_TRIGGERS);
                if let Some(fills) = self.execute_trigger(&trigger_id, mark_price) {
                    all_fills.extend(fills);
                }
            }
        }

        all_fills
    }

    /// Execute a single triggered order
    fn execute_trigger(
        &mut self,
        trigger_id: &TriggerOrderId,
        mark_price: Price,
    ) -> Option<Vec<Fill>> {
        // Get order details (clone to avoid borrow issues)
        let order = self.trigger_orders.get(trigger_id)?.clone();

        if order.status != TriggerOrderStatus::Pending {
            return None;
        }

        // Verify position still exists and get current size
        let current_position_size = {
            let account = self.accounts.get(&order.trader)?;
            let position = account.position(&order.symbol);
            position.size
        };

        // Check position is still valid for this trigger
        let position_is_long = current_position_size > 0;
        let trigger_expects_long = order.side == Side::Ask;

        if position_is_long != trigger_expects_long || current_position_size == 0 {
            // Position changed or closed - mark as failed
            if let Some(o) = self.trigger_orders.get_mut(trigger_id) {
                o.status = TriggerOrderStatus::Failed;
            }
            self.pending_trigger_events.push(TriggerEvent {
                id: trigger_id.clone(),
                trader: order.trader.clone(),
                symbol: order.symbol.clone(),
                event_type: TriggerEventType::Failed {
                    reason: "Position no longer exists or direction changed".to_string(),
                },
                timestamp: self.timestamp,
            });
            self.cleanup_trigger_order(trigger_id);
            return None;
        }

        // Clamp size to current position size
        let actual_size = order.size.min(current_position_size.abs());

        // Determine order price
        // For market orders (no limit_price), use sweep price
        let price = order.limit_price.unwrap_or_else(|| {
            if order.side == Side::Ask {
                1 // Minimum price to sweep bids
            } else {
                mark_price * 2 // High price to sweep asks
            }
        });

        // Create an IOC reduce-only order
        let tx = Transaction::PlaceOrder {
            trader: order.trader.clone(),
            symbol: order.symbol.clone(),
            side: order.side,
            price,
            size: actual_size,
            order_type: OrderType::Ioc,
            reduce_only: true,
        };

        // Execute the transaction
        let fills = match self.execute_tx(tx) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(trigger_id = %trigger_id, error = %e, "Trigger order execution failed");
                if let Some(o) = self.trigger_orders.get_mut(trigger_id) {
                    o.status = TriggerOrderStatus::Failed;
                }
                self.pending_trigger_events.push(TriggerEvent {
                    id: trigger_id.clone(),
                    trader: order.trader.clone(),
                    symbol: order.symbol.clone(),
                    event_type: TriggerEventType::Failed {
                        reason: e.to_string(),
                    },
                    timestamp: self.timestamp,
                });
                self.cleanup_trigger_order(trigger_id);
                return None;
            }
        };

        // Mark as triggered
        if let Some(o) = self.trigger_orders.get_mut(trigger_id) {
            o.status = TriggerOrderStatus::Triggered;
        }

        // Get the order ID from the fills (if any)
        let order_id = fills
            .first()
            .map(|f| f.maker_order_id.clone())
            .unwrap_or_default();

        // Emit triggered event
        self.pending_trigger_events.push(TriggerEvent {
            id: trigger_id.clone(),
            trader: order.trader,
            symbol: order.symbol,
            event_type: TriggerEventType::Triggered { order_id },
            timestamp: self.timestamp,
        });

        // Clean up indexes
        self.cleanup_trigger_order(trigger_id);

        Some(fills)
    }
}

#[cfg(test)]
#[path = "trigger_tests.rs"]
mod tests;
