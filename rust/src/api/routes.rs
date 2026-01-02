//! REST API Routes
//!
//! All REST endpoints for the exchange.
//! Matches the Go API for frontend compatibility.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::state::{PriceLevel, SharedState};
use crate::app::{OrderType, Side, Transaction};
use crate::crypto::{AgentDelegation, AgentSigner, EIP712Signer};

/// Stored delegation with signature
#[derive(Clone)]
pub struct StoredDelegation {
    pub delegation: AgentDelegation,
    pub signature: Vec<u8>,
}

/// Extended shared state with delegations
#[derive(Clone)]
pub struct ApiState {
    pub shared: SharedState,
    pub delegations: Arc<RwLock<HashMap<String, StoredDelegation>>>,
    pub eip712_signer: Arc<EIP712Signer>,
    pub agent_signer: Arc<AgentSigner>,
}

impl ApiState {
    pub fn new(shared: SharedState) -> Self {
        Self {
            shared,
            delegations: Arc::new(RwLock::new(HashMap::new())),
            eip712_signer: Arc::new(EIP712Signer::default_domain()),
            agent_signer: Arc::new(AgentSigner::default_domain()),
        }
    }
}

/// Create the API router (matches Go frontend)
pub fn create_router(state: SharedState) -> Router {
    let api_state = ApiState::new(state);

    let api_v1 = Router::new()
        // Market endpoints
        .route("/markets", get(get_markets))
        .route("/markets/:symbol", get(get_market))
        .route("/markets/:symbol/orderbook", get(get_orderbook))
        .route("/markets/:symbol/trades", get(get_trades))
        // Account endpoints
        .route("/accounts/:address", get(get_account))
        .route("/accounts/:address/positions", get(get_positions))
        .route("/accounts/:address/orders", get(get_orders))
        // Chain endpoints
        .route("/chain/status", get(get_chain_status))
        // Order submission
        .route("/orders", post(submit_order))
        .route("/orders/cancel", post(cancel_order))
        // Agent delegation
        .route("/delegations", post(register_delegation));

    Router::new()
        // Health check
        .route("/health", get(health))
        // API v1 routes
        .nest("/api/v1", api_v1)
        // WebSocket (legacy route too)
        .route("/ws", get(ws_handler_wrapped))
        // Legacy routes for compatibility
        .route("/api/order", post(submit_order_legacy))
        .route("/api/orderbook/:symbol", get(get_orderbook))
        .route("/api/account/:address", get(get_account))
        .route("/api/deposit", post(deposit))
        .route("/api/withdraw", post(withdraw))
        // State
        .with_state(api_state)
}

// =============================================================================
// WebSocket wrapper
// =============================================================================

async fn ws_handler_wrapped(
    ws: axum::extract::ws::WebSocketUpgrade,
    State(state): State<ApiState>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| super::websocket::handle_socket(socket, state.shared))
}

// =============================================================================
// Health Check
// =============================================================================

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

// =============================================================================
// Market Endpoints
// =============================================================================

#[derive(Debug, Serialize)]
pub struct MarketInfo {
    pub symbol: String,
    #[serde(rename = "baseAsset")]
    pub base_asset: String,
    #[serde(rename = "quoteAsset")]
    pub quote_asset: String,
    #[serde(rename = "type")]
    pub market_type: String,
    pub status: String,
    #[serde(rename = "tickSize")]
    pub tick_size: i64,
    #[serde(rename = "lotSize")]
    pub lot_size: i64,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: i32,
    #[serde(rename = "takerFeeBps")]
    pub taker_fee_bps: i64,
    #[serde(rename = "makerFeeBps")]
    pub maker_fee_bps: i64,
}

async fn get_markets(State(state): State<ApiState>) -> Json<Vec<MarketInfo>> {
    // Return default BTC-USDT market
    Json(vec![MarketInfo {
        symbol: "BTC-USDT".to_string(),
        base_asset: "BTC".to_string(),
        quote_asset: "USDT".to_string(),
        market_type: "perp".to_string(),
        status: "active".to_string(),
        tick_size: 1,
        lot_size: 1,
        max_leverage: 50,
        taker_fee_bps: 5,
        maker_fee_bps: 2,
    }])
}

