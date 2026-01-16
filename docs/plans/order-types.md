# Perp DEX Order Types Implementation Plan

## Overview

Implement Hyperliquid-style order features in phases.

---

## Phase 1: Core Order Options (Do Now)

### 1.1 TIF Selector UI
**Status:** ✅ Done

- Added `TimeInForce` type and `TIF_CODES` constant to `web/lib/types.ts`
- Added TIF selector buttons (GTC/IOC/Post Only) in `TradePanel.tsx`
- Market orders automatically use IOC, limit orders use selected TIF

### 1.2 Reduce Only
**Status:** ✅ Done

- Added `reduce_only: bool` to `Order` struct in `orderbook.rs`
- Added to `PlaceOrder` transaction in `mod.rs`
- Added validation in `state.rs` (rejects if would increase position, clamps size)
- Added `reduce_only: Option<bool>` to `OrderDetails` in `routes.rs`
- Added checkbox in `TradePanel.tsx`

### 1.3 Market Orders
**Status:** ✅ Done

Market orders use IOC (Immediate-or-Cancel) with sweep prices:
- Buy: Price = 2x current price (sweeps all asks)
- Sell: Price = 0.01 (sweeps all bids)
- No resting - unfilled portion is cancelled immediately

**Implementation:**
- Already has Limit/Market toggle in `TradePanel.tsx`
- Market orders automatically use `TIF_CODES.ioc`
- Updated price logic to use sweep prices for market orders

---

## Phase 2: Order Tracking & Triggers (Next)

### 2.1 Client Order ID (cloid)
Custom order IDs for tracking.

### 2.2 Stop Loss Orders
Trigger order when price crosses threshold.

### 2.3 Take Profit Orders
Same as SL, opposite direction.

---

## Phase 3: Advanced Features (Future)

### 3.1 TP/SL Attached to Position
Auto-create TP/SL when opening position.

### 3.2 Good-til-Date (GTD)
Order expires at specific timestamp.

### 3.3 Order Grouping
Link related orders (cancel one → cancel all).

---

## TIF Reference

| TIF | Name | Behavior |
|-----|------|----------|
| **GTC** | Good-Til-Cancel | Stays on book until filled or cancelled |
| **IOC** | Immediate-or-Cancel | Fill immediately, cancel unfilled portion |
| **ALO** | Add-Liquidity-Only | Rejected if would match (maker only) |

---

## File Summary

### Phase 1 Files

| File | Changes |
|------|---------|
| `src/app/orderbook.rs` | Add `reduce_only`, `Market` type |
| `src/app/mod.rs` | Update Transaction enum |
| `src/app/state.rs` | Add reduce_only validation |
| `src/api/routes.rs` | Update OrderDetails struct |
| `web/components/trading/TradePanel.tsx` | Add TIF, reduce-only, market UI |
| `web/lib/api.ts` | Update SignedTransaction type |

---

## Effort Estimates

| Phase | Feature | Effort |
|-------|---------|--------|
| 1 | TIF selector UI | 30 min |
| 1 | Reduce Only | 1 hr |
| 1 | Market Orders | 1 hr |
| 2 | Client Order ID | 30 min |
| 2 | Stop Loss | 2 hr |
| 2 | Take Profit | 1 hr |
| 3 | TP/SL attached | 2 hr |
| 3 | GTD expiry | 1 hr |

---

## Implementation Order (Phase 1)

1. **TIF selector** - Quickest win, backend already done
2. **Reduce Only** - Backend + frontend
3. **Market Orders** - Backend + frontend
