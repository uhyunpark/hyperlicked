//! REST API Routes
//!
//! All REST endpoints for the exchange.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use super::handlers::{deposit, register_delegation, submit_order_legacy, withdraw};
use super::state::{PriceLevel, SharedState};
use super::types::{
    AccountInfo, ApiState, ChainStatus, MarketInfo, OrderDetails, OrderInfo, OrderbookSnapshot,
    PositionInfo, SignedTransaction, SubmitOrderResponse,
};
use crate::app::{OrderType, Side, Transaction};

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
        .route("/delegations", post(register_delegation))
        // Deposit/Withdraw
        .route("/deposit", post(deposit))
        .route("/withdraw", post(withdraw));

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

async fn get_markets(State(_state): State<ApiState>) -> Json<Vec<MarketInfo>> {
    Json(vec![MarketInfo::default()])
}

async fn get_market(
    State(_state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<MarketInfo>, StatusCode> {
    if symbol == "BTC-USDT" {
        Ok(Json(MarketInfo::default()))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
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
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    let asks: Vec<PriceLevel> = book
        .ask_levels(20)
        .iter()
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    Ok(Json(OrderbookSnapshot {
        symbol,
        bids,
        asks,
        timestamp,
    }))
}

/// Query parameters for trades endpoint
#[derive(Deserialize)]
struct TradesQuery {
    limit: Option<usize>,
}

/// Trade response format
#[derive(serde::Serialize)]
struct TradeResponse {
    price: i64,
    size: i64,
    side: String,
    timestamp: u64,
}

async fn get_trades(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
    Query(params): Query<TradesQuery>,
) -> Json<Vec<TradeResponse>> {
    let limit = params.limit.unwrap_or(100).min(1000);
    let app = state.shared.app.read().await;

    let trades: Vec<TradeResponse> = app
        .get_trades(&symbol, limit)
        .into_iter()
        .map(|f| {
            let side = match f.side {
                crate::app::Side::Bid => "buy",
                crate::app::Side::Ask => "sell",
            };
            TradeResponse {
                price: f.price,
                size: f.size,
                side: side.to_string(),
                timestamp: f.timestamp,
            }
        })
        .collect();

    Json(trades)
}

// =============================================================================
// Account Endpoints
// =============================================================================

async fn get_account(State(state): State<ApiState>, Path(address): Path<String>) -> Json<AccountInfo> {
    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => {
            return Json(AccountInfo {
                address: address.clone(),
                balance: 0,
                locked_collateral: 0,
                available_balance: 0,
                unrealized_pnl: 0,
                total_equity: 0,
            })
        }
    };

    Json(AccountInfo {
        address: account.address.clone(),
        balance: account.balance,
        locked_collateral: account.locked,
        available_balance: account.balance,
        unrealized_pnl: 0,
        total_equity: account.balance + account.locked,
    })
}

async fn get_positions(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<PositionInfo>> {
    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => return Json(vec![]),
    };

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
                liquidation_price: pos.entry_price * 9 / 10,
                unrealized_pnl: pos.unrealized_pnl(mark),
                margin: 0,
                leverage: 10.0,
            }
        })
        .collect();

    Json(positions)
}

async fn get_orders(State(state): State<ApiState>, Path(address): Path<String>) -> Json<Vec<OrderInfo>> {
    let app = state.shared.app.read().await;
    let orders = app.orders_by_address(&address);

    let order_infos: Vec<OrderInfo> = orders
        .iter()
        .map(|o| {
            let side = match o.side {
                crate::app::Side::Bid => "buy",
                crate::app::Side::Ask => "sell",
            };
            let order_type = match o.order_type {
                crate::app::OrderType::Gtc => "limit",
                crate::app::OrderType::Ioc => "market",
                crate::app::OrderType::Alo => "limit",
            };
            let filled = o.original_size - o.size;
            let status = if filled > 0 && o.size > 0 { "partial" } else { "open" };

            OrderInfo {
                id: o.id.clone(),
                symbol: o.symbol.clone(),
                side: side.to_string(),
                order_type: order_type.to_string(),
                price: o.price,
                size: o.original_size,
                filled,
                status: status.to_string(),
                timestamp: o.timestamp,
            }
        })
        .collect();

    Json(order_infos)
}

// =============================================================================
// Chain Status
// =============================================================================

async fn get_chain_status(State(state): State<ApiState>) -> Json<ChainStatus> {
    let app = state.shared.app.read().await;
    let (b0, b1, b2) = app.mempool_stats();

    Json(ChainStatus {
        height: 0,
        view: 0,
        avg_block_time: 100.0,
        mempool_size: b0 + b1 + b2,
        validators: 4,
    })
}

// =============================================================================
// Order Submission
// =============================================================================

async fn submit_order(
    State(state): State<ApiState>,
    Json(req): Json<SignedTransaction>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    if req.tx_type == "order" {
        submit_order_tx(&state, req.order).await
    } else if req.tx_type == "cancel" {
        submit_cancel_tx(&state, req.cancel).await
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown transaction type: {}", req.tx_type),
        ))
    }
}

async fn submit_order_tx(
    state: &ApiState,
    order: Option<OrderDetails>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    let order = order.ok_or((StatusCode::BAD_REQUEST, "Missing order details".to_string()))?;

    let side = match order.side {
        1 => Side::Bid,
        2 => Side::Ask,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid side".to_string())),
    };

    let order_type = match order.order_type {
        1 => OrderType::Gtc,
        2 => OrderType::Ioc,
        3 => OrderType::Alo,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid order type".to_string())),
    };

    let price: i64 = order
        .price
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid price".to_string()))?;
    let size: i64 = order
        .qty
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid qty".to_string()))?;

    let tx = Transaction::PlaceOrder {
        trader: order.owner,
        symbol: order.symbol,
        side,
        price,
        size,
        order_type,
        reduce_only: order.reduce_only.unwrap_or(false),
    };

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
}

async fn submit_cancel_tx(
    state: &ApiState,
    cancel: Option<super::types::CancelDetails>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    let cancel = cancel.ok_or((StatusCode::BAD_REQUEST, "Missing cancel details".to_string()))?;

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
}

// =============================================================================
// Cancel Order
// =============================================================================

async fn cancel_order(
    State(state): State<ApiState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Try signed cancel format first
    if let Some(tx_type) = body.get("type").and_then(|v| v.as_str()) {
        if tx_type == "cancel" {
            let req: SignedTransaction =
                serde_json::from_value(body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

            let cancel = req
                .cancel
                .ok_or((StatusCode::BAD_REQUEST, "Missing cancel details".to_string()))?;

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

    // Simple cancel format: { orderId, address }
    let order_id = body
        .get("orderId")
        .and_then(|v| v.as_str())
        .ok_or((StatusCode::BAD_REQUEST, "Missing orderId".to_string()))?;

    let address = body.get("address").and_then(|v| v.as_str()).unwrap_or("unknown");

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
