//! Shared State
//!
//! Thread-safe state shared between API handlers and consensus.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::app::{AppState, SignedEnvelope};
use crate::network::UserTransactionPublisher;

/// Interval between bounded retries for authenticated user transactions that
/// are still waiting in the canonical mempool.
pub const USER_TRANSACTION_REBROADCAST_INTERVAL: Duration = Duration::from_secs(2);
/// Maximum number of pending envelopes retried by one periodic tick.
pub const MAX_USER_TRANSACTION_REBROADCAST_BATCH: usize = 32;
/// Maximum aggregate canonical envelope bytes retried by one periodic tick.
pub const MAX_USER_TRANSACTION_REBROADCAST_BYTES: usize = 2 * 1024 * 1024;

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
        premium: i64, // 1/1M units
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

/// Exact canonical event bytes attached to a finalized transaction receipt.
/// Semantic clients may decode known `event_type` values; unknown future
/// types remain losslessly available through `payload_hex`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FinalizedReceiptEvent {
    pub event_index: u32,
    pub event_type: u16,
    pub payload_hex: String,
}

// =============================================================================
// User-specific Events (sent only to subscribed users)
// =============================================================================

/// Events sent to specific users (not broadcast to all)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserEvent {
    /// A signed transaction crossed the durable finality boundary. This is
    /// the generic, lossless notification; it never fabricates fee, cloid, or
    /// balance fields that are absent from Commitment v2.
    #[serde(rename = "transactionFinalized")]
    TransactionFinalized {
        tx_hash: String,
        block_height: u64,
        block_hash: String,
        tx_index: u32,
        tx_type: u16,
        status: u8,
        error_code: u16,
        compute_units: u64,
        storage_read_bytes: u64,
        storage_write_bytes: u64,
        events: Vec<FinalizedReceiptEvent>,
    },
    /// User's order was filled (partially or fully)
    #[serde(rename = "userFill")]
    UserFill {
        symbol: String,
        order_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cloid: Option<String>, // Client order ID for correlation
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
        status: String, // "open", "partial", "filled", "cancelled"
        filled: i64,    // Total filled so far
        remaining: i64, // Remaining size
        timestamp: u64,
    },
    /// Order fully filled or cancelled (for order history streaming)
    #[serde(rename = "orderClosed")]
    OrderClosed {
        order_id: String,
        symbol: String,
        side: String,
        price: i64,
        size: i64,      // Original size
        filled: i64,    // Total filled
        status: String, // "filled" or "cancelled"
        timestamp: u64,
    },
    /// User's position changed
    #[serde(rename = "positionUpdate")]
    PositionUpdate {
        symbol: String,
        size: i64, // New position size (negative = short)
        entry_price: i64,
        mark_price: i64,
        unrealized_pnl: i64,
        liquidation_price: i64,
        margin: i64,
        leverage: i64, // Integer leverage (1-100x)
        timestamp: u64,
    },
    /// User's account balance changed
    #[serde(rename = "balanceUpdate")]
    BalanceUpdate {
        balance: i64,   // New total balance (cents)
        available: i64, // Available balance
        locked: i64,    // Locked in positions
        timestamp: u64,
    },
    /// Trigger order placed
    #[serde(rename = "triggerOrderPlaced")]
    TriggerOrderPlaced {
        id: String,
        symbol: String,
        trigger_type: String, // "sl" or "tp"
        trigger_price: i64,
        size: i64,
        timestamp: u64,
    },
    /// Trigger order activated (converted to regular order)
    #[serde(rename = "triggerOrderTriggered")]
    TriggerOrderTriggered {
        id: String,
        symbol: String,
        order_id: String, // Resulting order ID
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
        payment: i64, // cents: positive = received, negative = paid
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
        subs.entry(address_lower).or_insert_with(Vec::new).push(tx);

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
        maker_order_id: &str,
        taker_order_id: &str,
        maker_cloid: Option<&str>,
        taker_cloid: Option<&str>,
        side: &str,
        price: i64,
        size: i64,
        maker_fee: i64,
        taker_fee: i64,
        timestamp: u64,
    ) {
        // Notify maker
        self.send_to_user(
            maker,
            UserEvent::UserFill {
                symbol: symbol.to_string(),
                order_id: maker_order_id.to_string(),
                cloid: maker_cloid.map(|s| s.to_string()),
                side: side.to_string(),
                price,
                size,
                fee: maker_fee,
                is_maker: true,
                timestamp,
            },
        )
        .await;

        // Notify taker
        self.send_to_user(
            taker,
            UserEvent::UserFill {
                symbol: symbol.to_string(),
                order_id: taker_order_id.to_string(),
                cloid: taker_cloid.map(|s| s.to_string()),
                side: if side == "buy" { "sell" } else { "buy" }.to_string(),
                price,
                size,
                fee: taker_fee,
                is_maker: false,
                timestamp,
            },
        )
        .await;
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
        self.send_to_user(
            address,
            UserEvent::OrderUpdate {
                order_id: order_id.to_string(),
                symbol: symbol.to_string(),
                status: status.to_string(),
                filled,
                remaining,
                timestamp,
            },
        )
        .await;
    }

    /// Send order closed event (for order history streaming)
    pub async fn notify_order_closed(
        &self,
        address: &str,
        order_id: &str,
        symbol: &str,
        side: &str,
        price: i64,
        size: i64,
        filled: i64,
        status: &str, // "filled" or "cancelled"
        timestamp: u64,
    ) {
        self.send_to_user(
            address,
            UserEvent::OrderClosed {
                order_id: order_id.to_string(),
                symbol: symbol.to_string(),
                side: side.to_string(),
                price,
                size,
                filled,
                status: status.to_string(),
                timestamp,
            },
        )
        .await;
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
        liquidation_price: i64,
        margin: i64,
        leverage: i64,
        timestamp: u64,
    ) {
        self.send_to_user(
            address,
            UserEvent::PositionUpdate {
                symbol: symbol.to_string(),
                size,
                entry_price,
                mark_price,
                unrealized_pnl,
                liquidation_price,
                margin,
                leverage,
                timestamp,
            },
        )
        .await;
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
        self.send_to_user(
            address,
            UserEvent::BalanceUpdate {
                balance,
                available,
                locked,
                timestamp,
            },
        )
        .await;
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
        self.send_to_user(
            address,
            UserEvent::ADL {
                symbol: symbol.to_string(),
                size_reduced,
                close_price,
                realized_pnl,
                triggering_liquidation: triggering_liquidation.to_string(),
                timestamp,
            },
        )
        .await;
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
        self.send_to_user(
            address,
            UserEvent::FundingPayment {
                symbol: symbol.to_string(),
                payment,
                position_size,
                funding_rate_bps,
                timestamp,
            },
        )
        .await;
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
        self.send_to_user(
            address,
            UserEvent::Liquidated {
                symbol: symbol.to_string(),
                size,
                price,
                pnl,
                was_long,
                timestamp,
            },
        )
        .await;
    }

    /// Send trigger order placed event
    pub async fn notify_trigger_placed(
        &self,
        address: &str,
        id: &str,
        symbol: &str,
        trigger_type: &str,
        trigger_price: i64,
        size: i64,
        timestamp: u64,
    ) {
        self.send_to_user(
            address,
            UserEvent::TriggerOrderPlaced {
                id: id.to_string(),
                symbol: symbol.to_string(),
                trigger_type: trigger_type.to_string(),
                trigger_price,
                size,
                timestamp,
            },
        )
        .await;
    }

    /// Send trigger order triggered event
    pub async fn notify_trigger_triggered(
        &self,
        address: &str,
        id: &str,
        symbol: &str,
        order_id: &str,
        timestamp: u64,
    ) {
        self.send_to_user(
            address,
            UserEvent::TriggerOrderTriggered {
                id: id.to_string(),
                symbol: symbol.to_string(),
                order_id: order_id.to_string(),
                timestamp,
            },
        )
        .await;
    }

    /// Send trigger order cancelled event
    pub async fn notify_trigger_cancelled(
        &self,
        address: &str,
        id: &str,
        symbol: &str,
        timestamp: u64,
    ) {
        self.send_to_user(
            address,
            UserEvent::TriggerOrderCancelled {
                id: id.to_string(),
                symbol: symbol.to_string(),
                timestamp,
            },
        )
        .await;
    }
}

