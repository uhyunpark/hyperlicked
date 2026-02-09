//! Trigger Order Execution
//!
//! Handles placement, cancellation, and processing of trigger orders.

use crate::app::{
    orderbook::{Fill, Side},
    trigger::{
        determine_trigger_condition, determine_trigger_side, validate_trigger_price,
        Cloid, TriggerError, TriggerEvent, TriggerEventType, TriggerOrder, TriggerOrderId,
        TriggerOrderStatus, TriggerType,
    },
    Address, OrderType, Symbol, Transaction,
};
use crate::types::Price;

use super::AppState;

impl AppState {
    /// Generate next trigger order ID
    fn next_trigger_id(&mut self) -> TriggerOrderId {
        self.trigger_seq += 1;
        format!("T{}", self.trigger_seq)
    }

    /// Remove a trigger order from all indexes
    fn cleanup_trigger_order(&mut self, id: &str) -> Option<TriggerOrder> {
        let order = self.trigger_orders.remove(id)?;

        if let Some(ids) = self.trigger_orders_by_trader.get_mut(&order.trader) {
            ids.retain(|i| i != id);
        }
        if let Some(ids) = self.trigger_orders_by_symbol.get_mut(&order.symbol) {
            ids.retain(|i| i != id);
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
        let symbols: Vec<Symbol> = self.mark_prices.keys().cloned().collect();

        for symbol in symbols {
            let mark_price = match self.mark_prices.get(&symbol) {
                Some(&p) if p > 0 => p,
                _ => continue,
            };

            // Get trigger orders for this symbol that should fire
            let trigger_ids: Vec<TriggerOrderId> = self
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

            // Process each triggered order
            for trigger_id in trigger_ids {
                if let Some(fills) = self.execute_trigger(&trigger_id, mark_price) {
                    all_fills.extend(fills);
                }
            }
        }

        all_fills
    }

    /// Execute a single triggered order
    fn execute_trigger(&mut self, trigger_id: &TriggerOrderId, mark_price: Price) -> Option<Vec<Fill>> {
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
        let order_id = fills.first().map(|f| f.maker_order_id.clone()).unwrap_or_default();

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
