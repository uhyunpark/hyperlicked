# Core Types & Integer Math

Reference for core type definitions and integer math patterns.

## Table of Contents

- [Type Aliases](#type-aliases)
- [Integer Math](#integer-math)
- [Position & Account](#position--account)
- [MarketConfig](#marketconfig)
- [Serialization](#serialization)

---

## Type Aliases

```rust
// src/types.rs

// Consensus types
pub type View = u64;              // Consensus round number
pub type Height = u64;            // Block number
pub type Hash = [u8; 32];         // SHA-256 hash
pub type NodeId = [u8; 32];       // Validator identity (BLS pubkey hash)
pub type Signature = Vec<u8>;     // Variable length (BLS = 96 bytes)

// Trading types
pub type Price = i64;             // Cents (1 USD = 100)
pub type Size = i64;              // Satoshis (1 unit = 100_000_000)
pub type OrderId = u64;           // Unique order identifier
pub type FillId = u64;            // Unique fill identifier
pub type Symbol = String;         // Trading pair (e.g., "BTC-USDT")
pub type Address = String;        // Trader address (hex string)
```

---

## Integer Math

### Why Integers?

**CRITICAL**: No floats for cross-validator determinism.

Floats have platform-specific rounding:
```rust
// DON'T DO THIS
let price: f64 = 50000.50;  // ❌ Non-deterministic across machines
```

Integers are deterministic:
```rust
// DO THIS
let price: i64 = 5000050;   // ✅ Cents (50000.50 USD)
```

### Unit Conversions

```rust
// Price: cents (1 USD = 100 cents)
let price_cents: i64 = 5_000_000;       // $50,000.00
let price_dollars = price_cents / 100;  // 50000

// Size: satoshis (1 BTC = 100,000,000 satoshis)
let size_sats: i64 = 100_000_000;       // 1.0 BTC
let size_btc = size_sats / 100_000_000; // 1

// Notional value (USD value of position)
let notional = (size_sats * price_cents) / 100_000_000;
// = (100_000_000 * 5_000_000) / 100_000_000
// = 5_000_000 cents = $50,000
```

### PnL Calculations

**IMPORTANT**: Use i128 for intermediate calculations to prevent overflow.

```rust
impl Position {
    pub fn unrealized_pnl(&self, mark_price: Price) -> i64 {
        let price_diff = mark_price - self.entry_price;
        // Use i128 to prevent overflow: size * price_diff can exceed i64
        let pnl_i128 = (self.size as i128 * price_diff as i128) / 100_000_000;
        pnl_i128.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}
```

**Why i128?** With large positions (1000 BTC = 10^11 sats) and large prices ($100k = 10^7 cents),
the product exceeds i64::MAX (~9.2×10^18). Using i128 prevents silent overflow.

### Basis Points (bps)

```rust
// 1 bps = 0.01% = 0.0001
// 10000 bps = 100%

let funding_rate_bps: i64 = 10;  // 0.10%
let notional: i64 = 5_000_000;   // $50,000

let payment = (notional * funding_rate_bps) / 10000;
// = (5_000_000 * 10) / 10000
// = 5000 cents = $50
```

---

## Position & Account

### Position

```rust
pub struct Position {
    pub size: Size,                      // Signed: long > 0, short < 0
    pub entry_price: Price,
    pub realized_pnl: i64,
    pub cumulative_funding: i64,
    pub last_funding_timestamp: u64,
}
```

**size is signed:**
- Positive = long position
- Negative = short position
- Zero = no position

### Position Methods

```rust
impl Position {
    pub fn unrealized_pnl(&self, mark_price: Price) -> i64 {
        let price_diff = mark_price - self.entry_price;
        (self.size * price_diff) / 100_000_000
    }

    pub fn apply_funding(
        &mut self,
        funding_rate_bps: i64,
        index_price: Price,
        timestamp: u64
    ) -> i64 {
        let notional = (self.size.abs() * index_price) / 100_000_000;
        let payment = (notional * funding_rate_bps) / 10000;

        // Long pays short when rate > 0
        let signed_payment = if self.size > 0 { -payment } else { payment };

        self.cumulative_funding += signed_payment;
        self.last_funding_timestamp = timestamp;
        signed_payment
    }

    pub fn liquidation_price(
        &self,
        available_margin: i64,
        maintenance_rate_bps: i64
    ) -> Price {
        // Calculate price where equity = maintenance margin
        // Complex formula depends on position direction
        todo!()
    }
}
```

### Account

```rust
pub struct Account {
    pub address: Address,
    pub balance: i64,                    // Available balance (cents)
    pub locked: i64,                     // Locked for orders
    pub positions: HashMap<Symbol, Position>,
    pub nonce: u64,                      // For replay protection
}
```

### Margin Calculation

```rust
impl Account {
    pub fn available_margin(&self, mark_prices: &HashMap<Symbol, Price>) -> i64 {
        let mut equity = self.balance;

        for (symbol, position) in &self.positions {
            if let Some(&mark_price) = mark_prices.get(symbol) {
                equity += position.unrealized_pnl(mark_price);
            }
        }

        equity - self.locked
    }

    pub fn maintenance_margin(&self, mark_prices: &HashMap<Symbol, Price>) -> i64 {
        let mut required = 0i64;

        for (symbol, position) in &self.positions {
            if let Some(&mark_price) = mark_prices.get(symbol) {
                let notional = (position.size.abs() * mark_price) / 100_000_000;
                required += notional / 100;  // 1% maintenance margin
            }
        }

        required
    }
}
```

---

## MarketConfig

```rust
pub struct MarketConfig {
    pub symbol: Symbol,
    pub tick_size: Price,            // Minimum price increment
    pub lot_size: Size,              // Minimum size increment
    pub min_notional: i64,           // Minimum order value
    pub max_leverage: u8,            // Maximum allowed leverage
    pub maintenance_margin_bps: i64, // Maintenance margin rate
    pub initial_margin_bps: i64,     // Initial margin rate
    pub funding_interval_secs: u64,  // Funding payment interval
}
```

### Default Config

```rust
impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            symbol: "BTC-USDT".to_string(),
            tick_size: 100,           // $1.00 tick
            lot_size: 1_000_000,      // 0.01 BTC
            min_notional: 1_000_000,  // $10,000 minimum
            max_leverage: 50,
            maintenance_margin_bps: 50,  // 0.5%
            initial_margin_bps: 100,     // 1%
            funding_interval_secs: 3600, // 1 hour
        }
    }
}
```

---

## Serialization

### Serde Patterns

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub view: View,
    pub height: Height,
    // ...
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vote {
    // ...
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bls_pubkey: Option<Vec<u8>>,
}
```

### Transaction Serialization

```rust
impl Transaction {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        serde_json::from_slice(data).ok()
    }
}
```

### Deterministic Hashing

```rust
impl Block {
    pub fn hash(&self) -> Hash {
        let mut hasher = Sha256::new();
        // Always use little-endian for integers
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.parent);
        hasher.update(&self.payload);
        hasher.update(self.proposer);
        hasher.update(self.app_hash);
        hasher.update(self.timestamp.to_le_bytes());
        hasher.finalize().into()
    }
}
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [ORDERBOOK.md](ORDERBOOK.md) - Order matching
- [CONSENSUS.md](CONSENSUS.md) - Block structure
