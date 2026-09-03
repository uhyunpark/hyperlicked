//! Order Submission Endpoints
//!
//! Submit and cancel orders.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, Json};

use crate::api::types::{ApiState, SignedTransaction, SubmitOrderResponse};
use crate::api::verify::{uses_canonical_signature, verify_cancel, verify_order};
use crate::app::{OrderType, Side, SignatureScheme, SignedEnvelope, Transaction};

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
        let delegation_id = req.delegation_id.as_ref().ok_or((
            StatusCode::BAD_REQUEST,
            "Missing delegation_id for agent mode".to_string(),
        ))?;

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
    let verified = verify_order(
        &req,
        &state.eip712_signer,
        &state.agent_signer,
        delegation.as_ref(),
        current_timestamp_ms,
        &state.security_policy,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Validate margin/position without touching account nonce.  Nonces are
    // consumed only by deterministic block execution on every validator.
    {
        let app = state
            .shared
            .app
            .write()
            .expect("application state lock poisoned");
        let address_str = format!("{:?}", verified.owner);

        // Pre-submission validation: check balance and position constraints
        // This prevents "Order Submitted" toast when the order will fail at execution
        if let Some(account) = app.account(&address_str) {
            if verified.reduce_only {
                // Reduce-only validation: must have position in correct direction
                let pos = account.position(&verified.symbol);
                // side: 1 = Buy, 2 = Sell
                // Long (pos.size > 0) can only reduce with Sell (2)
                // Short (pos.size < 0) can only reduce with Buy (1)
                let is_reducing =
                    (pos.size > 0 && verified.side == 2) || (pos.size < 0 && verified.side == 1);
                if pos.size == 0 {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Reduce-only order invalid: no open position".to_string(),
                    ));
                }
                if !is_reducing {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Reduce-only order invalid: wrong direction".to_string(),
                    ));
                }
            } else {
                // Margin check for non-reduce-only orders. Keep this API
                // precheck bounded; consensus execution remains authoritative.
                let required_margin = calculate_required_margin(verified.price, verified.size)
                    .map_err(|reason| (StatusCode::BAD_REQUEST, reason.to_string()))?;
                if account.balance < required_margin {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!(
                            "Insufficient margin: need {} cents, have {} cents",
                            required_margin, account.balance
                        ),
                    ));
                }
            }
        } else {
            // Account doesn't exist - only allow if in dev mode with faucet
            // For non-dev mode, reject orders from non-existent accounts
            if !verified.reduce_only {
                let required_margin = calculate_required_margin(verified.price, verified.size)
                    .map_err(|reason| (StatusCode::BAD_REQUEST, reason.to_string()))?;
                if required_margin > 0 {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        "Insufficient margin: account has no funds".to_string(),
                    ));
                }
            }
        }
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

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 signature cannot authenticate the canonical consensus envelope; use signatureScheme=eip712-v1 and sign the protocol-defined HyperLickedTransaction typed data".to_string(),
        ));
    }

    let (scheme, signature) = if canonical_signature {
        let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
        let signature = hex::decode(sig_hex).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "invalid canonical signature".to_string(),
            )
        })?;
        (SignatureScheme::Eip712V1, signature)
    } else {
        (SignatureScheme::Dev, b"dev".to_vec())
    };

    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical order envelope requires a non-zero deadline".to_string(),
            ));
        }
        current_timestamp_ms.saturating_add(3_600_000)
    } else {
        verified.valid_until
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
        scheme,
        signature,
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let hash = state
        .submit_user_envelope(envelope, current_timestamp_ms)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(SubmitOrderResponse {
        status: "pending".to_string(),
        tx_hash: hex::encode(hash),
        message: None,
    }))
}

async fn submit_cancel_tx(
    state: &ApiState,
    req: SignedTransaction,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    let admission_timestamp_ms = current_timestamp_ms();
    // Verify signature
    let verified = verify_cancel(&req, &state.eip712_signer, &state.security_policy)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let trader = format!("{:?}", verified.owner);

    let tx = Transaction::CancelOrder {
        trader,
        order_id: verified.order_id.clone(),
    };

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 cancel cannot authenticate the canonical envelope; provide signatureScheme=eip712-v1 and a typed-data signature".to_string(),
        ));
    }
    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical cancel envelope requires cancel.deadline".to_string(),
            ));
        }
        current_timestamp_ms().saturating_add(3_600_000)
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

    Ok(Json(SubmitOrderResponse {
        status: "pending".to_string(),
        tx_hash: hex::encode(hash),
        message: None,
    }))
}

/// Cancel order endpoint - requires signed request
///
/// SECURITY: All cancels must be signed to prevent attackers from
/// canceling other users' orders. The signature proves ownership.
pub async fn cancel_order(
    State(state): State<ApiState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<SubmitOrderResponse>, (StatusCode, String)> {
    let admission_timestamp_ms = current_timestamp_ms();
    // Only accept signed cancel format
    let req: SignedTransaction = serde_json::from_value(body).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid cancel request: {}. Cancels must be signed.", e),
        )
    })?;

    // Verify signature and extract verified data
    let verified = verify_cancel(&req, &state.eip712_signer, &state.security_policy)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let trader = format!("{:?}", verified.owner);

    let tx = Transaction::CancelOrder {
        trader,
        order_id: verified.order_id.clone(),
    };

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 cancel cannot authenticate the canonical envelope; provide signatureScheme=eip712-v1 and a typed-data signature".to_string(),
        ));
    }
    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical cancel envelope requires cancel.deadline".to_string(),
            ));
        }
        current_timestamp_ms().saturating_add(3_600_000)
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

    Ok(Json(SubmitOrderResponse {
        status: "pending".to_string(),
        tx_hash: hex::encode(hash),
        message: None,
    }))
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64
}

/// Calculate the API's approximate initial margin without allowing signed
/// input arithmetic to overflow or wrap into a negative requirement.
///
/// Consensus execution performs the authoritative order validation. This is
/// only the route-level balance precheck, so it deliberately returns an error
/// for values outside the supported positive `i64` margin range.
fn calculate_required_margin(price: i64, size: i64) -> Result<i64, &'static str> {
    if price <= 0 || size <= 0 {
        return Err("price and size must be positive");
    }

    let notional = (price as i128)
        .checked_mul(size as i128)
        .ok_or("order notional overflow")?
        / 100_000_000;
    let required_margin = notional / 10;

    i64::try_from(required_margin).map_err(|_| "required margin exceeds supported range")
}

#[cfg(test)]
mod tests {
    use super::calculate_required_margin;

    #[test]
    fn margin_precheck_rejects_non_positive_inputs() {
        for (price, size) in [(0, 1), (1, 0), (-1, 1), (1, -1)] {
            assert_eq!(
                calculate_required_margin(price, size),
                Err("price and size must be positive")
            );
        }
    }

    #[test]
    fn margin_precheck_rejects_extreme_values_without_wrapping() {
        let result = std::panic::catch_unwind(|| calculate_required_margin(i64::MAX, i64::MAX));

        assert!(result.is_ok(), "extreme input must not panic");
        assert_eq!(
            result.unwrap(),
            Err("required margin exceeds supported range")
        );
    }

    #[test]
    fn margin_precheck_preserves_positive_margin_calculation() {
        assert_eq!(calculate_required_margin(100_000_000, 10), Ok(1));
    }
}
