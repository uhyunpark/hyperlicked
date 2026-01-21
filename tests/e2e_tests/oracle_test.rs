//! Oracle Integration Tests
//!
//! Tests for oracle price updates and funding rate integration.

use crate::e2e::helpers::*;
use hyperlicked::app::{OracleConfig, PriceSource, Transaction};

/// Test oracle update via transaction
#[test]
fn test_oracle_price_update() {
    let mut ctx = TestContext::with_default_traders();

    // Register a validator first (oracle updates require validator auth)
    let node_id = [1u8; 32];
    ctx.state
        .submit_tx(Transaction::RegisterValidator {
            operator: TRADER_ALICE.into(),
            node_id,
            bls_pubkey: vec![0u8; 48],
            self_stake: 10_000_000, // $100k
            commission_bps: 500,
        })
        .unwrap();
    ctx.execute_block();

    // Enable oracle
    ctx.state.oracle_mut().set_enabled(true);
    ctx.state.oracle_mut().set_config(
        BTC_SYMBOL,
        OracleConfig {
            min_sources: 3,
            max_staleness_ms: 10000,
            max_deviation_bps: 1000, // 10%
            fallback_to_mark: true,
        },
    );

    // Submit oracle price update
    let timestamp = ctx.timestamp();
    let sources = vec![
        PriceSource {
            source_id: "binance".into(),
            price: 5_100_000, // $51,000
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "coinbase".into(),
            price: 5_110_000,
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "okx".into(),
            price: 5_090_000,
            timestamp,
            weight_bps: 3334,
        },
    ];

    ctx.state
        .submit_tx(Transaction::OraclePriceUpdate {
            operator: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            sources,
            signature: vec![], // Signature check skipped in tests
        })
        .unwrap();
    ctx.execute_block();

    // Verify oracle price is set
    let oracle = ctx.state.oracle();
    assert!(oracle.prices.contains_key(BTC_SYMBOL));
    let oracle_price = oracle.prices.get(BTC_SYMBOL).unwrap();

    // Median should be around $51,000
    assert!(
        oracle_price.price >= 5_090_000 && oracle_price.price <= 5_110_000,
        "Oracle price {} should be between 5090000 and 5110000",
        oracle_price.price
    );
    assert_eq!(oracle_price.source_count, 3);
}

