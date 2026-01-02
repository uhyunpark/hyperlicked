//! WebSocket Handler
//!
//! Real-time streaming of orderbook updates, trades, and blocks.

use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use tracing::{debug, info, warn};

use super::state::{Event, SharedState};

/// WebSocket connection handler
pub struct WebSocketHandler;

/// Upgrade HTTP to WebSocket
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<SharedState>,
) -> Response {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

/// Handle WebSocket connection
pub async fn handle_socket(socket: WebSocket, state: SharedState) {
    let (mut sender, mut receiver) = socket.split();

    // Subscribe to events
    let mut event_rx = state.subscribe();

    info!("WebSocket client connected");

    // Spawn task to forward events to client
    let send_task = tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            let json = match serde_json::to_string(&event) {
                Ok(j) => j,
                Err(e) => {
                    warn!(error = %e, "Failed to serialize event");
                    continue;
                }
            };

            if sender.send(Message::Text(json)).await.is_err() {
                break;
            }
        }
    });

    // Handle incoming messages (subscriptions, pings)
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                debug!(msg = %text, "Received message");
                // Could handle subscription messages here
                // For now, clients receive all events
            }
            Ok(Message::Ping(data)) => {
                debug!("Received ping");
                // Pong is handled automatically by axum
                let _ = data; // Acknowledge
            }
            Ok(Message::Close(_)) => {
                info!("WebSocket client disconnected");
                break;
            }
            Err(e) => {
                warn!(error = %e, "WebSocket error");
                break;
            }
            _ => {}
        }
    }

    // Clean up
    send_task.abort();
    info!("WebSocket connection closed");
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
        state.broadcast(Event::Trade {
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
