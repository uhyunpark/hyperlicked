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

/// Pending unstake info for API response
#[derive(serde::Serialize)]
pub struct PendingUnstakeResponse {
    /// Validator address (None if unstaking self-stake)
    validator: Option<String>,
    /// Amount being unstaked (cents)
    amount: i64,
    /// Amount in USD
    amount_usd: f64,
    /// Time when unstake completes (ms timestamp)
    completion_time: u64,
    /// Time remaining until completion (ms), 0 if ready to claim
    time_remaining_ms: u64,
}

pub async fn get_pending_unstakes(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<PendingUnstakeResponse>> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    // Use current system time for display purposes
    let current_time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let unstakes: Vec<PendingUnstakeResponse> = staking
        .get_pending_unstakes(&address)
        .into_iter()
        .map(|r| {
            let time_remaining = r.completion_time.saturating_sub(current_time);
            PendingUnstakeResponse {
                validator: r.validator.clone(),
                amount: r.amount,
                amount_usd: r.amount as f64 / 100.0,
                completion_time: r.completion_time,
                time_remaining_ms: time_remaining,
            }
        })
        .collect();

    Json(unstakes)
}

/// Summary of staking info for a delegator
#[derive(serde::Serialize)]
pub struct StakingSummaryResponse {
    /// Total staked across all validators
    total_staked: i64,
    total_staked_usd: f64,
    /// Total amount in unbonding period
    total_unbonding: i64,
    total_unbonding_usd: f64,
    /// Number of active delegations
    delegation_count: usize,
    /// Number of pending unstakes
    pending_unstake_count: usize,
}

pub async fn get_staking_summary(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<StakingSummaryResponse> {
    let app = state.shared.app.read().await;
    let staking = app.staking();

    let delegations = staking.delegations_for(&address);
    let total_staked: i64 = delegations.iter().map(|d| d.amount).sum();
    let total_unbonding = staking.total_unbonding(&address);
    let pending_unstakes = staking.get_pending_unstakes(&address);

    Json(StakingSummaryResponse {
        total_staked,
        total_staked_usd: total_staked as f64 / 100.0,
        total_unbonding,
        total_unbonding_usd: total_unbonding as f64 / 100.0,
        delegation_count: delegations.len(),
        pending_unstake_count: pending_unstakes.len(),
    })
}
