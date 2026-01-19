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
    AccountInfo, ApiState, CandleInfo, ChainStatus, FundingInfo, MarketInfo, OrderInfo, OrderbookSnapshot,
    PositionInfo, SignedTransaction, SubmitOrderResponse,
};
use super::verify::{verify_cancel, verify_order};
use crate::app::{OrderType, Side, Transaction};

pub fn create_router(state: SharedState) -> Router {
    let api_state = ApiState::new(state);

    let api_v1 = Router::new()
        // Market endpoints
        .route("/markets", get(get_markets))
        .route("/markets/:symbol", get(get_market))
        .route("/markets/:symbol/orderbook", get(get_orderbook))
        .route("/markets/:symbol/trades", get(get_trades))
        .route("/markets/:symbol/candles", get(get_candles))
        .route("/markets/:symbol/funding", get(get_funding))
        // Account endpoints
        .route("/accounts/:address", get(get_account))
        .route("/accounts/:address/positions", get(get_positions))
        .route("/accounts/:address/orders", get(get_orders))
        .route("/accounts/:address/nonce", get(get_nonce))
        .route("/accounts/:address/funding", get(get_account_funding))
        // Chain endpoints
        .route("/chain/status", get(get_chain_status))
        .route("/chain/insurance-fund", get(get_insurance_fund))
        // Staking endpoints
        .route("/staking/validators", get(get_validators))
        .route("/staking/validators/:operator", get(get_validator))
        .route("/staking/delegations/:address", get(get_delegations))
        .route("/staking/epoch", get(get_epoch))
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
    /// Deterministic trade ID for deduplication
    id: String,
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
            // Generate deterministic ID from trade content for deduplication
            let id = format!("{}-{}-{}-{}", f.timestamp, f.price, f.size, side);
            TradeResponse {
                id,
                price: f.price,
                size: f.size,
                side: side.to_string(),
                timestamp: f.timestamp,
            }
        })
        .collect();

    Json(trades)
}

/// Query parameters for candles endpoint
#[derive(Deserialize)]
struct CandlesQuery {
    interval: Option<String>,
    limit: Option<usize>,
}

async fn get_candles(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
    Query(params): Query<CandlesQuery>,
) -> Result<Json<Vec<CandleInfo>>, StatusCode> {
    use crate::app::Interval;

    let interval_str = params.interval.as_deref().unwrap_or("1m");
    let interval = Interval::from_str(interval_str).ok_or(StatusCode::BAD_REQUEST)?;
    let limit = params.limit.unwrap_or(500).min(500);

    let app = state.shared.app.read().await;

    // Check if market exists
    if app.orderbook(&symbol).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let candles: Vec<CandleInfo> = app
        .get_candles(&symbol, interval, limit)
        .into_iter()
        .map(|c| CandleInfo {
            time: c.time,
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            volume: c.volume,
            trades: c.trades,
        })
        .collect();

    Ok(Json(candles))
}

async fn get_funding(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<FundingInfo>, StatusCode> {
    let app = state.shared.app.read().await;

    // Check if market exists
    if app.orderbook(&symbol).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let funding_rate_bps = app.funding_rate(&symbol);
    let last_funding_time = app.last_funding_time(&symbol);
    let next_funding_time = app.next_funding_time(&symbol);

    Ok(Json(FundingInfo {
        symbol,
        funding_rate: funding_rate_bps as f64 / 10000.0, // Convert bps to decimal
        funding_rate_bps,
        next_funding_time,
        last_funding_time,
    }))
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
    use crate::app::MAINTENANCE_MARGIN_BPS;

    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => return Json(vec![]),
    };

    // Get available margin for liquidation price calculation
    let available_margin = account.balance + account.locked;

    let positions: Vec<PositionInfo> = account
        .positions
        .iter()
        .filter(|(_, pos)| pos.size != 0)
        .map(|(symbol, pos)| {
            let mark = app.mark_price(symbol).unwrap_or(pos.entry_price);
            let notional = pos.notional(mark);
            // Approximate margin allocated to this position (proportional to notional)
            let total_notional: i64 = account.positions.values()
                .filter(|p| p.size != 0)
                .map(|p| p.notional(mark))
                .sum();
            let position_margin = if total_notional > 0 {
                (available_margin * notional) / total_notional
            } else {
                available_margin
            };

            PositionInfo {
                symbol: symbol.clone(),
                size: pos.size,
                entry_price: pos.entry_price,
                mark_price: mark,
                liquidation_price: pos.liquidation_price(position_margin, MAINTENANCE_MARGIN_BPS),
                unrealized_pnl: pos.unrealized_pnl(mark),
                margin: position_margin,
                leverage: if position_margin > 0 { notional as f64 / position_margin as f64 } else { 0.0 },
            }
        })
        .collect();

    Json(positions)
}

async fn get_nonce(State(state): State<ApiState>, Path(address): Path<String>) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    let nonce = app.accounts().get_nonce(&address);
    Json(serde_json::json!({ "address": address, "nonce": nonce }))
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

async fn get_account_funding(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<super::types::FundingPayment>> {
    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => return Json(vec![]),
    };

    // Return cumulative funding for each position
    let payments: Vec<super::types::FundingPayment> = account
        .positions
        .iter()
        .filter(|(_, pos)| pos.cumulative_funding != 0 || pos.size != 0)
        .map(|(symbol, pos)| {
            super::types::FundingPayment {
                symbol: symbol.clone(),
                payment: pos.cumulative_funding,
                payment_usd: pos.cumulative_funding as f64 / 100.0,
                funding_rate_bps: app.funding_rate(symbol),
                timestamp: pos.last_funding_timestamp,
            }
        })
        .collect();

    Json(payments)
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

async fn get_insurance_fund(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    Json(serde_json::json!({
        "balance": app.insurance_fund_balance(),
        "balance_usd": app.insurance_fund_balance() as f64 / 100.0
    }))
}

// =============================================================================
// Order Submission
// =============================================================================

async fn submit_order(
    State(state): State<ApiState>,
    Json(req): Json<SignedTransaction>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    if req.tx_type == "order" {
        submit_order_tx(&state, req).await
    } else if req.tx_type == "cancel" {
        submit_cancel_tx(&state, req).await
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            format!("Unknown transaction type: {}", req.tx_type),
        ))
    }
}

