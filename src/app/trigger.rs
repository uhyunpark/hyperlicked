//! Trigger Orders (Stop Loss / Take Profit)
//!
//! Trigger orders are conditional orders that execute when price conditions are met.
//! They are stored separately and converted to regular orders upon triggering.

use serde::{Deserialize, Serialize};

use crate::app::{Address, Side, Symbol};
use crate::types::Price;

/// Unique identifier for a trigger order
pub type TriggerOrderId = String;

/// Client-provided order ID for idempotency
pub type Cloid = String;

/// Type of trigger order
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    /// Stop Loss - triggers when price moves against position
    StopLoss,
    /// Take Profit - triggers when price moves in favor of position
    TakeProfit,
}

/// Condition for when the trigger activates
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerCondition {
    /// Trigger when mark price >= trigger price
    PriceAbove,
    /// Trigger when mark price <= trigger price
    PriceBelow,
}

/// Status of a trigger order
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerOrderStatus {
    /// Waiting for trigger condition
    Pending,
    /// Trigger condition met, order placed
    Triggered,
    /// Manually cancelled
    Cancelled,
    /// Failed to execute (e.g., position no longer exists)
    Failed,
}

/// A trigger order waiting to be activated
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerOrder {
    /// Unique identifier
    pub id: TriggerOrderId,
    /// Client-provided ID for idempotency (optional)
    pub cloid: Option<Cloid>,
    /// Trader address
    pub trader: Address,
    /// Trading symbol
    pub symbol: Symbol,
    /// Order side when triggered (Ask for closing longs, Bid for closing shorts)
    pub side: Side,
    /// Order size (satoshis)
    pub size: i64,
    /// Type of trigger (SL or TP)
    pub trigger_type: TriggerType,
    /// Condition for activation
    pub condition: TriggerCondition,
    /// Price at which to trigger
    pub trigger_price: Price,
    /// Limit price for the resulting order (None = market order)
    pub limit_price: Option<Price>,
    /// Always true for SL/TP - only reduces position
    pub reduce_only: bool,
    /// Creation timestamp
    pub timestamp: u64,
    /// Current status
    pub status: TriggerOrderStatus,
}

impl TriggerOrder {
    /// Check if this trigger should activate given the current mark price
    pub fn should_trigger(&self, mark_price: Price) -> bool {
        if self.status != TriggerOrderStatus::Pending {
            return false;
        }

        match self.condition {
            TriggerCondition::PriceAbove => mark_price >= self.trigger_price,
            TriggerCondition::PriceBelow => mark_price <= self.trigger_price,
        }
    }
}

/// Event emitted when a trigger order state changes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerEvent {
    /// Order ID
    pub id: TriggerOrderId,
    /// Trader address
    pub trader: Address,
    /// Symbol
    pub symbol: Symbol,
    /// Event type
    pub event_type: TriggerEventType,
    /// Timestamp
    pub timestamp: u64,
}

/// Type of trigger event
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerEventType {
    /// Trigger order was placed
    Placed,
    /// Trigger order was triggered and converted to regular order
    Triggered { order_id: String },
    /// Trigger order was cancelled
    Cancelled,
    /// Trigger order failed (reason)
    Failed { reason: String },
}

/// Errors that can occur with trigger orders
#[derive(Debug, Clone, thiserror::Error)]
pub enum TriggerError {
    #[error("trigger order not found")]
    NotFound,
    #[error("no position exists for this symbol")]
    NoPosition,
    #[error("invalid trigger price for this position")]
    InvalidTriggerPrice,
    #[error("size exceeds current position size")]
    SizeExceedsPosition,
    #[error("duplicate client order ID")]
    DuplicateCloid,
    #[error("trigger order already cancelled or triggered")]
    AlreadyProcessed,
    #[error("market not found")]
    MarketNotFound,
}

/// Determine the correct trigger condition based on position and trigger type
pub fn determine_trigger_condition(
    position_size: i64,
    trigger_type: TriggerType,
) -> TriggerCondition {
    let is_long = position_size > 0;

    match (is_long, trigger_type) {
        // Long position + Stop Loss = trigger when price drops to/below trigger
        (true, TriggerType::StopLoss) => TriggerCondition::PriceBelow,
        // Long position + Take Profit = trigger when price rises to/above trigger
        (true, TriggerType::TakeProfit) => TriggerCondition::PriceAbove,
        // Short position + Stop Loss = trigger when price rises to/above trigger
        (false, TriggerType::StopLoss) => TriggerCondition::PriceAbove,
        // Short position + Take Profit = trigger when price drops to/below trigger
        (false, TriggerType::TakeProfit) => TriggerCondition::PriceBelow,
    }
}

