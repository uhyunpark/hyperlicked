//! API Handlers
//!
//! Secondary request handlers extracted from routes.rs.

use alloy_primitives::{Address, U256};
use axum::{extract::State, http::StatusCode, Json};

use crate::app::{OrderType, Side, Transaction};
use crate::crypto::AgentDelegation;

use super::types::{
    ApiState, DepositRequest, LegacyOrderRequest, RegisterDelegationRequest,
    RegisterDelegationResponse, StoredDelegation, WithdrawRequest,
};

// =============================================================================
// Agent Delegation
// =============================================================================

pub async fn register_delegation(
    State(state): State<ApiState>,
    Json(req): Json<RegisterDelegationRequest>,
) -> Result<Json<RegisterDelegationResponse>, (StatusCode, String)> {
    // Parse addresses
    let wallet: Address = req.wallet.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Invalid wallet address".to_string(),
        )
    })?;
    let agent: Address = req
        .agent
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid agent address".to_string()))?;

    // Parse expiration and nonce
    let expiration: u64 = req
        .expiration
        .parse()
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid expiration".to_string()))?;
    let nonce: U256 = U256::from_str_radix(&req.nonce, 10)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid nonce".to_string()))?;

    // Decode signature
    let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
    let signature = hex::decode(sig_hex)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Invalid signature hex".to_string()))?;

    // Create delegation
    let delegation = AgentDelegation {
        wallet,
        agent,
        expiration,
        nonce,
    };

    // Verify signature
    let valid = state
        .agent_signer
        .verify_delegation(&delegation, &signature)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    if !valid {
        return Err((
            StatusCode::BAD_REQUEST,
            "Invalid delegation signature".to_string(),
        ));
    }

    // Store delegation
    let delegation_id = format!("{}-{}", req.wallet, req.nonce);
    let stored = StoredDelegation {
        delegation,
        signature,
    };

    state
        .delegations
        .write()
        .await
        .insert(delegation_id.clone(), stored);

    Ok(Json(RegisterDelegationResponse {
        status: "registered".to_string(),
        delegation_id,
        message: "Agent key delegation registered successfully".to_string(),
    }))
}

// =============================================================================
// Legacy Endpoints
// =============================================================================

pub async fn submit_order_legacy(
    State(state): State<ApiState>,
    Json(req): Json<LegacyOrderRequest>,
) -> Json<serde_json::Value> {
    let side = match req.side.to_lowercase().as_str() {
        "buy" | "bid" => Side::Bid,
        _ => Side::Ask,
    };

    let order_type = match req.order_type.to_lowercase().as_str() {
        "ioc" => OrderType::Ioc,
        "alo" => OrderType::Alo,
        _ => OrderType::Gtc,
    };

    let tx = Transaction::PlaceOrder {
        trader: req.trader,
        symbol: req.symbol,
        side,
        price: req.price,
        size: req.size,
        order_type,
        reduce_only: false,
    };

    let mut app = state
        .shared
        .app
        .write()
        .expect("application state lock poisoned");
    match app.submit_tx(tx) {
        Ok(hash) => Json(serde_json::json!({
            "success": true,
            "tx_hash": hex::encode(hash)
        })),
        Err(e) => Json(serde_json::json!({
            "success": false,
            "error": e.to_string()
        })),
    }
}

pub async fn deposit(
    State(state): State<ApiState>,
    Json(req): Json<DepositRequest>,
) -> Json<serde_json::Value> {
    let tx = Transaction::Deposit {
        trader: req.trader,
        amount: req.amount,
    };

    let mut app = state
        .shared
        .app
        .write()
        .expect("application state lock poisoned");
    match app.submit_tx(tx) {
        Ok(_) => Json(serde_json::json!({"success": true})),
        Err(e) => Json(serde_json::json!({"success": false, "error": e.to_string()})),
    }
}

pub async fn withdraw(
    State(state): State<ApiState>,
    Json(req): Json<WithdrawRequest>,
) -> Json<serde_json::Value> {
    let tx = Transaction::Withdraw {
        trader: req.trader,
        amount: req.amount,
    };

    let mut app = state
        .shared
        .app
        .write()
        .expect("application state lock poisoned");
    match app.submit_tx(tx) {
        Ok(_) => Json(serde_json::json!({"success": true})),
        Err(e) => Json(serde_json::json!({"success": false, "error": e.to_string()})),
    }
}
