//! Staking Tests
//!
//! Tests for validator registration, delegation, and undelegation.

use hyperlicked::app::{staking::MIN_SELF_STAKE, Transaction};

use crate::e2e::helpers::*;

/// Test validator registration succeeds with valid parameters
#[test]
fn test_register_validator() {
    let mut ctx = TestContext::new();

    // Operator needs enough balance for self-stake
    let self_stake = MIN_SELF_STAKE + 100_000; // Min + extra

    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(self_stake * 2)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Register as validator
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .with_commission(500) // 5%
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify validator exists
    assert_validator_exists(&ctx.state, TRADER_ALICE);

    // Balance should be reduced by self-stake
    let expected_balance = self_stake * 2 - self_stake;
    assert_hyck_balance(&ctx.state, TRADER_ALICE, expected_balance);
}

/// Test delegation to validator increases stake
#[test]
fn test_delegate_to_validator() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    let delegation_amount = 500_000; // $5,000

    // Fund both accounts
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

    // Record initial stake
    let alice_addr = TRADER_ALICE.to_string();
    let initial_stake = ctx
        .state
        .staking()
        .get_validator(&alice_addr)
        .unwrap()
        .total_stake;

    // Bob delegates to Alice
    ctx.state
        .submit_tx(
            DelegateBuilder::new(TRADER_BOB, TRADER_ALICE)
                .amount(delegation_amount)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify stake increased
    let new_stake = ctx
        .state
        .staking()
        .get_validator(&alice_addr)
        .unwrap()
        .total_stake;
    assert_eq!(
        new_stake,
        initial_stake + delegation_amount,
        "Validator stake should increase by delegation amount"
    );

    // Bob's balance should decrease
    assert_hyck_balance(&ctx.state, TRADER_BOB, DEFAULT_DEPOSIT - delegation_amount);
}

/// Test undelegation starts unbonding queue
#[test]
fn test_undelegate_unbonding() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    let delegation_amount = 500_000;

    // Setup
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

    // Alice registers
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Bob delegates
    ctx.state
        .submit_tx(
            DelegateBuilder::new(TRADER_BOB, TRADER_ALICE)
                .amount(delegation_amount)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let balance_before_undelegate = ctx.hyck_balance(TRADER_BOB);

    // Bob undelegates half
    let undelegate_amount = delegation_amount / 2;
    ctx.state
        .submit_tx(Transaction::Undelegate {
            delegator: TRADER_BOB.into(),
            validator: TRADER_ALICE.into(),
            amount: undelegate_amount,
        })
        .unwrap();
    ctx.execute_block();

    // Validator stake should decrease
    let alice_addr = TRADER_ALICE.to_string();
    let validator = ctx.state.staking().get_validator(&alice_addr).unwrap();
    assert_eq!(
        validator.total_stake,
        self_stake + delegation_amount - undelegate_amount,
        "Validator stake should decrease after undelegation"
    );

    // Bob's balance should NOT increase yet (in unbonding)
    assert_hyck_balance(&ctx.state, TRADER_BOB, balance_before_undelegate);
}

/// Test insufficient stake fails validator registration
#[test]
fn test_register_fails_insufficient_stake() {
    let mut ctx = TestContext::new();

    let insufficient_stake = MIN_SELF_STAKE - 1;

    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_ALICE)
                .amount(DEFAULT_DEPOSIT)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Try to register with insufficient stake
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(insufficient_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Validator should NOT exist (registration failed)
    let alice_addr = TRADER_ALICE.to_string();
    let validator = ctx.state.staking().get_validator(&alice_addr);
    assert!(
        validator.is_none(),
        "Validator should not exist with insufficient stake"
    );
}

/// Test delegation to non-existent validator fails
#[test]
fn test_delegate_to_nonexistent_validator_fails() {
    let mut ctx = TestContext::new();

    ctx.state
        .submit_tx(
            NativeHyckTransferBuilder::new(TRADER_BOB)
                .amount(DEFAULT_DEPOSIT)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    let initial_balance = ctx.hyck_balance(TRADER_BOB);

    // Try to delegate to non-existent validator
    ctx.state
        .submit_tx(
            DelegateBuilder::new(TRADER_BOB, "nonexistent_validator")
                .amount(500_000)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Balance should remain unchanged (delegation failed)
    assert_hyck_balance(&ctx.state, TRADER_BOB, initial_balance);
}

/// Test claim unstaked after unbonding period
#[test]
fn test_claim_unstaked() {
    let mut ctx = TestContext::new();

    let self_stake = MIN_SELF_STAKE + 100_000;
    let delegation_amount = 500_000;

    // Setup
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

    // Alice registers
    ctx.state
        .submit_tx(
            ValidatorBuilder::new(TRADER_ALICE)
                .with_stake(self_stake)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Bob delegates
    ctx.state
        .submit_tx(
            DelegateBuilder::new(TRADER_BOB, TRADER_ALICE)
                .amount(delegation_amount)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Bob undelegates all
    ctx.state
        .submit_tx(Transaction::Undelegate {
            delegator: TRADER_BOB.into(),
            validator: TRADER_ALICE.into(),
            amount: delegation_amount,
        })
        .unwrap();
    ctx.execute_block();

    let balance_after_undelegate = ctx.hyck_balance(TRADER_BOB);

    // Fast forward past unbonding period (7 days = 604,800,000 ms)
    let unbonding_period = 604_800_000;
    ctx.execute_block_at(ctx.timestamp() + unbonding_period + 1);

    // Claim unstaked funds
    ctx.state
        .submit_tx(Transaction::ClaimUnstaked {
            delegator: TRADER_BOB.into(),
        })
        .unwrap();
    ctx.execute_block();

    // Bob should receive his funds back
    let balance_after_claim = ctx.hyck_balance(TRADER_BOB);
    assert!(
        balance_after_claim > balance_after_undelegate,
        "Balance should increase after claiming: {} > {}",
        balance_after_claim,
        balance_after_undelegate
    );
}
