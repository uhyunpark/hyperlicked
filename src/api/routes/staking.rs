//! Staking Endpoints
//!
//! Validator and delegation information.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::types::ApiState;

/// Validator info for API response
#[derive(serde::Serialize)]
pub struct ValidatorResponse {
    operator: String,
    node_id: String,
    self_stake: i64,
    self_stake_usd: f64,
    total_stake: i64,
    total_stake_usd: f64,
    commission_bps: i64,
    status: String,
    pending_rewards: i64,
}

pub async fn get_validators(State(state): State<ApiState>) -> Json<Vec<ValidatorResponse>> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let validators: Vec<ValidatorResponse> = staking
        .validators
        .values()
        .map(|v| ValidatorResponse {
            operator: v.operator.clone(),
            node_id: hex::encode(v.node_id),
            self_stake: v.self_stake,
            self_stake_usd: v.self_stake as f64 / 100.0,
            total_stake: v.total_stake,
            total_stake_usd: v.total_stake as f64 / 100.0,
            commission_bps: v.commission_bps,
            status: format!("{:?}", v.status),
            pending_rewards: v.pending_rewards,
        })
        .collect();

    Json(validators)
}

pub async fn get_validator(
    State(state): State<ApiState>,
    Path(operator): Path<String>,
) -> Result<Json<ValidatorResponse>, StatusCode> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let v = staking
        .validators
        .get(&operator)
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ValidatorResponse {
        operator: v.operator.clone(),
        node_id: hex::encode(v.node_id),
        self_stake: v.self_stake,
        self_stake_usd: v.self_stake as f64 / 100.0,
        total_stake: v.total_stake,
        total_stake_usd: v.total_stake as f64 / 100.0,
        commission_bps: v.commission_bps,
        status: format!("{:?}", v.status),
        pending_rewards: v.pending_rewards,
    }))
}

/// Delegation info for API response
#[derive(serde::Serialize)]
pub struct DelegationResponse {
    delegator: String,
    validator: String,
    amount: i64,
    amount_usd: f64,
    pending_rewards: i64,
}

pub async fn get_delegations(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<DelegationResponse>> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let delegations: Vec<DelegationResponse> = staking
        .delegations_for(&address)
        .into_iter()
        .map(|d| DelegationResponse {
            delegator: d.delegator.clone(),
            validator: d.validator.clone(),
            amount: d.amount,
            amount_usd: d.amount as f64 / 100.0,
            pending_rewards: d.pending_rewards,
        })
        .collect();

    Json(delegations)
}

/// Epoch info for API response
#[derive(serde::Serialize)]
pub struct EpochResponse {
    current_epoch: u64,
    current_view: u64,
    active_validators: usize,
    total_staked: i64,
    total_staked_usd: f64,
    rounds_per_epoch: u64,
}

pub async fn get_epoch(State(state): State<ApiState>) -> Json<EpochResponse> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    Json(EpochResponse {
        current_epoch: staking.current_epoch,
        current_view: app.current_view(),
        active_validators: staking.active_validators().len(),
        total_staked: staking.total_staked,
        total_staked_usd: staking.total_staked as f64 / 100.0,
        rounds_per_epoch: crate::app::staking::ROUNDS_PER_EPOCH,
    })
}
