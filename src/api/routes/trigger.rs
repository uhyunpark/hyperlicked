//! Trigger Order Endpoints
//!
//! Stop Loss and Take Profit order management.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use std::collections::HashMap;

use crate::api::types::{
    ApiState, CancelTriggerOrderRequest, PlaceTriggerOrderRequest, PlaceTriggerOrderResponse,
    TriggerOrderInfo,
};
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
    let trigger_type = match req.trigger_type.as_str() {
        "sl" | "stop_loss" => crate::app::TriggerType::StopLoss,
        "tp" | "take_profit" => crate::app::TriggerType::TakeProfit,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid trigger type. Use 'sl' or 'tp'".to_string(),
            ))
        }
    };

    let tx = Transaction::PlaceTriggerOrder {
        trader: req.trader,
        symbol: req.symbol,
        trigger_type,
        trigger_price: req.trigger_price,
        size: req.size,
        limit_price: req.limit_price,
        cloid: req.cloid,
    };

    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(hash) => {
            // The hash is used to generate a pseudo-ID until the block commits
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
    let tx = if let Some(trigger_order_id) = req.trigger_order_id {
        Transaction::CancelTriggerOrder {
            trader: req.trader,
            trigger_order_id,
        }
    } else if let (Some(symbol), Some(cloid)) = (req.symbol, req.cloid) {
        Transaction::CancelTriggerOrderByCloid {
            trader: req.trader,
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

pub async fn cancel_trigger_order_by_id(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let trader = params
        .get("trader")
        .or(params.get("address"))
        .ok_or((StatusCode::BAD_REQUEST, "Missing trader/address parameter".to_string()))?;

    let tx = Transaction::CancelTriggerOrder {
        trader: trader.clone(),
        trigger_order_id: id,
    };

    let mut app = state.shared.app.write().await;
    let _ = app.submit_tx(tx);

    Ok(Json(serde_json::json!({ "status": "submitted" })))
}
