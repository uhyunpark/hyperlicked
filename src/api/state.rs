//! Shared State
//!
//! Thread-safe state shared between API handlers and consensus.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::app::AppState;

/// Events broadcast to WebSocket clients
/// Format matches frontend expectations (flat structure with lowercase type)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Event {
    /// Orderbook update
    #[serde(rename = "orderbook")]
    OrderbookUpdate {
        symbol: String,
        bids: Vec<PriceLevel>,
        asks: Vec<PriceLevel>,
        timestamp: u64,
    },
    /// Trade executed
    Trade {
        /// Deterministic trade ID for deduplication
        id: String,
        symbol: String,
        price: i64,
        size: i64,
        side: String,
        timestamp: u64,
    },
    /// Block committed
    #[serde(rename = "block")]
    BlockCommitted {
        height: u64,
        hash: String,
        tx_count: usize,
    },
    /// Mark price update (broadcast every block with trades)
    #[serde(rename = "markPrice")]
    MarkPriceUpdate {
        symbol: String,
        mark_price: i64,
        index_price: Option<i64>,
        timestamp: u64,
    },
    /// Asset context update (market stats, streamed every block)
    #[serde(rename = "assetCtx")]
    AssetCtx {
        symbol: String,
        #[serde(rename = "markPrice")]
        mark_price: i64,
        #[serde(rename = "oraclePrice")]
        oracle_price: Option<i64>,
        #[serde(rename = "midPrice")]
        mid_price: i64,
        #[serde(rename = "fundingRate")]
        funding_rate: i64, // 1/1M units
        premium: i64,      // 1/1M units
        #[serde(rename = "openInterest")]
        open_interest: i64, // satoshis
        #[serde(rename = "prevDayPrice")]
        prev_day_price: i64, // cents
        #[serde(rename = "dayVolume")]
        day_volume: i64, // satoshis
        #[serde(rename = "dayNotionalVolume")]
        day_notional_volume: i64, // cents
        #[serde(rename = "nextFundingTime")]
        next_funding_time: u64,
        timestamp: u64,
    },
}

/// Price level for API responses
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriceLevel {
    pub price: i64,
    pub size: i64,
}

// =============================================================================
// User-specific Events (sent only to subscribed users)
// =============================================================================

/// Events sent to specific users (not broadcast to all)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserEvent {
    /// User's order was filled (partially or fully)
    #[serde(rename = "userFill")]
    UserFill {
        symbol: String,
        order_id: String,
        side: String,
        price: i64,
        size: i64,
        fee: i64,
        is_maker: bool,
        timestamp: u64,
    },
    /// User's order status changed
    #[serde(rename = "orderUpdate")]
    OrderUpdate {
        order_id: String,
        symbol: String,
        status: String,        // "open", "partial", "filled", "cancelled"
        filled: i64,           // Total filled so far
        remaining: i64,        // Remaining size
        timestamp: u64,
    },
    /// User's position changed
    #[serde(rename = "positionUpdate")]
    PositionUpdate {
        symbol: String,
        size: i64,             // New position size (negative = short)
        entry_price: i64,
        mark_price: i64,
        unrealized_pnl: i64,
        timestamp: u64,
    },
    /// User's account balance changed
    #[serde(rename = "balanceUpdate")]
    BalanceUpdate {
        balance: i64,          // New total balance (cents)
        available: i64,        // Available balance
        locked: i64,           // Locked in positions
        timestamp: u64,
    },
    /// Trigger order placed
    #[serde(rename = "triggerOrderPlaced")]
    TriggerOrderPlaced {
        id: String,
        symbol: String,
        trigger_type: String,  // "sl" or "tp"
        trigger_price: i64,
        size: i64,
        timestamp: u64,
    },
    /// Trigger order activated (converted to regular order)
    #[serde(rename = "triggerOrderTriggered")]
    TriggerOrderTriggered {
        id: String,
        symbol: String,
        order_id: String,      // Resulting order ID
        timestamp: u64,
    },
    /// Trigger order cancelled
    #[serde(rename = "triggerOrderCancelled")]
    TriggerOrderCancelled {
        id: String,
        symbol: String,
        timestamp: u64,
    },
    /// Position was auto-deleveraged
    #[serde(rename = "adl")]
    ADL {
        symbol: String,
        size_reduced: i64,
        close_price: i64,
        realized_pnl: i64,
        triggering_liquidation: String,
        timestamp: u64,
    },
    /// Funding payment received/paid
    #[serde(rename = "fundingPayment")]
    FundingPayment {
        symbol: String,
        payment: i64,          // cents: positive = received, negative = paid
        position_size: i64,
        funding_rate_bps: i64,
        timestamp: u64,
    },
    /// Position was liquidated
    #[serde(rename = "liquidated")]
    Liquidated {
        symbol: String,
        size: i64,
        price: i64,
        pnl: i64,
        was_long: bool,
        timestamp: u64,
    },
}

/// User subscription info
pub struct UserSubscription {
    pub sender: mpsc::UnboundedSender<UserEvent>,
}

/// Registry of user subscriptions by address
pub struct UserRegistry {
    subscriptions: RwLock<HashMap<String, Vec<mpsc::UnboundedSender<UserEvent>>>>,
}

impl UserRegistry {
    pub fn new() -> Self {
        Self {
            subscriptions: RwLock::new(HashMap::new()),
        }
    }