async fn get_market(
    State(_state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<MarketInfo>, StatusCode> {
    if symbol == "BTC-USDT" {
        Ok(Json(MarketInfo {
            symbol: "BTC-USDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            market_type: "perp".to_string(),
            status: "active".to_string(),
            tick_size: 1,
            lot_size: 1,
            max_leverage: 50,
            taker_fee_bps: 5,
            maker_fee_bps: 2,
        }))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[derive(Debug, Serialize)]
pub struct OrderbookSnapshot {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: i64,
}

async fn get_orderbook(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<OrderbookSnapshot>, StatusCode> {
    let app = state.shared.app.read().await;
    let book = app.orderbook(&symbol).ok_or(StatusCode::NOT_FOUND)?;

    let bids: Vec<PriceLevel> = book
        .bid_levels(20)
        .iter()
        .map(|l| PriceLevel { price: l.price, size: l.size })
        .collect();

    let asks: Vec<PriceLevel> = book
        .ask_levels(20)
        .iter()
        .map(|l| PriceLevel { price: l.price, size: l.size })
        .collect();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    Ok(Json(OrderbookSnapshot { symbol, bids, asks, timestamp }))
}

async fn get_trades(Path(_symbol): Path<String>) -> Json<Vec<serde_json::Value>> {
    // TODO: Implement trade history
    Json(vec![])
}

// =============================================================================
// Account Endpoints
// =============================================================================

#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub address: String,
    pub balance: i64,
    #[serde(rename = "lockedCollateral")]
    pub locked_collateral: i64,
    #[serde(rename = "availableBalance")]
    pub available_balance: i64,
    #[serde(rename = "unrealizedPnL")]
    pub unrealized_pnl: i64,
    #[serde(rename = "totalEquity")]
    pub total_equity: i64,
}

async fn get_account(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Result<Json<AccountInfo>, StatusCode> {
    let app = state.shared.app.read().await;
    let account = app.account(&address).ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(AccountInfo {
        address: account.address.clone(),
        balance: account.balance,
        locked_collateral: account.locked,
        available_balance: account.balance,
        unrealized_pnl: 0, // TODO: Calculate
        total_equity: account.balance + account.locked,
    }))
}

#[derive(Debug, Serialize)]
pub struct PositionInfo {
    pub symbol: String,
    pub size: i64,
    #[serde(rename = "entryPrice")]
    pub entry_price: i64,
    #[serde(rename = "markPrice")]
    pub mark_price: i64,
    #[serde(rename = "liquidationPrice")]
    pub liquidation_price: i64,
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: i64,
    pub margin: i64,
    pub leverage: f64,
}

async fn get_positions(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Result<Json<Vec<PositionInfo>>, StatusCode> {
    let app = state.shared.app.read().await;
    let account = app.account(&address).ok_or(StatusCode::NOT_FOUND)?;

    let positions: Vec<PositionInfo> = account
        .positions
        .iter()
        .filter(|(_, pos)| pos.size != 0)
        .map(|(symbol, pos)| {
            let mark = app.mark_price(symbol).unwrap_or(pos.entry_price);
            PositionInfo {
                symbol: symbol.clone(),
                size: pos.size,
                entry_price: pos.entry_price,
                mark_price: mark,
                liquidation_price: pos.entry_price * 9 / 10, // Placeholder
                unrealized_pnl: pos.unrealized_pnl(mark),
                margin: 0, // TODO
                leverage: 10.0, // TODO
            }
        })
        .collect();

    Ok(Json(positions))
}

async fn get_orders(Path(_address): Path<String>) -> Json<Vec<serde_json::Value>> {
    // TODO: Implement order tracking
    Json(vec![])
}

// =============================================================================
// Chain Status
// =============================================================================

#[derive(Debug, Serialize)]
pub struct ChainStatus {
    pub height: u64,
    pub view: u64,
    #[serde(rename = "avgBlockTime")]
    pub avg_block_time: f64,
    #[serde(rename = "mempoolSize")]
    pub mempool_size: usize,
    pub validators: usize,
}

async fn get_chain_status(State(state): State<ApiState>) -> Json<ChainStatus> {
    let app = state.shared.app.read().await;
    let (b0, b1, b2) = app.mempool_stats();

    Json(ChainStatus {
        height: 0, // TODO: Get from consensus
        view: 0,
        avg_block_time: 100.0,
        mempool_size: b0 + b1 + b2,
        validators: 4,
    })
}

// =============================================================================
// Order Submission (Signed) - Matches frontend SignedTransaction format
// =============================================================================

/// Order details within SignedTransaction
#[derive(Debug, Deserialize)]
pub struct OrderDetails {
    pub symbol: String,
    pub side: u8,           // 1=Buy, 2=Sell
    #[serde(rename = "type")]
    pub order_type: u8,     // 1=GTC, 2=IOC, 3=ALO
    pub price: String,      // BigInt as string
    pub qty: String,        // BigInt as string
    pub nonce: String,      // BigInt as string
    pub deadline: String,   // BigInt as string
    pub leverage: u8,
    pub owner: String,      // Address
}

/// Cancel details within SignedTransaction
#[derive(Debug, Deserialize)]
pub struct CancelDetails {
    pub order_id: String,
    pub symbol: String,
    pub nonce: String,
    pub owner: String,
}

/// Signed transaction from frontend (matches lib/api.ts SignedTransaction)
#[derive(Debug, Deserialize)]
pub struct SignedTransaction {
    #[serde(rename = "type")]
    pub tx_type: String,            // "order" or "cancel"
    pub order: Option<OrderDetails>,
    pub cancel: Option<CancelDetails>,
    pub signature: String,
    pub agent_mode: Option<bool>,
    pub delegation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitOrderResponse {
    pub status: String,
    #[serde(rename = "orderId")]
    pub order_id: String,
    pub message: Option<String>,
}

async fn submit_order(
    State(state): State<ApiState>,
    Json(req): Json<SignedTransaction>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    // Handle order submission
    if req.tx_type == "order" {
        let order = req.order.ok_or((StatusCode::BAD_REQUEST, "Missing order details".to_string()))?;

        // Parse side: 1=Buy, 2=Sell
        let side = match order.side {
            1 => Side::Bid,
            2 => Side::Ask,
            _ => return Err((StatusCode::BAD_REQUEST, "Invalid side".to_string())),
        };

        // Parse order type: 1=GTC, 2=IOC, 3=ALO
        let order_type = match order.order_type {
            1 => OrderType::Gtc,
            2 => OrderType::Ioc,
            3 => OrderType::Alo,
            _ => return Err((StatusCode::BAD_REQUEST, "Invalid order type".to_string())),
        };

        // Parse price and qty (BigInt strings to i64)
        let price: i64 = order.price.parse()
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid price".to_string()))?;
        let size: i64 = order.qty.parse()
            .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid qty".to_string()))?;

        // TODO: Verify EIP-712 signature or agent signature
        // For now, trust the owner field
        let trader = order.owner.clone();

        // Create transaction
        let tx = Transaction::PlaceOrder {
            trader,
            symbol: order.symbol,
            side,
            price,
            size,
            order_type,
        };

        // Submit to mempool
        let mut app = state.shared.app.write().await;
        match app.submit_tx(tx) {
            Ok(hash) => {
                let order_id = format!("0x{}", hex::encode(&hash[..4]));
                Ok(Json(SubmitOrderResponse {
                    status: "submitted".to_string(),
                    order_id,
                    message: None,
                }))
            }
            Err(e) => Err((StatusCode::BAD_REQUEST, e.to_string())),
        }
    } else if req.tx_type == "cancel" {
        let cancel = req.cancel.ok_or((StatusCode::BAD_REQUEST, "Missing cancel details".to_string()))?;

        let tx = Transaction::CancelOrder {
            trader: cancel.owner,
            order_id: cancel.order_id.clone(),
        };

        let mut app = state.shared.app.write().await;
        let _ = app.submit_tx(tx);

        Ok(Json(SubmitOrderResponse {
            status: "submitted".to_string(),
            order_id: cancel.order_id,
            message: None,
        }))
    } else {
        Err((StatusCode::BAD_REQUEST, format!("Unknown transaction type: {}", req.tx_type)))
    }
}

// =============================================================================
// Cancel Order - handles both simple and signed formats
// =============================================================================

async fn cancel_order(
    State(state): State<ApiState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Try to parse as SignedTransaction first (signed cancel)
    if let Some(tx_type) = body.get("type").and_then(|v| v.as_str()) {
        if tx_type == "cancel" {
            let req: SignedTransaction = serde_json::from_value(body)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

            let cancel = req.cancel.ok_or((StatusCode::BAD_REQUEST, "Missing cancel details".to_string()))?;

            let tx = Transaction::CancelOrder {
                trader: cancel.owner,
                order_id: cancel.order_id.clone(),
            };

            let mut app = state.shared.app.write().await;
            let _ = app.submit_tx(tx);

            return Ok(Json(serde_json::json!({
                "status": "submitted",
                "orderId": cancel.order_id
            })));
        }
    }

    // Otherwise parse as simple cancel request { orderId, address }
    let order_id = body.get("orderId")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "Missing orderId".to_string()))?;

    let address = body.get("address")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let tx = Transaction::CancelOrder {
        trader: address.to_string(),
        order_id: order_id.to_string(),
    };

    let mut app = state.shared.app.write().await;
    let _ = app.submit_tx(tx);

    Ok(Json(serde_json::json!({
        "status": "submitted",
        "orderId": order_id
    })))
}

// =============================================================================
// Agent Delegation
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct RegisterDelegationRequest {
    pub wallet: String,
    pub agent: String,
    pub expiration: String,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterDelegationResponse {
    pub status: String,
    #[serde(rename = "delegationId")]
    pub delegation_id: String,
    pub message: String,
}

async fn register_delegation(
    State(state): State<ApiState>,
    Json(req): Json<RegisterDelegationRequest>,
) -> Result<Json<RegisterDelegationResponse>, (StatusCode, String)> {
    use alloy_primitives::{Address, U256};

    // Parse addresses
    let wallet: Address = req.wallet.parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid wallet address".to_string()))?;
    let agent: Address = req.agent.parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid agent address".to_string()))?;

    // Parse expiration and nonce
    let expiration: u64 = req.expiration.parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid expiration".to_string()))?;
    let nonce: U256 = U256::from_str_radix(&req.nonce, 10)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid nonce".to_string()))?;

    // Decode signature
    let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
    let signature = hex::decode(sig_hex)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid signature hex".to_string()))?;

    // Create delegation
    let delegation = AgentDelegation {
        wallet,
        agent,
        expiration,
        nonce,
    };

    // Verify signature
    let valid = state.agent_signer.verify_delegation(&delegation, &signature)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if !valid {
        return Err((StatusCode::BAD_REQUEST, "Invalid delegation signature".to_string()));
    }

    // Store delegation
    let delegation_id = format!("{}-{}", req.wallet, req.nonce);
    let stored = StoredDelegation { delegation, signature };

    state.delegations.write().await.insert(delegation_id.clone(), stored);

    Ok(Json(RegisterDelegationResponse {
        status: "registered".to_string(),
        delegation_id,
        message: "Agent key delegation registered successfully".to_string(),
    }))
}

// =============================================================================
// Legacy Endpoints (for compatibility)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct LegacyOrderRequest {
    pub trader: String,
    pub symbol: String,
    pub side: String,
    pub price: i64,
    pub size: i64,
    pub order_type: String,
}

async fn submit_order_legacy(
    State(state): State<ApiState>,
    Json(req): Json<LegacyOrderRequest>,
) -> Json<serde_json::Value> {
    let side = match req.side.to_lowercase().as_str() {
        "buy" | "bid" => Side::Bid,
        _ => Side::Ask,
    };

    let order_type = match req.order_type.to_lowercase().as_str() {
        "ioc" => OrderType::Ioc,
        "alo" => OrderType::Alo,
        _ => OrderType::Gtc,
    };

    let tx = Transaction::PlaceOrder {
        trader: req.trader,
        symbol: req.symbol,
        side,
        price: req.price,
        size: req.size,
        order_type,
    };

    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(hash) => Json(serde_json::json!({
            "success": true,
            "tx_hash": hex::encode(hash)
        })),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e.to_string()
        })),
    }
}

#[derive(Debug, Deserialize)]
pub struct DepositRequest {
    pub trader: String,
    pub amount: i64,
}

async fn deposit(
    State(state): State<ApiState>,
    Json(req): Json<DepositRequest>,
) -> Json<serde_json::Value> {
    let tx = Transaction::Deposit {
        trader: req.trader,
        amount: req.amount,
    };

    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(_) => Json(serde_json::json!({"success": true})),
        Err(e) => Json(serde_json::json!({"success": false, "error": e.to_string()})),
    }
}

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    pub trader: String,
    pub amount: i64,
}

async fn withdraw(
    State(state): State<ApiState>,
    Json(req): Json<WithdrawRequest>,
) -> Json<serde_json::Value> {
    let tx = Transaction::Withdraw {
        trader: req.trader,
        amount: req.amount,
    };

    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(_) => Json(serde_json::json!({"success": true})),
        Err(e) => Json(serde_json::json!({"success": false, "error": e.to_string()})),
    }
}
