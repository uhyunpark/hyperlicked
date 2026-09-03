//! Advanced Staking Tests
//!
//! Tests for ClaimRewards, Unjail, SubmitEvidence, and full epoch flow.

use hyperlicked::app::staking::{ValidatorStatus, MIN_SELF_STAKE};
use hyperlicked::app::Transaction;
use hyperlicked::types::ConsensusConfig;

use crate::e2e::helpers::*;

// =============================================================================
// ClaimRewards Tests
// =============================================================================

/// Test claiming validator rewards
#[test]
fn test_claim_validator_rewards() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Register validator
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let balance_after_register = ctx.hyck_balance(TRADER_ALICE);

    // Set pending rewards directly
    ctx.state
        .staking_mut()
        .validators
        .get_mut(&TRADER_ALICE.to_string())
        .unwrap()
        .pending_rewards = 50_000;

    // Claim rewards
    ctx.state
        .submit_tx(Transaction::ClaimRewards {
            claimant: TRADER_ALICE.into(),
            validator: None,
        })
        .unwrap();
    ctx.execute_block();

    // Balance should increase by reward amount
    assert_hyck_balance(&ctx.state, TRADER_ALICE, balance_after_register + 50_000);
}

/// Test claiming delegation rewards
#[test]
fn test_claim_delegation_rewards() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_BOB)
                .amount(DEFAULT_DEPOSIT)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Alice registers as validator
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Bob delegates
    let delegation_amount = 500_000;
    ctx.state
        .submit_tx(
            DelegateBuilder::new(TRADER_BOB, TRADER_ALICE)
                .amount(delegation_amount)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let balance_after_delegate = ctx.hyck_balance(TRADER_BOB);

    // Set delegation pending rewards directly
    let key = (TRADER_BOB.to_string(), TRADER_ALICE.to_string());
    ctx.state
        .staking_mut()
        .delegations
        .get_mut(&key)
        .unwrap()
        .pending_rewards = 25_000;

    // Claim delegation rewards
    ctx.state
        .submit_tx(Transaction::ClaimRewards {
            claimant: TRADER_BOB.into(),
            validator: Some(TRADER_ALICE.into()),
        })
        .unwrap();
    ctx.execute_block();

    assert_hyck_balance(&ctx.state, TRADER_BOB, balance_after_delegate + 25_000);
}

/// Test claiming with zero rewards leaves balance unchanged
#[test]
fn test_claim_rewards_zero() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let balance_before = ctx.hyck_balance(TRADER_ALICE);

    // Claim with 0 pending rewards (default)
    ctx.state
        .submit_tx(Transaction::ClaimRewards {
            claimant: TRADER_ALICE.into(),
            validator: None,
        })
        .unwrap();
    ctx.execute_block();

    assert_hyck_balance(&ctx.state, TRADER_ALICE, balance_before);
}

// =============================================================================
// Unjail Tests
// =============================================================================

/// Test unjailing after jail period expires
#[test]
fn test_unjail_after_expiry() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Jail the validator
    let jail_duration = 5000;
    let jail_time = ctx.timestamp();
    ctx.state
        .staking_mut()
        .manual_jail(TRADER_ALICE, jail_time, Some(jail_duration));

    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Jailed);

    // Fast-forward past jail expiry
    ctx.state
        .submit_tx(Transaction::Unjail {
            operator: TRADER_ALICE.into(),
        })
        .unwrap();
    ctx.execute_block_at(jail_time + jail_duration + 1);

    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Inactive);
}

/// Test unjailing too early fails
#[test]
fn test_unjail_too_early() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Jail with 1 hour duration
    let jail_time = ctx.timestamp();
    ctx.state
        .staking_mut()
        .manual_jail(TRADER_ALICE, jail_time, Some(3_600_000));

    // Try to unjail immediately (should fail in execution)
    ctx.state
        .submit_tx(Transaction::Unjail {
            operator: TRADER_ALICE.into(),
        })
        .unwrap();
    ctx.execute_block_at(jail_time + 1000); // Only 1 second later

    // Should still be jailed
    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Jailed);
}

/// Test unjailing a non-jailed validator fails
#[test]
fn test_unjail_not_jailed() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Validator is Pending, not Jailed - unjail should fail silently in execution
    ctx.state
        .submit_tx(Transaction::Unjail {
            operator: TRADER_ALICE.into(),
        })
        .unwrap();
    ctx.execute_block();

    // Status should remain Pending (unchanged)
    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Inactive);
}

// =============================================================================
// SubmitEvidence Tests
// =============================================================================

