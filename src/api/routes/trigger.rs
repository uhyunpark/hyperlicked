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
use crate::api::verify::{
    uses_canonical_signature, verify_cancel_trigger_order, verify_trigger_order,
};
use crate::app::{SignatureScheme, SignedEnvelope, Transaction};

pub async fn get_trigger_orders(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<TriggerOrderInfo>> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
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
        &state.security_policy,
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

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 trigger signature cannot authenticate the canonical consensus envelope; use signatureScheme=eip712-v1 and a typed-data signature".to_string(),
        ));
    }
    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical trigger envelope requires trigger.deadline".to_string(),
            ));
        }
        block_timestamp_ms.saturating_add(3_600_000)
    } else {
        verified.valid_until
    };
    let signature = if canonical_signature {
        let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
        hex::decode(sig_hex).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "invalid canonical signature".to_string(),
            )
        })?
    } else {
        b"dev".to_vec()
    };
    let envelope = SignedEnvelope::new(
        state
            .shared
            .app
            .read()
            .expect("application state lock poisoned")
            .chain_domain(),
        verified.owner.into_array(),
        verified.nonce,
        verified.valid_after,
        valid_until,
        tx,
        if canonical_signature {
            SignatureScheme::Eip712V1
        } else {
            SignatureScheme::Dev
        },
        signature,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let hash = state
        .submit_user_envelope(envelope, block_timestamp_ms)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(PlaceTriggerOrderResponse {
        status: "pending".to_string(),
        tx_hash: hex::encode(hash),
    }))
}

pub async fn cancel_trigger_order(
    State(state): State<ApiState>,
    Json(req): Json<CancelTriggerOrderRequest>,
) -> Result<Json<PlaceTriggerOrderResponse>, (StatusCode, String)> {
    let admission_timestamp_ms = block_timestamp_ms();
    // Verify signature
    let verified = verify_cancel_trigger_order(&req, &state.eip712_signer, &state.security_policy)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let trader = format!("{:?}", verified.owner);

    let tx = if let Some(trigger_order_id) = verified.trigger_order_id.clone() {
        Transaction::CancelTriggerOrder {
            trader,
            trigger_order_id,
        }
    } else if let (Some(symbol), Some(cloid)) = (verified.symbol.clone(), verified.cloid.clone()) {
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

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 trigger cancel cannot authenticate the canonical envelope; use signatureScheme=eip712-v1 and a typed-data signature".to_string(),
        ));
    }
    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical trigger cancel envelope requires cancel.deadline".to_string(),
            ));
        }
        block_timestamp_ms().saturating_add(3_600_000)
    } else {
        verified.valid_until
    };
    let signature = if canonical_signature {
        let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
        hex::decode(sig_hex).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "invalid canonical signature".to_string(),
            )
        })?
    } else {
        b"dev".to_vec()
    };
    let envelope = SignedEnvelope::new(
        state
            .shared
            .app
            .read()
            .expect("application state lock poisoned")
            .chain_domain(),
        verified.owner.into_array(),
        verified.nonce,
        verified.valid_after,
        valid_until,
        tx,
        if canonical_signature {
            SignatureScheme::Eip712V1
        } else {
            SignatureScheme::Dev
        },
        signature,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let hash = state
        .submit_user_envelope(envelope, admission_timestamp_ms)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(PlaceTriggerOrderResponse {
        status: "pending".to_string(),
        tx_hash: hex::encode(hash),
    }))
}

fn block_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}