/// Shared state accessible by API handlers
#[derive(Clone)]
pub struct SharedState {
    /// Application state (orderbooks, accounts, mempool)
    pub app: Arc<StdRwLock<AppState>>,
    /// Event broadcaster for WebSocket clients (public events)
    pub events: broadcast::Sender<Event>,
    /// User-specific event registry
    pub users: Arc<UserRegistry>,
    /// Best-effort post-finality user notifications. The address travels
    /// beside the event so the WebSocket boundary can filter it without an
    /// async lock inside the synchronous consensus commit callback.
    committed_user_events: broadcast::Sender<(String, UserEvent)>,
    /// State corruption flag (set when app_hash mismatch detected)
    ///
    /// When true, the node detected an app_hash mismatch after a valid QC,
    /// indicating either:
    /// 1. This node's state is corrupt and needs resync
    /// 2. The validator network is Byzantine (2f+1 colluding)
    ///
    /// The node should NOT process further blocks until operator intervention.
    pub state_corrupted: Arc<AtomicBool>,
    /// Outbound-only transport capability for canonical signed user
    /// transaction propagation.  The consensus receive loop is never shared
    /// with API handlers.
    transaction_publisher: Arc<StdRwLock<Option<Arc<dyn UserTransactionPublisher>>>>,
}

