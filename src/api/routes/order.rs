//! Order Submission Endpoints
//!
//! Submit and cancel orders.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, Json};

use crate::api::types::{ApiState, SignedTransaction, SubmitOrderResponse};
use crate::api::verify::{verify_cancel, verify_order};
use crate::app::{OrderType, Side, Transaction};

pub async fn submit_order(
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
    // Note: Using current time for API-level validation. Consensus-level
    // validation will use block timestamp for determinism.
    let current_timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let verified = verify_order(&req, &state.eip712_signer, &state.agent_signer, delegation.as_ref(), current_timestamp_ms)
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

pub async fn cancel_order(
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
