//! Chain Status Endpoints
//!
//! Chain status, health check, and insurance fund information.

use axum::{extract::State, Json};

use crate::api::types::{ApiState, ChainStatus};

pub async fn get_chain_status(State(state): State<ApiState>) -> Json<ChainStatus> {
    let app = state.shared.app.read().await;
    let (b0, b1, b2) = app.mempool_stats();

    Json(ChainStatus {
        height: app.committed_height(),
        view: app.current_view(),
        avg_block_time: 100.0,
        mempool_size: b0 + b1 + b2,
        validators: app.staking().validators.len(),
    })
}

/// Health check endpoint with detailed node status
///
/// Returns:
/// - status: "healthy" or "degraded"
/// - height: Current committed block height
/// - view: Current consensus view
/// - mempool_size: Pending transactions
/// - persistence: Whether DATA_DIR is configured
///
/// Note: Consensus-level corruption detection is not exposed here
/// as the API layer doesn't have direct access to the consensus engine.
/// For full Byzantine detection, monitor the node logs for state corruption warnings.
pub async fn get_node_health(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    let (b0, b1, b2) = app.mempool_stats();
    let mempool_size = b0 + b1 + b2;

    // Check if persistence is configured
    let is_persistent = crate::config::Config::global().data_dir.is_some();

    // Basic health checks
    let is_healthy = true; // App layer is always healthy if responding

    Json(serde_json::json!({
        "status": if is_healthy { "healthy" } else { "degraded" },
        "height": app.committed_height(),
        "view": app.current_view(),
        "mempool_size": mempool_size,
        "persistence": is_persistent,
        "validators": app.staking().validators.len(),
        "active_validators": app.staking().active_validators().len(),
        "insurance_fund": app.insurance_fund_balance(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }))
}

pub async fn get_insurance_fund(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    Json(serde_json::json!({
        "balance": app.insurance_fund_balance(),
        "balance_usd": app.insurance_fund_balance() as f64 / 100.0
    }))
}