/// Determine the order side when trigger activates
pub fn determine_trigger_side(position_size: i64) -> Side {
    if position_size > 0 {
        // Long position -> sell to close
        Side::Ask
    } else {
        // Short position -> buy to close
        Side::Bid
    }
}

/// Validate trigger price relative to mark price and position
pub fn validate_trigger_price(
    position_size: i64,
    trigger_type: TriggerType,
    trigger_price: Price,
    mark_price: Price,
) -> Result<(), TriggerError> {
    let is_long = position_size > 0;

    match (is_long, trigger_type) {
        // Long + SL: trigger price must be below current mark (losing direction)
        (true, TriggerType::StopLoss) => {
            if trigger_price >= mark_price {
                return Err(TriggerError::InvalidTriggerPrice);
            }
        }
        // Long + TP: trigger price must be above current mark (winning direction)
        (true, TriggerType::TakeProfit) => {
            if trigger_price <= mark_price {
                return Err(TriggerError::InvalidTriggerPrice);
            }
        }
        // Short + SL: trigger price must be above current mark (losing direction)
        (false, TriggerType::StopLoss) => {
            if trigger_price <= mark_price {
                return Err(TriggerError::InvalidTriggerPrice);
            }
        }
        // Short + TP: trigger price must be below current mark (winning direction)
        (false, TriggerType::TakeProfit) => {
            if trigger_price >= mark_price {
                return Err(TriggerError::InvalidTriggerPrice);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trigger_condition_long_sl() {
        let condition = determine_trigger_condition(100, TriggerType::StopLoss);
        assert_eq!(condition, TriggerCondition::PriceBelow);
    }

    #[test]
    fn test_trigger_condition_long_tp() {
        let condition = determine_trigger_condition(100, TriggerType::TakeProfit);
        assert_eq!(condition, TriggerCondition::PriceAbove);
    }

    #[test]
    fn test_trigger_condition_short_sl() {
        let condition = determine_trigger_condition(-100, TriggerType::StopLoss);
        assert_eq!(condition, TriggerCondition::PriceAbove);
    }

    #[test]
    fn test_trigger_condition_short_tp() {
        let condition = determine_trigger_condition(-100, TriggerType::TakeProfit);
        assert_eq!(condition, TriggerCondition::PriceBelow);
    }

    #[test]
    fn test_validate_long_sl_valid() {
        // Long position, SL below mark price - valid
        assert!(validate_trigger_price(100, TriggerType::StopLoss, 4500, 5000).is_ok());
    }

    #[test]
    fn test_validate_long_sl_invalid() {
        // Long position, SL above mark price - invalid
        assert!(validate_trigger_price(100, TriggerType::StopLoss, 5500, 5000).is_err());
    }

    #[test]
    fn test_validate_long_tp_valid() {
        // Long position, TP above mark price - valid
        assert!(validate_trigger_price(100, TriggerType::TakeProfit, 5500, 5000).is_ok());
    }

    #[test]
    fn test_validate_short_sl_valid() {
        // Short position, SL above mark price - valid
        assert!(validate_trigger_price(-100, TriggerType::StopLoss, 5500, 5000).is_ok());
    }

    #[test]
    fn test_validate_short_tp_valid() {
        // Short position, TP below mark price - valid
        assert!(validate_trigger_price(-100, TriggerType::TakeProfit, 4500, 5000).is_ok());
    }

    #[test]
    fn test_should_trigger() {
        let order = TriggerOrder {
            id: "t1".to_string(),
            cloid: None,
            trader: "alice".to_string(),
            symbol: "BTC-USDT".to_string(),
            side: Side::Ask,
            size: 100,
            trigger_type: TriggerType::StopLoss,
            condition: TriggerCondition::PriceBelow,
            trigger_price: 4500,
            limit_price: None,
            reduce_only: true,
            timestamp: 0,
            status: TriggerOrderStatus::Pending,
        };

        // Price above trigger - should not trigger
        assert!(!order.should_trigger(5000));

        // Price at trigger - should trigger
        assert!(order.should_trigger(4500));

        // Price below trigger - should trigger
        assert!(order.should_trigger(4000));
    }
}
