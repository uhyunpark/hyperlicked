//! Shared State
//!
//! Thread-safe state shared between API handlers and consensus.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

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
}

/// Price level for API responses
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PriceLevel {
    pub price: i64,
    pub size: i64,
}

/// Shared state accessible by API handlers
#[derive(Clone)]
pub struct SharedState {
    /// Application state (orderbooks, accounts, mempool)
    pub app: Arc<RwLock<AppState>>,
    /// Event broadcaster for WebSocket clients
    pub events: broadcast::Sender<Event>,
}

impl SharedState {
    /// Create new shared state
    pub fn new(app: AppState) -> Self {
        let (events, _) = broadcast::channel(1000);
        Self {
            app: Arc::new(RwLock::new(app)),
            events,
        }
    }

    /// Broadcast an event to all WebSocket clients
    pub fn broadcast(&self, event: Event) {
        // Ignore send errors (no receivers)
        let _ = self.events.send(event);
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }
}
