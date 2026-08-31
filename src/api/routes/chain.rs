//! Chain Status Endpoints
//!
//! Chain status, health check, and insurance fund information.

use axum::{extract::State, Json};

use crate::api::types::{ApiState, ChainStatus};

pub async fn get_chain_status(State(state): State<ApiState>) -> Json<ChainStatus> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
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
/// - status: "healthy", "degraded", or "corrupted"
/// - height: Current committed block height
/// - view: Current consensus view
/// - mempool_size: Pending transactions
/// - persistence: Whether the canonical API has a durable store attached
/// - state_corrupted: Whether Byzantine detection triggered (app_hash mismatch)
///
/// When state_corrupted is true, operator intervention is required.
/// The node detected an app_hash mismatch after a valid QC, indicating either:
/// 1. This node's state is corrupt and needs resync from a trusted snapshot
/// 2. The validator network is Byzantine (2f+1 colluding)
pub async fn get_node_health(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let (b0, b1, b2) = app.mempool_stats();
    let mempool_size = b0 + b1 + b2;

    // The canonical runtime injects its RocksDB handle into ApiState.  Read
    // that source of truth instead of the process-global DATA_DIR setting;
    // callers may explicitly pass --data-dir without setting the env var.
    let is_persistent = state.store.is_some();

    // Check if state is corrupted (Byzantine detection)
    let state_corrupted = state.shared.is_state_corrupted();

    // Health status: corrupted > degraded > healthy
    let status = if state_corrupted {
        "corrupted"
    } else {
        "healthy"
    };

    Json(serde_json::json!({
        "status": status,
        "height": app.committed_height(),
        "view": app.current_view(),
        "mempool_size": mempool_size,
        "persistence": is_persistent,
        "state_corrupted": state_corrupted,
        "validators": app.staking().validators.len(),
        "active_validators": app.staking().active_validators().len(),
        "insurance_fund": app.insurance_fund_balance(),
        "timestamp": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::SharedState;
    use crate::app::AppState;
    use crate::storage::RocksDbStore;
    use axum::extract::State;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn health_reports_persistence_from_api_store() {
        let shared = SharedState::new(AppState::new());
        let without_store = get_node_health(State(ApiState::new(shared.clone()))).await;
        assert_eq!(without_store.0["persistence"], false);

        let dir = TempDir::new().unwrap();
        let store = Arc::new(RocksDbStore::open(dir.path()).unwrap());
        let with_store = get_node_health(State(ApiState::with_store(shared, store))).await;
        assert_eq!(with_store.0["persistence"], true);
    }
}

pub async fn get_insurance_fund(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    Json(serde_json::json!({
        "balance": app.insurance_fund_balance(),
        "balance_usd": app.insurance_fund_balance() as f64 / 100.0
    }))
}
