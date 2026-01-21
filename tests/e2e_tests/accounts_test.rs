//! Account Tests
//!
//! Tests for deposit, withdraw, and account lifecycle.

use hyperlicked::app::Transaction;

use crate::e2e::helpers::*;

/// Test that first deposit creates an account
#[test]
fn test_deposit_creates_account() {
    let mut ctx = TestContext::new();

    // Initially no account
    assert_account_not_exists(&ctx.state, TRADER_ALICE);

    // Submit deposit
    ctx.state
        .submit_tx(Transaction::Deposit {
            trader: TRADER_ALICE.into(),
            amount: DEFAULT_DEPOSIT,
        })
        .expect("deposit should be accepted");

    // Execute block
    ctx.execute_block();

    // Account should exist with correct balance
    assert_account_exists(&ctx.state, TRADER_ALICE);
    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT);
}

/// Test that subsequent deposits add to balance
#[test]
fn test_deposit_adds_to_balance() {
    let mut ctx = TestContext::with_traders(&[(TRADER_ALICE, DEFAULT_DEPOSIT)]);

    let initial_balance = ctx.balance(TRADER_ALICE);
    let additional = 50_000_000; // $500k

    // Submit another deposit
    ctx.state
        .submit_tx(Transaction::Deposit {
            trader: TRADER_ALICE.into(),
            amount: additional,
        })
        .expect("deposit should be accepted");

    ctx.execute_block();

    // Balance should be sum of both deposits
    assert_balance(&ctx.state, TRADER_ALICE, initial_balance + additional);
}

/// Test that withdrawal reduces balance
#[test]
fn test_withdraw_reduces_balance() {
    let mut ctx = TestContext::with_traders(&[(TRADER_ALICE, DEFAULT_DEPOSIT)]);

    let withdraw_amount = 30_000_000; // $300k

    ctx.state
        .submit_tx(Transaction::Withdraw {
            trader: TRADER_ALICE.into(),
            amount: withdraw_amount,
        })
        .expect("withdraw should be accepted");

    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT - withdraw_amount);
}

/// Test that withdrawal fails with insufficient balance
#[test]
fn test_withdraw_fails_insufficient() {
    let mut ctx = TestContext::with_traders(&[(TRADER_ALICE, SMALL_DEPOSIT)]);

    let initial_balance = ctx.balance(TRADER_ALICE);

    // Try to withdraw more than balance
    ctx.state
        .submit_tx(Transaction::Withdraw {
            trader: TRADER_ALICE.into(),
            amount: SMALL_DEPOSIT + 100, // More than available
        })
        .expect("withdraw should be accepted to mempool");

    ctx.execute_block();

    // Balance should remain unchanged (tx failed during execution)
    assert_balance(&ctx.state, TRADER_ALICE, initial_balance);
}

/// Test multiple deposits from different traders in same block
#[test]
fn test_multiple_deposits_same_block() {
    let mut ctx = TestContext::new();

    // Submit multiple deposits
    for (trader, amount) in &[
        (TRADER_ALICE, 100_000_000i64),
        (TRADER_BOB, 50_000_000),
        (TRADER_CHARLIE, 25_000_000),
    ] {
        ctx.state
            .submit_tx(Transaction::Deposit {
                trader: (*trader).into(),
                amount: *amount,
            })
            .expect("deposit should be accepted");
    }

    // Execute single block
    ctx.execute_block();

    // All accounts should be created with correct balances
    assert_balance(&ctx.state, TRADER_ALICE, 100_000_000);
    assert_balance(&ctx.state, TRADER_BOB, 50_000_000);
    assert_balance(&ctx.state, TRADER_CHARLIE, 25_000_000);
}

/// Test deposit then withdraw sequence
#[test]
fn test_deposit_withdraw_sequence() {
    let mut ctx = TestContext::new();

    // Deposit
    ctx.state
        .submit_tx(Transaction::Deposit {
            trader: TRADER_ALICE.into(),
            amount: DEFAULT_DEPOSIT,
        })
        .unwrap();
    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT);

    // Partial withdraw
    ctx.state
        .submit_tx(Transaction::Withdraw {
            trader: TRADER_ALICE.into(),
            amount: 40_000_000,
        })
        .unwrap();
    ctx.execute_block();

    assert_balance(&ctx.state, TRADER_ALICE, DEFAULT_DEPOSIT - 40_000_000);

    // Another deposit
    ctx.state
        .submit_tx(Transaction::Deposit {
            trader: TRADER_ALICE.into(),
            amount: 20_000_000,
        })
        .unwrap();
    ctx.execute_block();

    assert_balance(
        &ctx.state,
        TRADER_ALICE,
        DEFAULT_DEPOSIT - 40_000_000 + 20_000_000,
    );
}
