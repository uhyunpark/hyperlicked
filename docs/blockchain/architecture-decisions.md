# Architecture Decision Records

This document records important architectural decisions for the Hyperlicked blockchain.

---

## ADR-001: Orderbook State in App Hash

**Status:** FINAL (2026-01-29)
**Context:** Consensus safety requires validators to detect state divergence
**Decision:** Hash only aggregate orderbook state (best_bid, best_ask, last_price) per symbol, NOT individual orders

### Background

The `app_hash` is computed after each block execution and included in votes. Validators with different `app_hash` values will not reach consensus, detecting Byzantine faults or implementation bugs.

### Question

Should the `app_hash` include:
1. **Option A:** Full order hashing - Hash every individual order in the orderbook
2. **Option B:** Aggregate hashing - Hash only (best_bid, best_ask, last_price) per symbol

### Decision

**Option B: Aggregate hashing**

Current implementation in `src/app/state/consensus.rs:130-142`:
```rust
// === Orderbooks ===
for symbol in &symbols {
    if let Some(book) = self.orderbooks.get(*symbol) {
        hasher.update(symbol.as_bytes());
        hasher.update(book.best_bid().unwrap_or(0).to_le_bytes());
        hasher.update(book.best_ask().unwrap_or(0).to_le_bytes());
        hasher.update(book.last_price().to_le_bytes());
    }
}
```

### Rationale

1. **Individual order divergence manifests as different fills**
   - If two validators have different orders in their books, they will produce different fills
   - Different fills → different account states (balances, positions)
   - Account state IS fully hashed (every account, every position)
   - Therefore, order-level divergence will be detected when it causes different execution outcomes

2. **Performance considerations**
   - Full order hashing: O(n) per symbol where n = number of open orders
   - Aggregate hashing: O(1) per symbol
   - With thousands of open orders per symbol, full hashing becomes expensive
   - Block production must be fast (<10ms target)

3. **What aggregate hash catches**
   - Best bid/ask divergence (price-level bugs)
   - Last price divergence (matching bugs)
   - Missing/extra symbols (market configuration bugs)

### Trade-offs

**Pros of current approach:**
- Fast: O(k) where k = number of symbols (typically <10)
- Catches meaningful divergence (price levels affect matching)
- Account state hash catches fill divergence

**Cons of current approach:**
- Two validators could have different mid-book orders with same best_bid/ask
- This scenario only matters if those orders never interact
- If they interact, fills will differ, accounts will differ, hash will differ

**Mitigating factor:**
- The only way to have different mid-book orders with same best/ask is if:
  - Orders were placed in different sequences (not possible with deterministic consensus)
  - OR a bug exists that doesn't affect best prices (very narrow edge case)

### Alternatives Considered

1. **Full order hashing** - Too expensive for production throughput targets
2. **Merkle tree of orders** - Better than full hashing but still O(n) for any change
3. **Incremental hash with order ID tracking** - Complex, not worth the edge case protection

### Consequences

- Block production remains fast with large orderbooks
- Rare edge cases of mid-book divergence won't be immediately detected
- Any divergence that affects trading will be caught via account state hash
- This is an acceptable trade-off for production performance

### Status

**FINAL** - Do not revisit unless fundamental architecture changes.

If we ever need stricter orderbook consistency guarantees (e.g., for regulatory compliance), we can add a periodic full orderbook hash check outside the critical path.

---

## ADR-002: Insurance Fund Cannot Go Negative

**Status:** FINAL (2026-01-29)
**Context:** ADL and liquidation can create losses exceeding insurance fund
**Decision:** Floor insurance fund at zero after ADL processing

### Background

The insurance fund absorbs losses from underwater liquidations. When a position's loss exceeds the account's remaining margin, the insurance fund covers the difference. If the insurance fund is insufficient, ADL (auto-deleverage) distributes the loss to profitable traders.

### Problem

After ADL processing, the insurance fund could theoretically go negative if:
1. ADL doesn't fully absorb the loss
2. A bug in the accounting allows negative values

### Decision

After all liquidation and ADL processing in each block, enforce:
```rust
if self.insurance_fund < 0 {
    tracing::warn!("Insurance fund went negative, flooring at zero");
    self.insurance_fund = 0;
}
```

### Rationale

1. **Negative fund is meaningless** - A negative balance represents debt, but insurance fund has no debt mechanism
2. **Prevents accounting cascades** - Negative fund in subsequent blocks could cause increasingly wrong calculations
3. **Warning log alerts operators** - If this happens, something is wrong that needs investigation
4. **Defense in depth** - ADL should prevent this, but the floor is a safety net

### Consequences

- Insurance fund is always >= 0
- Operators are alerted if the floor is hit
- No silent state corruption from negative balances

---

## Future ADRs

Reserved for future architectural decisions:

- ADR-003: Reserved
- ADR-004: Reserved
- ADR-005: Reserved
