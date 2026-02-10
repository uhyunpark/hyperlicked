//! Admin Endpoints
//!
//! Administrative operations like adding markets.

use axum::{extract::State, http::StatusCode, Json};

use crate::api::types::{AddMarketRequest, AddMarketResponse, ApiState};
use crate::api::verify::verify_add_market;
use crate::app::{MarketConfig, Transaction};

pub async fn add_market(
    State(state): State<ApiState>,
    Json(req): Json<AddMarketRequest>,
) -> Result<Json<AddMarketResponse>, (StatusCode, String)> {
    // Verify signature
    let verified = verify_add_market(&req, &state.eip712_signer).map_err(|e| {
        (StatusCode::UNAUTHORIZED, format!("verification failed: {}", e))
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

    // Submit to mempool
    let mut app = state.shared.app.write().await;
    app.submit_tx(tx).map_err(|e| {
        (StatusCode::BAD_REQUEST, format!("submit failed: {}", e))
    })?;

    Ok(Json(AddMarketResponse {
        status: "ok".to_string(),
        symbol,
    }))
}
