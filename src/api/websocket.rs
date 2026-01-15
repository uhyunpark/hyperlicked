//! WebSocket Handler
//!
//! Real-time streaming of orderbook updates, trades, and blocks.
//! Supports user-specific subscriptions for fills and position updates.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::state::{Event, SharedState, UserEvent};

/// WebSocket connection handler
pub struct WebSocketHandler;

/// Client subscription request
#[derive(Debug, Deserialize)]
struct SubscribeRequest {
    op: String,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    address: Option<String>,
}

/// Upgrade HTTP to WebSocket
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle WebSocket connection
pub async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Subscribe to public events
    let mut public_rx = state.subscribe();

    // Channel for sending messages to this client
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<String>();
    let msg_tx_clone = msg_tx.clone();

    // User subscription state
    let mut user_address: Option<String> = None;
    let mut user_rx: Option<mpsc::UnboundedReceiver<UserEvent>> = None;

    info!("WebSocket client connected");

    // Task to send messages to client
    let send_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            if ws_sender.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Task to forward public events
    let msg_tx_public = msg_tx.clone();
    let public_task = tokio::spawn(async move {
        while let Ok(event) = public_rx.recv().await {
            if let Ok(json) = serde_json::to_string(&event) {
                if msg_tx_public.send(json).is_err() {
                    break;
                }
            }
        }
    });

    // Handle incoming messages (subscriptions)
    loop {
        tokio::select! {
            // Check for user events if subscribed
            user_event = async {
                if let Some(ref mut rx) = user_rx {
                    rx.recv().await
                } else {
                    std::future::pending::<Option<UserEvent>>().await
                }
            } => {
                if let Some(event) = user_event {
                    if let Ok(json) = serde_json::to_string(&event) {
                        if msg_tx_clone.send(json).is_err() {
                            break;
                        }
                    }
                }
            }

            // Handle incoming WebSocket messages
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        debug!(msg = %text, "Received message");

                        // Parse subscription request
                        if let Ok(req) = serde_json::from_str::<SubscribeRequest>(&text) {
                            if req.op == "subscribe" {
                                // Subscribe to user events if address provided
                                if let Some(addr) = req.address {
                                    info!(address = %addr, "User subscribing to personal events");
                                    user_address = Some(addr.clone());
                                    user_rx = Some(state.subscribe_user(&addr).await);

                                    // Send confirmation
                                    let confirm = serde_json::json!({
                                        "type": "subscribed",
                                        "channel": "user",
                                        "address": addr
                                    });
                                    let _ = msg_tx_clone.send(confirm.to_string());
                                }
                            } else if req.op == "unsubscribe" {
                                if req.address.is_some() {
                                    info!("User unsubscribing from personal events");
                                    user_address = None;
                                    user_rx = None;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Ping(_))) => {
                        debug!("Received ping");
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("WebSocket client disconnected");
                        break;
                    }
                    Some(Err(e)) => {
                        warn!(error = %e, "WebSocket error");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
    }

    // Clean up
    send_task.abort();
    public_task.abort();

    if let Some(addr) = user_address {
        info!(address = %addr, "WebSocket connection closed");
    } else {
        info!("WebSocket connection closed");
    }
}

impl WebSocketHandler {
    /// Broadcast orderbook update to all clients
    pub fn broadcast_orderbook_update(
        state: &SharedState,
        symbol: &str,
        bids: Vec<super::state::PriceLevel>,
        asks: Vec<super::state::PriceLevel>,
    ) {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        state.broadcast(Event::OrderbookUpdate {
            symbol: symbol.to_string(),
            bids,
            asks,
            timestamp,
        });
    }

    /// Broadcast trade to all clients
    pub fn broadcast_trade(
        state: &SharedState,
        symbol: &str,
        price: i64,
        size: i64,
        side: &str,
        timestamp: u64,
    ) {
        // Generate deterministic ID from trade content for deduplication
        let id = format!("{}-{}-{}-{}", timestamp, price, size, side);
        state.broadcast(Event::Trade {
            id,
            symbol: symbol.to_string(),
            price,
            size,
            side: side.to_string(),
            timestamp,
        });
    }

    /// Broadcast block committed to all clients
    pub fn broadcast_block(state: &SharedState, height: u64, hash: &str, tx_count: usize) {
        state.broadcast(Event::BlockCommitted {
            height,
            hash: hash.to_string(),
            tx_count,
        });
    }
}