impl SharedState {
    /// Create new shared state
    pub fn new(app: AppState) -> Self {
        let (events, _) = broadcast::channel(1000);
        let (committed_user_events, _) = broadcast::channel(1000);
        Self {
            app: Arc::new(StdRwLock::new(app)),
            events,
            users: Arc::new(UserRegistry::new()),
            committed_user_events,
            state_corrupted: Arc::new(AtomicBool::new(false)),
            transaction_publisher: Arc::new(StdRwLock::new(None)),
        }
    }

    /// Attach the live node's outbound transaction publisher.
    pub fn set_user_transaction_publisher(&self, publisher: Arc<dyn UserTransactionPublisher>) {
        if let Ok(mut slot) = self.transaction_publisher.write() {
            *slot = Some(publisher);
        } else {
            self.set_state_corrupted();
        }
    }

    /// Publish an already admitted canonical envelope without holding a state
    /// lock across network I/O.  Test and single-node callers may leave the
    /// publisher unset; local admission still behaves as before.
    pub async fn publish_user_transaction(&self, envelope: SignedEnvelope) -> Result<(), String> {
        let publisher = self
            .transaction_publisher
            .read()
            .map_err(|_| "transaction publisher lock poisoned".to_string())?
            .clone();
        if let Some(publisher) = publisher {
            publisher
                .publish_user_transaction(envelope)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Retry an already admitted envelope using the publisher's direct
    /// all-peer path. No application lock is held while network I/O awaits.
    pub async fn rebroadcast_user_transaction(
        &self,
        envelope: SignedEnvelope,
    ) -> Result<(), String> {
        let publisher = self
            .transaction_publisher
            .read()
            .map_err(|_| "transaction publisher lock poisoned".to_string())?
            .clone();
        if let Some(publisher) = publisher {
            publisher
                .rebroadcast_user_transaction(envelope)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Select a bounded batch of currently eligible authenticated
    /// transactions from the canonical mempool. This read is intentionally
    /// separate from network publication so no application lock is held while
    /// a peer send awaits.
    pub fn pending_user_envelopes_batch_at(
        &self,
        timestamp: u64,
        cursor: usize,
        max_count: usize,
        max_encoded_bytes: usize,
    ) -> (Vec<SignedEnvelope>, usize) {
        match self.app.read() {
            Ok(app) => app.mempool.pending_user_envelopes_batch_at(
                timestamp,
                cursor,
                max_count,
                max_encoded_bytes,
            ),
            Err(_) => {
                self.set_state_corrupted();
                (Vec::new(), 0)
            }
        }
    }

    /// Check if state is corrupted (Byzantine detection triggered)
    ///
    /// When true, the node detected an app_hash mismatch after a valid QC.
    /// The node should NOT process further blocks until operator intervention.
    pub fn is_state_corrupted(&self) -> bool {
        self.state_corrupted.load(Ordering::Relaxed)
    }

    /// Mark state as corrupted
    ///
    /// Called when app_hash mismatch is detected after a valid QC.
    pub fn set_state_corrupted(&self) {
        self.state_corrupted.store(true, Ordering::Relaxed);
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

    /// Publish a finalized user event without blocking the consensus commit
    /// path. No receivers is a normal condition.
    pub fn broadcast_committed_user_event(&self, address: &str, event: UserEvent) {
        let _ = self
            .committed_user_events
            .send((address.to_ascii_lowercase(), event));
    }

    /// Subscribe to post-finality user events. Consumers must filter by the
    /// authenticated address carried beside each event.
    pub fn subscribe_committed_user_events(&self) -> broadcast::Receiver<(String, UserEvent)> {
        self.committed_user_events.subscribe()
    }

    /// Subscribe to user-specific events
    pub async fn subscribe_user(&self, address: &str) -> mpsc::UnboundedReceiver<UserEvent> {
        self.users.subscribe(address).await
    }
}

/// Retry propagation for authenticated transactions that have not crossed a
/// committed block boundary yet.
///
/// This task deliberately has no durable queue of its own.  The canonical
/// mempool is the source of truth, so committing or evicting an entry removes
/// it from the next snapshot automatically.  A bounded rotating cursor keeps
/// a large pending pool from starving entries near the end of the queue.
pub async fn run_user_transaction_rebroadcast(shared: SharedState) {
    let mut ticker = tokio::time::interval(USER_TRANSACTION_REBROADCAST_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The API performs the initial publication.  The worker starts with a
    // retry after the first full interval rather than sending a duplicate
    // immediately during node startup.
    ticker.tick().await;

    let mut cursor = 0usize;
    loop {
        ticker.tick().await;
        let timestamp = current_timestamp_ms();
        let (batch, next_cursor) = shared.pending_user_envelopes_batch_at(
            timestamp,
            cursor,
            MAX_USER_TRANSACTION_REBROADCAST_BATCH,
            MAX_USER_TRANSACTION_REBROADCAST_BYTES,
        );
        cursor = next_cursor;

        for envelope in batch {
            if let Err(error) = shared.rebroadcast_user_transaction(envelope).await {
                tracing::debug!(error = %error, "user transaction rebroadcast failed; retaining mempool entry");
            }
        }
    }
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

#[cfg(test)]
mod rebroadcast_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::app::{SignatureScheme, Transaction, ENVELOPE_VERSION};
    use crate::network::UserTransactionPublisher;

    #[derive(Clone)]
    struct RecordingPublisher {
        attempts: Arc<AtomicUsize>,
        fail_first: Arc<AtomicBool>,
        delivered: Arc<Mutex<Vec<SignedEnvelope>>>,
    }

    #[async_trait]
    impl UserTransactionPublisher for RecordingPublisher {
        async fn publish_user_transaction(&self, envelope: SignedEnvelope) -> anyhow::Result<()> {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.fail_first.swap(false, Ordering::SeqCst) {
                return Err(anyhow::anyhow!("peer disconnected"));
            }
            self.delivered.lock().unwrap().push(envelope);
            Ok(())
        }

        async fn rebroadcast_user_transaction(
            &self,
            envelope: SignedEnvelope,
        ) -> anyhow::Result<()> {
            self.publish_user_transaction(envelope).await
        }
    }

    fn envelope(nonce: u64, valid_after: u64, valid_until: u64) -> SignedEnvelope {
        SignedEnvelope {
            version: ENVELOPE_VERSION,
            chain_domain: [7; 32],
            signer: [nonce as u8; 20],
            nonce,
            valid_after,
            valid_until,
            action: Transaction::Deposit {
                trader: format!("0x{}", hex::encode([nonce as u8; 20])),
                amount: 1,
            },
            signature_scheme: SignatureScheme::Dev,
            signature: b"dev".to_vec(),
        }
    }

    #[test]
    fn batch_selection_rotates_and_is_bounded() {
        let mut mempool = crate::app::Mempool::with_config(100, 100, 0);
        let envelopes: Vec<_> = (0..5).map(|nonce| envelope(nonce, 0, 100)).collect();
        for entry in envelopes {
            mempool.add_envelope(entry, 0).unwrap();
        }
        let (first, cursor) = mempool.pending_user_envelopes_batch_at(10, 0, 2, usize::MAX);
        assert_eq!(
            first.iter().map(|tx| tx.nonce).collect::<Vec<_>>(),
            vec![0, 1]
        );

        let (second, next_cursor) =
            mempool.pending_user_envelopes_batch_at(10, cursor, 2, usize::MAX);
        assert_eq!(
            second.iter().map(|tx| tx.nonce).collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert_eq!(next_cursor, 4);
    }

    #[test]
    fn empty_or_zero_batch_resets_cursor() {
        let mempool = crate::app::Mempool::with_config(100, 100, 0);
        let (batch, cursor) = mempool.pending_user_envelopes_batch_at(10, 7, 2, usize::MAX);
        assert!(batch.is_empty());
        assert_eq!(cursor, 0);

        let mut mempool = crate::app::Mempool::with_config(100, 100, 0);
        mempool.add_envelope(envelope(0, 0, 100), 0).unwrap();
        let (batch, cursor) = mempool.pending_user_envelopes_batch_at(10, 7, 0, usize::MAX);
        assert!(batch.is_empty());
        assert_eq!(cursor, 0);
    }

    #[test]
    fn batch_selection_is_fair_when_pool_exceeds_limit() {
        let mut mempool = crate::app::Mempool::with_config(100, 100, 0);
        for nonce in 0..3 {
            mempool.add_envelope(envelope(nonce, 0, 100), 0).unwrap();
        }
        let mut cursor = 0;
        let mut selected = Vec::new();

        for _ in 0..4 {
            let (batch, next_cursor) =
                mempool.pending_user_envelopes_batch_at(10, cursor, 1, usize::MAX);
            selected.push(batch[0].nonce);
            cursor = next_cursor;
        }

        assert_eq!(selected, vec![0, 1, 2, 0]);
    }

    #[tokio::test(start_paused = true)]
    async fn rebroadcast_retries_lost_send_with_exact_envelope_and_stops_after_expiry_or_commit() {
        let shared = SharedState::new(AppState::new_with_chain_domain([7; 32]));
        let now = current_timestamp_ms();
        let active = envelope(1, 0, u64::MAX);
        let active_bytes = bincode::serialize(&active).unwrap();
        let expired = envelope(2, 0, 1);
        let committed = envelope(3, 0, u64::MAX);
        let committed_hash = committed.hash().unwrap();

        {
            let mut app = shared.app.write().unwrap();
            app.mempool
                .add_envelope(active.clone(), now)
                .expect("active envelope should enter the canonical mempool");
            app.mempool
                .add_envelope(expired, now)
                .expect("expired envelope should enter the canonical mempool");
            app.mempool
                .add_envelope(committed, now)
                .expect("committed envelope should enter the canonical mempool");
            app.mempool.commit_proposal_unchecked(&[committed_hash]);
        }

        let publisher = RecordingPublisher {
            attempts: Arc::new(AtomicUsize::new(0)),
            fail_first: Arc::new(AtomicBool::new(true)),
            delivered: Arc::new(Mutex::new(Vec::new())),
        };
        let attempts = publisher.attempts.clone();
        let delivered = publisher.delivered.clone();
        shared.set_user_transaction_publisher(Arc::new(publisher));

        let worker = tokio::spawn(run_user_transaction_rebroadcast(shared));
        // Let the worker consume the intentionally immediate first interval
        // tick before advancing to the first retry window.
        tokio::task::yield_now().await;
        tokio::time::advance(USER_TRANSACTION_REBROADCAST_INTERVAL).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(delivered.lock().unwrap().is_empty());

        // The peer is now available. The next bounded tick must deliver the
        // exact bytes that were admitted, while expired/committed entries are
        // absent from the source mempool snapshot.
        tokio::time::advance(USER_TRANSACTION_REBROADCAST_INTERVAL).await;
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let delivered = delivered.lock().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(delivered.len(), 1);
        assert_eq!(bincode::serialize(&delivered[0]).unwrap(), active_bytes);
        drop(delivered);
        worker.abort();
        let _ = worker.await;
    }
}