/// Test submitting valid equivocation evidence slashes and tombstones
#[test]
fn test_submit_evidence_valid_slashes() {
    let mut ctx = TestContext::new();

    let (sk, pk_bytes, node_id) = test_bls_keypair(1);
    let self_stake = MIN_SELF_STAKE + 100_000;
    let context = ConsensusConfig {
        epoch: 0,
        genesis_hash: [9u8; 32],
        node_id,
        validators: vec![node_id],
        voting_powers: vec![self_stake as u64],
        view_timeout_ms: 0,
        bls_pubkeys: vec![pk_bytes.clone()],
        bls_secret_key: None,
    }
    .context()
    .expect("registered validator must have a canonical context");

    // Fund and register validator with real BLS pubkey
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .with_node_id(node_id)
                .with_bls_pubkey(pk_bytes.clone())
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Create and submit valid evidence
    let evidence = create_valid_evidence(context, &sk, node_id, 100);
    ctx.state
        .submit_tx(Transaction::SubmitEvidence {
            submitter: format!("system:equivocation:{}", hex::encode(node_id)),
            evidence,
        })
        .unwrap();
    ctx.execute_block();

    // Validator should be tombstoned
    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Tombstoned);

    // Stake should be slashed (50%)
    let validator = ctx
        .state
        .staking()
        .get_validator(&TRADER_ALICE.to_string())
        .unwrap();
    assert!(
        validator.self_stake < self_stake,
        "Self-stake should be reduced after slashing"
    );
    let expected_slash = self_stake / 2; // 50% slash
    assert_eq!(validator.self_stake, self_stake - expected_slash);
}

/// Test submitting invalid evidence is rejected
#[test]
fn test_submit_evidence_invalid_rejected() {
    let mut ctx = TestContext::new();

    let (_sk, pk_bytes, node_id) = test_bls_keypair(1);
    let self_stake = MIN_SELF_STAKE + 100_000;
    let context = ConsensusConfig {
        epoch: 0,
        genesis_hash: [9u8; 32],
        node_id,
        validators: vec![node_id],
        voting_powers: vec![self_stake as u64],
        view_timeout_ms: 0,
        bls_pubkeys: vec![pk_bytes.clone()],
        bls_secret_key: None,
    }
    .context()
    .expect("registered validator must have a canonical context");

    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .with_node_id(node_id)
                .with_bls_pubkey(pk_bytes.clone())
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Submit evidence with forged signatures (wrong key)
    let (wrong_sk, _, _) = test_bls_keypair(99);
    let bad_evidence = create_valid_evidence(context, &wrong_sk, node_id, 100);

    let error = ctx
        .state
        .submit_tx(Transaction::SubmitEvidence {
            submitter: format!("system:equivocation:{}", hex::encode(node_id)),
            evidence: bad_evidence,
        })
        .expect_err("forged evidence must fail verified admission");
    assert!(error.to_string().contains("evidence"));

    // Validator should NOT be tombstoned - evidence was invalid
    assert_validator_status(&ctx.state, TRADER_ALICE, ValidatorStatus::Inactive);
}

// =============================================================================
// Full Epoch Flow Test
// =============================================================================

/// Staking rewards must move already-issued HYCK out of the funded reserve.
#[test]
fn test_block_reward_spends_reserve_without_changing_fixed_supply() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Register validator
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let liquid_before = ctx.state.hyck_liquid_supply().unwrap();
    // Transition epoch to activate (epoch 0 → 1)
    let ts = ctx.timestamp();
    ctx.state.staking_mut().transition_epoch(0, ts);
    let reserve_before = ctx.state.staking().emissions_reserve;

    // Registration occurred after the reward clock started, so the first
    // boundary establishes eligible stake and the second pays it. Advance in
    // 30-second steps to preserve the parent-relative timestamp rule.
    let steps = hyperlicked::app::staking::STAKING_REWARD_EPOCH_MS / 30_000 * 2;
    for _ in 0..steps {
        ctx.execute_block_at(ctx.timestamp() + 30_000);
    }

    let pending = ctx
        .state
        .staking()
        .validator_pending_rewards(&TRADER_ALICE.to_string());
    assert!(pending > 0, "an active validator should receive a reward");
    assert_eq!(
        reserve_before - ctx.state.staking().emissions_reserve,
        pending,
        "every rewarded base unit must come from the emissions reserve"
    );
    assert_eq!(ctx.state.hyck_liquid_supply().unwrap(), liquid_before);
    ctx.state.validate_hyck_supply().unwrap();
}
