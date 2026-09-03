//! Oracle Endpoints
//!
//! Oracle price feed information.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::types::ApiState;
use crate::app::{OraclePrice, PriceSource};

/// Oracle price response
#[derive(serde::Serialize)]
pub struct OraclePriceResponse {
    pub symbol: String,
    pub price: i64,
    pub price_usd: f64,
    pub timestamp: u64,
    pub source_count: u32,
    pub confidence_bps: i64,
    pub is_stale: bool,
    pub enabled: bool,
}

impl From<&OraclePrice> for OraclePriceResponse {
    fn from(p: &OraclePrice) -> Self {
        Self {
            symbol: p.symbol.clone(),
            price: p.price,
            price_usd: p.price as f64 / 100.0,
            timestamp: p.timestamp,
            source_count: p.source_count,
            confidence_bps: p.confidence_bps,
            is_stale: false, // Will be set by caller
            enabled: true,   // Will be set by caller
        }
    }
}

/// Price source response
#[derive(serde::Serialize)]
pub struct PriceSourceResponse {
    pub source_id: String,
    pub price: i64,
    pub price_usd: f64,
    pub timestamp: u64,
    pub weight_bps: i64,
}

impl From<&PriceSource> for PriceSourceResponse {
    fn from(s: &PriceSource) -> Self {
        Self {
            source_id: s.source_id.clone(),
            price: s.price,
            price_usd: s.price as f64 / 100.0,
            timestamp: s.timestamp,
            weight_bps: s.weight_bps,
        }
    }
}

/// Get aggregated oracle price for a symbol
pub async fn get_oracle_price(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<OraclePriceResponse>, StatusCode> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let oracle = app.oracle();

    if !oracle.enabled {
        // Return mark price in bootstrap mode
        let mark = app.mark_price(&symbol).ok_or(StatusCode::NOT_FOUND)?;
        return Ok(Json(OraclePriceResponse {
            symbol: symbol.clone(),
            price: mark,
            price_usd: mark as f64 / 100.0,
            timestamp: app.timestamp,
            source_count: 0,
            confidence_bps: 0,
            is_stale: false,
            enabled: false,
        }));
    }

    let oracle_price = oracle.prices.get(&symbol).ok_or(StatusCode::NOT_FOUND)?;
    let is_stale = oracle.is_stale(&symbol, app.timestamp);

    Ok(Json(OraclePriceResponse {
        symbol: oracle_price.symbol.clone(),
        price: oracle_price.price,
        price_usd: oracle_price.price as f64 / 100.0,
        timestamp: oracle_price.timestamp,
        source_count: oracle_price.source_count,
        confidence_bps: oracle_price.confidence_bps,
        is_stale,
        enabled: true,
    }))
}

/// Get individual source prices for a symbol
pub async fn get_oracle_sources(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<Vec<PriceSourceResponse>>, StatusCode> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let oracle = app.oracle();

    let sources = oracle
        .source_prices
        .get(&symbol)
        .ok_or(StatusCode::NOT_FOUND)?;

    let response: Vec<PriceSourceResponse> =
        sources.iter().map(PriceSourceResponse::from).collect();

    Ok(Json(response))
}

/// Oracle status response
#[derive(serde::Serialize)]
pub struct OracleStatusResponse {
    pub enabled: bool,
    pub symbols_count: usize,
    pub symbols: Vec<String>,
}

/// Get oracle system status
pub async fn get_oracle_status(State(state): State<ApiState>) -> Json<OracleStatusResponse> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let oracle = app.oracle();

    let symbols: Vec<String> = oracle.prices.keys().cloned().collect();

    Json(OracleStatusResponse {
        enabled: oracle.enabled,
        symbols_count: symbols.len(),
        symbols,
    })
}

/// Oracle price update request
#[derive(serde::Deserialize)]
#[allow(dead_code)]
pub struct OracleUpdateRequest {
    pub operator: String,
    pub symbol: String,
    pub sources: Vec<PriceSourceInput>,
    pub signature: String,
}

/// Price source input
#[derive(serde::Deserialize)]
#[allow(dead_code)]
pub struct PriceSourceInput {
    pub source_id: String,
    pub price: i64,
    pub timestamp: u64,
    pub weight_bps: i64,
}

/// Submit oracle price update (for operators)
pub async fn submit_oracle_update(
    State(state): State<ApiState>,
    Json(req): Json<OracleUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    // Oracle updates currently use an operator BLS signature embedded in the
    // legacy Transaction.  That object is not a canonical chain-domain
    // envelope and would be accepted by only the node receiving this request.
    // Keep the endpoint explicitly disabled until the BLS envelope scheme is
    // wired through consensus; never enqueue it as an unsigned System tx.
    let _ = (state, req);
    Err((
        StatusCode::NOT_IMPLEMENTED,
        "oracle submission is disabled until canonical BLS transaction envelopes are enabled"
            .to_string(),
    ))
}

/// Request to enable/disable oracle.
#[derive(serde::Deserialize)]
#[allow(dead_code)]
pub struct SetOracleEnabledRequest {
    pub enabled: bool,
}

/// Enable or disable oracle.
///
/// Oracle enablement is consensus state.  There is intentionally no direct
/// API mutation path: a future governance/system transaction must carry the
/// change in a block so every validator applies it in the same order.
pub async fn set_oracle_enabled(
    State(state): State<ApiState>,
    Json(req): Json<SetOracleEnabledRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let _ = (state, req);
    Err(StatusCode::FORBIDDEN)
}
