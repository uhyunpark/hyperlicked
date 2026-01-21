//! ADL (Auto-Deleveraging) Endpoints
//!
//! Query ADL events. Note: ADL events are currently only available via
//! WebSocket notifications. This endpoint provides placeholder for future
//! persistent ADL history.

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::api::types::ApiState;

/// ADL event info for API response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ADLEventInfo {
    pub address: String,
    pub symbol: String,
    pub size_reduced: i64,
    pub close_price: i64,
    pub realized_pnl: i64,
    pub triggering_liquidation: String,
    pub timestamp: u64,
}

/// Get ADL history for an address
///
/// Note: Currently returns empty as ADL events are only available via WebSocket.
/// A persistent storage solution would be needed for proper history.
pub async fn get_adl_history(
    State(_state): State<ApiState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Vec<ADLEventInfo>> {
    // Extract optional query params (for future use)
    let _address = params.get("address");
    let _symbol = params.get("symbol");
    let _limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100);

    // Currently, ADL history requires persistent storage implementation.
    // ADL events are broadcast via WebSocket in real-time.
    // Return empty array for now.
    Json(vec![])
}
