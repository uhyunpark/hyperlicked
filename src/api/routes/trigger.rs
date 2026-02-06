//! Trigger Order Endpoints
//!
//! Stop Loss and Take Profit order management.
//! All mutations require EIP-712 signatures.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::types::{
    ApiState, CancelTriggerOrderRequest, PlaceTriggerOrderRequest, PlaceTriggerOrderResponse,
    TriggerOrderInfo,
};
use crate::api::verify::{verify_cancel_trigger_order, verify_trigger_order};
use crate::app::Transaction;

pub async fn get_trigger_orders(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<TriggerOrderInfo>> {
    let app = state.shared.app.read().await;
    let orders = app.trigger_orders_by_trader(&address);

    let infos: Vec<TriggerOrderInfo> = orders
        .into_iter()
        .map(|o| {
            let side = match o.side {
                crate::app::Side::Bid => "buy",
                crate::app::Side::Ask => "sell",
            };
            let trigger_type = match o.trigger_type {
                crate::app::TriggerType::StopLoss => "sl",
                crate::app::TriggerType::TakeProfit => "tp",
            };
            let status = match o.status {
                crate::app::TriggerOrderStatus::Pending => "pending",
                crate::app::TriggerOrderStatus::Triggered => "triggered",
                crate::app::TriggerOrderStatus::Cancelled => "cancelled",
                crate::app::TriggerOrderStatus::Failed => "failed",
            };
            TriggerOrderInfo {
                id: o.id.clone(),
                cloid: o.cloid.clone(),
                symbol: o.symbol.clone(),
                side: side.to_string(),
                trigger_type: trigger_type.to_string(),
                trigger_price: o.trigger_price,
                size: o.size,
                limit_price: o.limit_price,
                status: status.to_string(),
                timestamp: o.timestamp,
            }
        })
        .collect();

    Json(infos)
}

pub async fn place_trigger_order(
    State(state): State<ApiState>,
    Json(req): Json<PlaceTriggerOrderRequest>,
) -> Result<Json<PlaceTriggerOrderResponse>, (StatusCode, String)> {
    // Look up delegation if agent mode
    let delegation = if req.agent_mode.unwrap_or(false) {
        if let Some(ref id) = req.delegation_id {
            let delegations = state.delegations.read().await;
            delegations.get(id).cloned()
        } else {
            None
        }
    } else {
        None
    };

    // Use current time for API-level validation (consensus uses block timestamp)
    let block_timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    // Verify signature
    let verified = verify_trigger_order(
        &req,
        &state.eip712_signer,
        &state.agent_signer,
        delegation.as_ref(),
        block_timestamp_ms,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Map trigger type from u8 to enum
    let trigger_type = match verified.trigger_type {
        1 => crate::app::TriggerType::StopLoss,
        2 => crate::app::TriggerType::TakeProfit,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid trigger type. Use 1 (StopLoss) or 2 (TakeProfit)".to_string(),
            ))
        }
    };

    let tx = Transaction::PlaceTriggerOrder {
        trader: format!("{:?}", verified.owner),
        symbol: verified.symbol,
        trigger_type,
        trigger_price: verified.trigger_price,
        size: verified.size,
        limit_price: verified.limit_price,
        cloid: verified.cloid,
    };

    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(hash) => {
            let trigger_order_id = format!("T{}", hex::encode(&hash[..4]));
            Ok(Json(PlaceTriggerOrderResponse {
                status: "submitted".to_string(),
                trigger_order_id,
            }))
        }
        Err(e) => Err((StatusCode::BAD_REQUEST, e.to_string())),
    }
}

pub async fn cancel_trigger_order(
    State(state): State<ApiState>,
    Json(req): Json<CancelTriggerOrderRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Verify signature
    let verified = verify_cancel_trigger_order(&req, &state.eip712_signer)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let trader = format!("{:?}", verified.owner);

    let tx = if let Some(trigger_order_id) = verified.trigger_order_id {
        Transaction::CancelTriggerOrder {
            trader,
            trigger_order_id,
        }
    } else if let (Some(symbol), Some(cloid)) = (verified.symbol, verified.cloid) {
        Transaction::CancelTriggerOrderByCloid {
            trader,
            symbol,
            cloid,
        }
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "Must provide either triggerOrderId or (symbol + cloid)".to_string(),
        ));
    };

    let mut app = state.shared.app.write().await;
    let _ = app.submit_tx(tx);

    Ok(Json(serde_json::json!({ "status": "submitted" })))
}
