//! Oracle Endpoints
//!
//! Oracle price feed information.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::types::ApiState;
use crate::app::{OraclePrice, PriceSource, Transaction};

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
    let app = state.shared.app.read().await;
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
    let app = state.shared.app.read().await;
    let oracle = app.oracle();

    let sources = oracle
        .source_prices
        .get(&symbol)
        .ok_or(StatusCode::NOT_FOUND)?;

    let response: Vec<PriceSourceResponse> = sources.iter().map(PriceSourceResponse::from).collect();

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
    let app = state.shared.app.read().await;
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
pub struct OracleUpdateRequest {
    pub operator: String,
    pub symbol: String,
    pub sources: Vec<PriceSourceInput>,
    pub signature: String,
}

/// Price source input
#[derive(serde::Deserialize)]
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
    let sources: Vec<PriceSource> = req
        .sources
        .into_iter()
        .map(|s| PriceSource {
            source_id: s.source_id,
            price: s.price,
            timestamp: s.timestamp,
            weight_bps: s.weight_bps,
        })
        .collect();

    let signature = hex::decode(&req.signature).unwrap_or_default();

    let tx = Transaction::OraclePriceUpdate {
        operator: req.operator,
        symbol: req.symbol,
        sources,
        signature,
    };

    // Submit to mempool
    let mut app = state.shared.app.write().await;
    match app.submit_tx(tx) {
        Ok(hash) => Ok(Json(serde_json::json!({
            "status": "submitted",
            "hash": hex::encode(hash)
        }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, e.to_string())),
    }
}
