//! Staking Endpoints
//!
//! Validator and delegation information.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::types::ApiState;
use crate::app::staking::HYCK_BASE_UNITS_PER_HYCK;

fn hyck_from_base_units(amount: i64) -> f64 {
    amount as f64 / HYCK_BASE_UNITS_PER_HYCK as f64
}

/// Validator info for API response
#[derive(serde::Serialize)]
pub struct ValidatorResponse {
    operator: String,
    node_id: String,
    self_stake: i64,
    self_stake_hyck: f64,
    total_stake: i64,
    total_stake_hyck: f64,
    commission_bps: i64,
    status: String,
    pending_rewards: i64,
}

pub async fn get_validators(State(state): State<ApiState>) -> Json<Vec<ValidatorResponse>> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let staking = app.staking();

    let validators: Vec<ValidatorResponse> = staking
        .validators
        .values()
        .map(|v| ValidatorResponse {
            operator: v.operator.clone(),
            node_id: hex::encode(v.node_id),
            self_stake: v.self_stake,
            self_stake_hyck: hyck_from_base_units(v.self_stake),
            total_stake: v.total_stake,
            total_stake_hyck: hyck_from_base_units(v.total_stake),
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
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let staking = app.staking();

    let v = staking
        .validators
        .get(&operator)
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok(Json(ValidatorResponse {
        operator: v.operator.clone(),
        node_id: hex::encode(v.node_id),
        self_stake: v.self_stake,
        self_stake_hyck: hyck_from_base_units(v.self_stake),
        total_stake: v.total_stake,
        total_stake_hyck: hyck_from_base_units(v.total_stake),
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
    amount_hyck: f64,
    pending_rewards: i64,
}

pub async fn get_delegations(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<DelegationResponse>> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let staking = app.staking();

    let delegations: Vec<DelegationResponse> = staking
        .delegations_for(&address)
        .into_iter()
        .map(|d| DelegationResponse {
            delegator: d.delegator.clone(),
            validator: d.validator.clone(),
            amount: d.amount,
            amount_hyck: hyck_from_base_units(d.amount),
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
    total_staked_hyck: f64,
    rounds_per_epoch: u64,
}

pub async fn get_epoch(State(state): State<ApiState>) -> Json<EpochResponse> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let staking = app.staking();

    Json(EpochResponse {
        current_epoch: staking.current_epoch,
        current_view: app.current_view(),
        active_validators: staking.active_validators().len(),
        total_staked: staking.total_staked,
        total_staked_hyck: hyck_from_base_units(staking.total_staked),
        rounds_per_epoch: crate::app::staking::ROUNDS_PER_EPOCH,
    })
}

/// Pending unstake info for API response
#[derive(serde::Serialize)]
pub struct PendingUnstakeResponse {
    /// Validator address (None if unstaking self-stake)
    validator: Option<String>,
    /// Amount being unstaked (HYCK base units)
    amount: i64,
    /// Amount in HYCK
    amount_hyck: f64,
    /// Time when unstake completes (ms timestamp)
    completion_time: u64,
    /// Time remaining until completion (ms), 0 if ready to claim
    time_remaining_ms: u64,
}

pub async fn get_pending_unstakes(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<PendingUnstakeResponse>> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
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
                amount_hyck: hyck_from_base_units(r.amount),
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
    total_staked_hyck: f64,
    /// Total amount in unbonding period
    total_unbonding: i64,
    total_unbonding_hyck: f64,
    /// Number of active delegations
    delegation_count: usize,
    /// Number of pending unstakes
    pending_unstake_count: usize,
}

pub async fn get_staking_summary(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<StakingSummaryResponse> {
    let app = state
        .shared
        .app
        .read()
        .expect("application state lock poisoned");
    let staking = app.staking();

    let delegations = staking.delegations_for(&address);
    let total_staked: i64 = delegations.iter().map(|d| d.amount).sum();
    let total_unbonding = staking.total_unbonding(&address);
    let pending_unstakes = staking.get_pending_unstakes(&address);

    Json(StakingSummaryResponse {
        total_staked,
        total_staked_hyck: hyck_from_base_units(total_staked),
        total_unbonding,
        total_unbonding_hyck: hyck_from_base_units(total_unbonding),
        delegation_count: delegations.len(),
        pending_unstake_count: pending_unstakes.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staking_responses_serialize_hyck_units_without_usd_fields() {
        let validator = ValidatorResponse {
            operator: "validator".into(),
            node_id: "node".into(),
            self_stake: 1_500_000,
            self_stake_hyck: hyck_from_base_units(1_500_000),
            total_stake: 2_750_000,
            total_stake_hyck: hyck_from_base_units(2_750_000),
            commission_bps: 500,
            status: "Active".into(),
            pending_rewards: 0,
        };
        let summary = StakingSummaryResponse {
            total_staked: 3_250_000,
            total_staked_hyck: hyck_from_base_units(3_250_000),
            total_unbonding: 750_000,
            total_unbonding_hyck: hyck_from_base_units(750_000),
            delegation_count: 1,
            pending_unstake_count: 1,
        };
        let delegation = DelegationResponse {
            delegator: "delegator".into(),
            validator: "validator".into(),
            amount: 1_250_000,
            amount_hyck: hyck_from_base_units(1_250_000),
            pending_rewards: 0,
        };
        let pending_unstake = PendingUnstakeResponse {
            validator: Some("validator".into()),
            amount: 1_250_000,
            amount_hyck: hyck_from_base_units(1_250_000),
            completion_time: 10,
            time_remaining_ms: 5,
        };

        let validator_json = serde_json::to_value(validator).unwrap();
        let summary_json = serde_json::to_value(summary).unwrap();
        let delegation_json = serde_json::to_value(delegation).unwrap();
        let pending_unstake_json = serde_json::to_value(pending_unstake).unwrap();

        assert_eq!(validator_json["self_stake_hyck"], 1.5);
        assert_eq!(validator_json["total_stake_hyck"], 2.75);
        assert!(validator_json.get("self_stake_usd").is_none());
        assert!(validator_json.get("total_stake_usd").is_none());
        assert_eq!(summary_json["total_staked_hyck"], 3.25);
        assert_eq!(summary_json["total_unbonding_hyck"], 0.75);
        assert!(summary_json.get("total_staked_usd").is_none());
        assert!(summary_json.get("total_unbonding_usd").is_none());
        assert_eq!(delegation_json["amount_hyck"], 1.25);
        assert!(delegation_json.get("amount_usd").is_none());
        assert_eq!(pending_unstake_json["amount_hyck"], 1.25);
        assert!(pending_unstake_json.get("amount_usd").is_none());
    }
}