async fn submit_order_tx(
    state: &ApiState,
    req: SignedTransaction,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    // Get delegation if agent mode
    let delegation = if req.agent_mode.unwrap_or(false) {
        let delegation_id = req
            .delegation_id
            .as_ref()
            .ok_or((StatusCode::BAD_REQUEST, "Missing delegation_id for agent mode".to_string()))?;

        let delegations = state.delegations.read().await;
        delegations.get(delegation_id).cloned()
    } else {
        None
    };

    // Verify signature and extract verified data
    let verified = verify_order(&req, &state.eip712_signer, &state.agent_signer, delegation.as_ref())
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Validate and consume nonce
    {
        let mut app = state.shared.app.write().await;
        let address_str = format!("{:?}", verified.owner);

        app.accounts_mut()
            .use_nonce(&address_str, verified.nonce)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    }

    // Convert to internal types
    let side = match verified.side {
        1 => Side::Bid,
        2 => Side::Ask,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid side".to_string())),
    };

    let order_type = match verified.order_type {
        1 => OrderType::Gtc,
        2 => OrderType::Ioc,
        3 => OrderType::Alo,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid order type".to_string())),
    };

    let trader = format!("{:?}", verified.owner);

    let tx = Transaction::PlaceOrder {
        trader,
        symbol: verified.symbol,
        side,
        price: verified.price,
        size: verified.size,
        order_type,
        reduce_only: verified.reduce_only,
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
    req: SignedTransaction,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    // Verify signature
    let verified = verify_cancel(&req, &state.eip712_signer)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Validate and consume nonce
    {
        let mut app = state.shared.app.write().await;
        let address_str = format!("{:?}", verified.owner);

        app.accounts_mut()
            .use_nonce(&address_str, verified.nonce)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    }

    let trader = format!("{:?}", verified.owner);

    let tx = Transaction::CancelOrder {
        trader,
        order_id: verified.order_id.clone(),
    };

    let mut app = state.shared.app.write().await;
    let _ = app.submit_tx(tx);

    Ok(Json(SubmitOrderResponse {
        status: "submitted".to_string(),
        order_id: verified.order_id,
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

// =============================================================================
// Staking Endpoints
// =============================================================================

/// Validator info for API response
#[derive(serde::Serialize)]
struct ValidatorResponse {
    operator: String,
    node_id: String,
    self_stake: i64,
    self_stake_usd: f64,
    total_stake: i64,
    total_stake_usd: f64,
    commission_bps: i64,
    status: String,
    pending_rewards: i64,
}

async fn get_validators(State(state): State<ApiState>) -> Json<Vec<ValidatorResponse>> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let validators: Vec<ValidatorResponse> = staking
        .validators
        .values()
        .map(|v| ValidatorResponse {
            operator: v.operator.clone(),
            node_id: hex::encode(v.node_id),
            self_stake: v.self_stake,
            self_stake_usd: v.self_stake as f64 / 100.0,
            total_stake: v.total_stake,
            total_stake_usd: v.total_stake as f64 / 100.0,
            commission_bps: v.commission_bps,
            status: format!("{:?}", v.status),
            pending_rewards: v.pending_rewards,
        })
        .collect();

    Json(validators)
}

async fn get_validator(
    State(state): State<ApiState>,
    Path(operator): Path<String>,
) -> Result<Json<ValidatorResponse>, StatusCode> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let v = staking
        .validators
        .get(&operator)
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ValidatorResponse {
        operator: v.operator.clone(),
        node_id: hex::encode(v.node_id),
        self_stake: v.self_stake,
        self_stake_usd: v.self_stake as f64 / 100.0,
        total_stake: v.total_stake,
        total_stake_usd: v.total_stake as f64 / 100.0,
        commission_bps: v.commission_bps,
        status: format!("{:?}", v.status),
        pending_rewards: v.pending_rewards,
    }))
}

/// Delegation info for API response
#[derive(serde::Serialize)]
struct DelegationResponse {
    delegator: String,
    validator: String,
    amount: i64,
    amount_usd: f64,
    pending_rewards: i64,
}

async fn get_delegations(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<DelegationResponse>> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let delegations: Vec<DelegationResponse> = staking
        .delegations_for(&address)
        .into_iter()
        .map(|d| DelegationResponse {
            delegator: d.delegator.clone(),
            validator: d.validator.clone(),
            amount: d.amount,
            amount_usd: d.amount as f64 / 100.0,
            pending_rewards: d.pending_rewards,
        })
        .collect();

    Json(delegations)
}

/// Epoch info for API response
#[derive(serde::Serialize)]
struct EpochResponse {
    current_epoch: u64,
    current_view: u64,
    active_validators: usize,
    total_staked: i64,
    total_staked_usd: f64,
    rounds_per_epoch: u64,
}

async fn get_epoch(State(state): State<ApiState>) -> Json<EpochResponse> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    Json(EpochResponse {
        current_epoch: staking.current_epoch,
        current_view: app.current_view(),
        active_validators: staking.active_validators().len(),
        total_staked: staking.total_staked,
        total_staked_usd: staking.total_staked as f64 / 100.0,
        rounds_per_epoch: crate::app::staking::ROUNDS_PER_EPOCH,
    })
}
