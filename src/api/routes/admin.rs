//! Admin Endpoints
//!
//! Administrative operations like adding markets.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{extract::State, http::StatusCode, Json};

use crate::api::types::{AddMarketRequest, AddMarketResponse, ApiState};
use crate::api::verify::{uses_canonical_signature, verify_add_market};
use crate::app::{MarketConfig, SignatureScheme, SignedEnvelope, Transaction};

pub async fn add_market(
    State(state): State<ApiState>,
    Json(req): Json<AddMarketRequest>,
) -> Result<Json<AddMarketResponse>, (StatusCode, String)> {
    // Verify signature
    let verified =
        verify_add_market(&req, &state.eip712_signer, &state.security_policy).map_err(|e| {
            (
                StatusCode::UNAUTHORIZED,
                format!("verification failed: {}", e),
            )
        })?;

    let symbol = verified.symbol.clone();

    // Build MarketConfig with defaults for fields not in the EIP-712 signature
    let config = MarketConfig {
        symbol: verified.symbol,
        tick_size: verified.tick_size,
        lot_size: verified.lot_size,
        min_notional: verified.min_notional,
        maker_fee: verified.maker_fee,
        taker_fee: verified.taker_fee,
        ..MarketConfig::default()
    };

    // Build transaction
    let tx = Transaction::AddMarket {
        admin: format!("{:?}", verified.owner),
        config,
        initial_mark_price: verified.initial_mark_price,
    };

    let canonical_signature = uses_canonical_signature(req.signature_scheme.as_deref());
    if !canonical_signature && !state.security_policy.skip_signature_verification {
        return Err((
            StatusCode::BAD_REQUEST,
            "legacy EIP-712 admin signature cannot authenticate the canonical consensus envelope; use signatureScheme=eip712-v1 and typed-data deadline".to_string(),
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_millis() as u64;
    let valid_until = if verified.valid_until == 0 {
        if canonical_signature {
            return Err((
                StatusCode::BAD_REQUEST,
                "canonical admin envelope requires deadline".to_string(),
            ));
        }
        now.saturating_add(3_600_000)
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

    // Submit canonical envelope to mempool and publish after local admission.
    state
        .submit_user_envelope(envelope, now)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("submit failed: {}", e)))?;

    Ok(Json(AddMarketResponse {
        status: "ok".to_string(),
        symbol,
    }))
}
