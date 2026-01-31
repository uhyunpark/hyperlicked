//! WebSocket Handler
//!
//! Real-time streaming of orderbook updates, trades, and blocks.
//! Supports user-specific subscriptions for fills and position updates.
//!
//! ## Authentication (CRITICAL-4)
//!
//! User subscriptions (fills, positions, orders) require EIP-712 signature
//! to prove ownership of the subscribed address. This prevents attackers
//! from spying on other users' trading activity.
//!
//! Public channels (orderbook, trades, blocks) do not require authentication.

use alloy_primitives::{keccak256, Address};
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::state::{Event, SharedState, UserEvent};
use super::types::ApiState;
use crate::config::{Config, Mode};
use crate::crypto::recover_address;

/// WebSocket connection handler
pub struct WebSocketHandler;

/// Client subscription request (unauthenticated - for public channels)
#[derive(Debug, Deserialize)]
struct SubscribeRequest {
    op: String,
    #[serde(default)]
    _channels: Vec<String>, // Reserved for future channel-specific subscriptions
    #[serde(default)]
    address: Option<String>,
    /// EIP-712 signature proving ownership (required for user channels in non-dev mode)
    /// Can be signed by the wallet OR by a delegated agent key
    #[serde(default)]
    signature: Option<String>,
    /// Timestamp for replay protection (must be within 5 minutes)
    #[serde(default)]
    timestamp: Option<u64>,
    /// Optional: agent address if signature is from an agent key
    /// If provided, we verify the signature is from this agent AND that
    /// this agent is delegated for the target address
    #[serde(default)]
    agent: Option<String>,
}

/// CRITICAL-4: Verify subscription authentication.
///
/// User subscriptions require a signature to prove ownership.
/// Signature can be from:
/// 1. The wallet itself (direct ownership)
/// 2. A delegated agent key (if agent is registered for this wallet)
///
/// The signed message format is: "Subscribe to {address} at {timestamp}"
///
/// Returns true if authentication passes or is not required (dev mode).
async fn verify_subscription_auth(
    address: &str,
    signature: &Option<String>,
    timestamp: &Option<u64>,
    agent: &Option<String>,
    api_state: &ApiState,
) -> bool {
    // In dev mode, allow unauthenticated subscriptions for testing
    if Config::global().mode == Mode::Dev {
        debug!(address, "Dev mode: allowing unauthenticated subscription");
        return true;
    }

    // Require signature and timestamp in non-dev mode
    let (signature, timestamp) = match (signature, timestamp) {
        (Some(sig), Some(ts)) => (sig, ts),
        _ => {
            warn!(address, "Subscription rejected: missing signature or timestamp");
            return false;
        }
    };

    // Verify timestamp is within 5 minutes (replay protection)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now.abs_diff(*timestamp) > 300 {
        warn!(address, timestamp, now, "Subscription rejected: timestamp too old");
        return false;
    }

    // Parse the wallet address
    let wallet_address = match address.parse::<Address>() {
        Ok(addr) => addr,
        Err(_) => {
            warn!(address, "Subscription rejected: invalid address format");
            return false;
        }
    };

    // Decode signature (hex string to bytes)
    let sig_bytes = match hex::decode(signature.trim_start_matches("0x")) {
        Ok(bytes) if bytes.len() == 65 => bytes,
        _ => {
            warn!(address, "Subscription rejected: invalid signature format");
            return false;
        }
    };

    // Create the message hash (EIP-191 personal_sign format)
    // Message: "Subscribe to {address} at {timestamp}"
    let message = format!("Subscribe to {} at {}", address, timestamp);
    let prefixed_message = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
    let message_hash: [u8; 32] = keccak256(prefixed_message.as_bytes()).into();

    // Recover signer address
    let recovered = match recover_address(&message_hash, &sig_bytes) {
        Ok(addr) => addr,
        Err(e) => {
            warn!(address, error = %e, "Subscription rejected: signature recovery failed");
            return false;
        }
    };

    // Case 1: Direct wallet signature
    if recovered == wallet_address {
        debug!(address, "Subscription authenticated by wallet");
        return true;
    }

    // Case 2: Agent key signature - verify delegation exists
    if let Some(agent_addr) = agent {
        if let Ok(expected_agent) = agent_addr.parse::<Address>() {
            if recovered == expected_agent {
                // Check if this agent is delegated for this wallet
                let delegations = api_state.delegations.read().await;
                let now_ms = now * 1000;

                // Look for a valid delegation from wallet to this agent
                // Delegations are keyed by "{wallet}-{nonce}", so we need to iterate
                // to find any valid delegation for this wallet/agent pair
                let found_valid = delegations.values().any(|stored| {
                    stored.delegation.wallet == wallet_address
                        && stored.delegation.agent == expected_agent
                        && !stored.delegation.is_expired_at(now_ms)
                });

                if found_valid {
                    debug!(
                        address,
                        agent = %agent_addr,
                        "Subscription authenticated by agent key"
                    );
                    return true;
                }

                warn!(
                    address,
                    agent = %agent_addr,
                    "Subscription rejected: no valid delegation for agent"
                );
                return false;
            }
        }
    }

    warn!(
        address,
        recovered = %recovered,
        "Subscription rejected: signature from unrecognized address"
    );
    false
}

/// Handle WebSocket connection
pub async fn handle_socket(socket: WebSocket, api_state: ApiState) {
    let (mut ws_sender, mut ws_receiver) = socket.split();

    // Get shared state reference for convenience
    let state = &api_state.shared;

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
                                    // CRITICAL-4: Verify ownership of the address
                                    // Accepts wallet signature OR agent key signature
                                    if verify_subscription_auth(
                                        &addr,
                                        &req.signature,
                                        &req.timestamp,
                                        &req.agent,
                                        &api_state,
                                    ).await {
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
                                    } else {
                                        // Send authentication error
                                        let error = serde_json::json!({
                                            "type": "error",
                                            "code": "AUTH_REQUIRED",
                                            "message": "User subscription requires signature (wallet or agent key)"
                                        });
                                        let _ = msg_tx_clone.send(error.to_string());
                                    }
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

    /// Broadcast mark price update to all clients
    pub fn broadcast_mark_price(
        state: &SharedState,
        symbol: &str,
        mark_price: i64,
        index_price: Option<i64>,
        timestamp: u64,
    ) {
        state.broadcast(Event::MarkPriceUpdate {
            symbol: symbol.to_string(),
            mark_price,
            index_price,
            timestamp,
        });
    }

    /// Broadcast asset context (market stats) to all clients
    pub fn broadcast_asset_ctx(state: &SharedState, ctx: AssetCtxData) {
        state.broadcast(Event::AssetCtx {
            symbol: ctx.symbol,
            mark_price: ctx.mark_price,
            oracle_price: ctx.oracle_price,
            mid_price: ctx.mid_price,
            funding_rate: ctx.funding_rate,
            premium: ctx.premium,
            open_interest: ctx.open_interest,
            prev_day_price: ctx.prev_day_price,
            day_volume: ctx.day_volume,
            day_notional_volume: ctx.day_notional_volume,
            next_funding_time: ctx.next_funding_time,
            timestamp: ctx.timestamp,
        });
    }
}

/// Data for asset context broadcast
pub struct AssetCtxData {
    pub symbol: String,
    pub mark_price: i64,
    pub oracle_price: Option<i64>,
    pub mid_price: i64,
    pub funding_rate: i64,
    pub premium: i64,
    pub open_interest: i64,
    pub prev_day_price: i64,
    pub day_volume: i64,
    pub day_notional_volume: i64,
    pub next_funding_time: u64,
    pub timestamp: u64,
}