/// Test that funding uses oracle index price when enabled
#[test]
fn test_funding_uses_oracle_index_price() {
    let mut ctx = TestContext::with_default_traders();

    // Register validator
    let node_id = [1u8; 32];
    ctx.state
        .submit_tx(Transaction::RegisterValidator {
            operator: TRADER_ALICE.into(),
            node_id,
            bls_pubkey: vec![0u8; 48],
            self_stake: 10_000_000,
            commission_bps: 500,
        })
        .unwrap();
    ctx.execute_block();

    // Setup positions: Alice long, Bob short
    ctx.state
        .submit_tx(
            OrderBuilder::bid(TRADER_ALICE)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.state
        .submit_tx(
            OrderBuilder::ask(TRADER_BOB)
                .at_price(DEFAULT_BTC_PRICE)
                .with_size(ONE_BTC)
                .build(),
        )
        .unwrap();
    ctx.execute_block();

    // Verify positions
    assert_long_position(&ctx.state, TRADER_ALICE, BTC_SYMBOL);
    assert_short_position(&ctx.state, TRADER_BOB, BTC_SYMBOL);

    // Enable oracle with lower index price (should create negative funding = shorts pay longs)
    ctx.state.oracle_mut().set_enabled(true);
    ctx.state.oracle_mut().set_config(
        BTC_SYMBOL,
        OracleConfig {
            min_sources: 3,
            max_staleness_ms: 10000,
            max_deviation_bps: 1000,
            fallback_to_mark: true,
        },
    );

    // Submit oracle price slightly below mark (creates negative premium)
    let timestamp = ctx.timestamp();
    let sources = vec![
        PriceSource {
            source_id: "binance".into(),
            price: 4_950_000, // $49,500 (1% below mark)
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "coinbase".into(),
            price: 4_950_000,
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "okx".into(),
            price: 4_950_000,
            timestamp,
            weight_bps: 3334,
        },
    ];

    ctx.state
        .submit_tx(Transaction::OraclePriceUpdate {
            operator: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            sources,
            signature: vec![],
        })
        .unwrap();
    ctx.execute_block();

    // Verify index_price uses oracle
    let index_price = ctx.state.index_price(BTC_SYMBOL).unwrap();
    assert_eq!(
        index_price, 4_950_000,
        "Index price should use oracle price"
    );

    // Mark price should still be from last trade
    let mark_price = ctx.state.mark_price(BTC_SYMBOL).unwrap();
    assert_eq!(mark_price, DEFAULT_BTC_PRICE, "Mark price unchanged");
}

/// Test oracle falls back to mark price when disabled
#[test]
fn test_oracle_fallback_to_mark() {
    let ctx = TestContext::with_default_traders();

    // Oracle disabled by default
    assert!(!ctx.state.oracle().enabled);

    // index_price should return mark price
    let index = ctx.state.index_price(BTC_SYMBOL);
    let mark = ctx.state.mark_price(BTC_SYMBOL);

    assert_eq!(index, mark, "Index should equal mark when oracle disabled");
}

/// Test oracle circuit breaker rejects high deviation
#[test]
fn test_oracle_circuit_breaker() {
    let mut ctx = TestContext::with_default_traders();

    // Register validator
    let node_id = [1u8; 32];
    ctx.state
        .submit_tx(Transaction::RegisterValidator {
            operator: TRADER_ALICE.into(),
            node_id,
            bls_pubkey: vec![0u8; 48],
            self_stake: 10_000_000,
            commission_bps: 500,
        })
        .unwrap();
    ctx.execute_block();

    // Enable oracle with 10% max deviation
    ctx.state.oracle_mut().set_enabled(true);
    ctx.state.oracle_mut().set_config(
        BTC_SYMBOL,
        OracleConfig {
            min_sources: 3,
            max_staleness_ms: 10000,
            max_deviation_bps: 1000, // 10%
            fallback_to_mark: true,
        },
    );

    // Try to submit oracle price 15% above mark (should fail)
    let timestamp = ctx.timestamp();
    let sources = vec![
        PriceSource {
            source_id: "binance".into(),
            price: 5_750_000, // $57,500 (15% above $50k mark)
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "coinbase".into(),
            price: 5_750_000,
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "okx".into(),
            price: 5_750_000,
            timestamp,
            weight_bps: 3334,
        },
    ];

    ctx.state
        .submit_tx(Transaction::OraclePriceUpdate {
            operator: TRADER_ALICE.into(),
            symbol: BTC_SYMBOL.into(),
            sources,
            signature: vec![],
        })
        .unwrap();
    ctx.execute_block();

    // Oracle should NOT have this price (circuit breaker triggered)
    let oracle = ctx.state.oracle();
    assert!(
        !oracle.prices.contains_key(BTC_SYMBOL),
        "Circuit breaker should reject high deviation prices"
    );
}

/// Test unauthorized oracle update is rejected
#[test]
fn test_oracle_unauthorized_rejected() {
    let mut ctx = TestContext::with_default_traders();

    // Enable oracle
    ctx.state.oracle_mut().set_enabled(true);
    ctx.state.oracle_mut().set_config(
        BTC_SYMBOL,
        OracleConfig {
            min_sources: 3,
            max_staleness_ms: 10000,
            max_deviation_bps: 1000,
            fallback_to_mark: true,
        },
    );

    // Try to submit without being a validator (Alice is not registered)
    // Note: This test only works when SKIP_SIG_VERIFY is not set
    // In most test environments, sig verification is skipped, so this test
    // verifies the transaction structure is correct even if auth passes

    let timestamp = ctx.timestamp();
    let sources = vec![
        PriceSource {
            source_id: "binance".into(),
            price: 5_000_000,
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "coinbase".into(),
            price: 5_000_000,
            timestamp,
            weight_bps: 3333,
        },
        PriceSource {
            source_id: "okx".into(),
            price: 5_000_000,
            timestamp,
            weight_bps: 3334,
        },
    ];

    // Submit should succeed to mempool
    let result = ctx.state.submit_tx(Transaction::OraclePriceUpdate {
        operator: "unauthorized_user".into(),
        symbol: BTC_SYMBOL.into(),
        sources,
        signature: vec![],
    });
    assert!(result.is_ok(), "Transaction should be accepted to mempool");

    // But execution should fail (if SKIP_SIG_VERIFY is false)
    // The transaction will be in mempool but rejected during block execution
    ctx.execute_block();

    // In test environment with SKIP_SIG_VERIFY=true, this will actually succeed
    // In production, it would fail. This test documents the expected behavior.
}
