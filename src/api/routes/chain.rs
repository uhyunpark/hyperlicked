//! Chain Status Endpoints
//!
//! Chain status and insurance fund information.

use axum::{extract::State, Json};

use crate::api::types::{ApiState, ChainStatus};

pub async fn get_chain_status(State(state): State<ApiState>) -> Json<ChainStatus> {
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

pub async fn get_insurance_fund(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    Json(serde_json::json!({
        "balance": app.insurance_fund_balance(),
        "balance_usd": app.insurance_fund_balance() as f64 / 100.0
    }))
}