    /// Subscribe a user connection
    pub async fn subscribe(&self, address: &str) -> mpsc::UnboundedReceiver<UserEvent> {
        let (tx, rx) = mpsc::unbounded_channel();
        let address_lower = address.to_lowercase();

        let mut subs = self.subscriptions.write().await;
        subs.entry(address_lower)
            .or_insert_with(Vec::new)
            .push(tx);

        rx
    }

    /// Unsubscribe a user connection (called when WebSocket closes)
    pub async fn unsubscribe(&self, address: &str, sender: &mpsc::UnboundedSender<UserEvent>) {
        let address_lower = address.to_lowercase();
        let mut subs = self.subscriptions.write().await;

        if let Some(senders) = subs.get_mut(&address_lower) {
            senders.retain(|s| !s.same_channel(sender));
            if senders.is_empty() {
                subs.remove(&address_lower);
            }
        }
    }

    /// Send event to a specific user (all their connections)
    pub async fn send_to_user(&self, address: &str, event: UserEvent) {
        let address_lower = address.to_lowercase();
        let subs = self.subscriptions.read().await;

        if let Some(senders) = subs.get(&address_lower) {
            for sender in senders {
                let _ = sender.send(event.clone());
            }
        }
    }

    /// Send fill events to both maker and taker
    pub async fn notify_fill(
        &self,
        maker: &str,
        taker: &str,
        symbol: &str,
        order_id: &str,
        side: &str,
        price: i64,
        size: i64,
        maker_fee: i64,
        taker_fee: i64,
        timestamp: u64,
    ) {
        // Notify maker
        self.send_to_user(maker, UserEvent::UserFill {
            symbol: symbol.to_string(),
            order_id: order_id.to_string(),
            side: side.to_string(),
            price,
            size,
            fee: maker_fee,
            is_maker: true,
            timestamp,
        }).await;

        // Notify taker
        self.send_to_user(taker, UserEvent::UserFill {
            symbol: symbol.to_string(),
            order_id: format!("{}-taker", order_id),
            side: if side == "buy" { "sell" } else { "buy" }.to_string(),
            price,
            size,
            fee: taker_fee,
            is_maker: false,
            timestamp,
        }).await;
    }

    /// Send order update to a user
    pub async fn notify_order_update(
        &self,
        address: &str,
        order_id: &str,
        symbol: &str,
        status: &str,
        filled: i64,
        remaining: i64,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::OrderUpdate {
            order_id: order_id.to_string(),
            symbol: symbol.to_string(),
            status: status.to_string(),
            filled,
            remaining,
            timestamp,
        }).await;
    }

    /// Send position update to a user
    pub async fn notify_position_update(
        &self,
        address: &str,
        symbol: &str,
        size: i64,
        entry_price: i64,
        mark_price: i64,
        unrealized_pnl: i64,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::PositionUpdate {
            symbol: symbol.to_string(),
            size,
            entry_price,
            mark_price,
            unrealized_pnl,
            timestamp,
        }).await;
    }

    /// Send balance update to a user
    pub async fn notify_balance_update(
        &self,
        address: &str,
        balance: i64,
        available: i64,
        locked: i64,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::BalanceUpdate {
            balance,
            available,
            locked,
            timestamp,
        }).await;
    }

    /// Send ADL event to a user
    pub async fn notify_adl(
        &self,
        address: &str,
        symbol: &str,
        size_reduced: i64,
        close_price: i64,
        realized_pnl: i64,
        triggering_liquidation: &str,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::ADL {
            symbol: symbol.to_string(),
            size_reduced,
            close_price,
            realized_pnl,
            triggering_liquidation: triggering_liquidation.to_string(),
            timestamp,
        }).await;
    }

    /// Send funding payment event to a user
    pub async fn notify_funding_payment(
        &self,
        address: &str,
        symbol: &str,
        payment: i64,
        position_size: i64,
        funding_rate_bps: i64,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::FundingPayment {
            symbol: symbol.to_string(),
            payment,
            position_size,
            funding_rate_bps,
            timestamp,
        }).await;
    }

    /// Send liquidated event to a user
    pub async fn notify_liquidated(
        &self,
        address: &str,
        symbol: &str,
        size: i64,
        price: i64,
        pnl: i64,
        was_long: bool,
        timestamp: u64,
    ) {
        self.send_to_user(address, UserEvent::Liquidated {
            symbol: symbol.to_string(),
            size,
            price,
            pnl,
            was_long,
            timestamp,
        }).await;
    }
}

/// Shared state accessible by API handlers
#[derive(Clone)]
pub struct SharedState {
    /// Application state (orderbooks, accounts, mempool)
    pub app: Arc<RwLock<AppState>>,
    /// Event broadcaster for WebSocket clients (public events)
    pub events: broadcast::Sender<Event>,
    /// User-specific event registry
    pub users: Arc<UserRegistry>,
}

impl SharedState {
    /// Create new shared state
    pub fn new(app: AppState) -> Self {
        let (events, _) = broadcast::channel(1000);
        Self {
            app: Arc::new(RwLock::new(app)),
            events,
            users: Arc::new(UserRegistry::new()),
        }
    }

    /// Broadcast an event to all WebSocket clients
    pub fn broadcast(&self, event: Event) {
        // Ignore send errors (no receivers)
        let _ = self.events.send(event);
    }

    /// Subscribe to public events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Subscribe to user-specific events
    pub async fn subscribe_user(&self, address: &str) -> mpsc::UnboundedReceiver<UserEvent> {
        self.users.subscribe(address).await
    }
}
