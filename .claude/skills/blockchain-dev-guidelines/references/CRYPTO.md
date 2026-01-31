# Cryptographic Operations

Reference for BLS signatures, EIP-712 signing, and agent keys.

## Table of Contents

- [BLS Signatures](#bls-signatures)
- [Signature Aggregation](#signature-aggregation)
- [EIP-712 Signing](#eip-712-signing)
- [Agent Keys](#agent-keys)

---

## BLS Signatures

### Overview

BLS12-381 is used for validator signatures because:
- **Aggregation**: Combine N signatures into 1
- **Compact proofs**: 96 bytes regardless of signer count
- **Verification efficiency**: Single pairing check for aggregated sig

### Key Sizes

```rust
// Public key: 48 bytes (compressed G1 point)
pub struct BlsPublicKey { inner: PublicKey }

// Signature: 96 bytes (compressed G2 point)
pub struct BlsSignature { inner: Signature }

// Secret key: 32 bytes
pub struct BlsSecretKey { inner: SecretKey }
```

### Key Generation

```rust
// src/crypto/bls.rs

impl BlsSecretKey {
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let sk = SecretKey::key_gen(seed, &[]).expect("key gen failed");
        Self { inner: sk }
    }

    pub fn public_key(&self) -> BlsPublicKey {
        BlsPublicKey { inner: self.inner.sk_to_pk() }
    }
}
```

### Signing

```rust
const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_";

impl BlsSecretKey {
    pub fn sign(&self, message: &[u8]) -> BlsSignature {
        let sig = self.inner.sign(message, DST, &[]);
        BlsSignature { inner: sig }
    }
}
```

### Verification

```rust
impl BlsPublicKey {
    pub fn verify(&self, message: &[u8], signature: &BlsSignature) -> bool {
        signature.inner.verify(
            true,  // Check group membership
            message,
            DST,
            &[],
            &self.inner,
            true   // Check point validity
        ) == blst::BLST_ERROR::BLST_SUCCESS
    }
}
```

---

## Signature Aggregation

### Aggregating Signatures

```rust
pub fn aggregate_signatures(signatures: &[BlsSignature]) -> Result<BlsSignature> {
    if signatures.is_empty() {
        return Err(BlsError::NoSignatures);
    }

    let sigs: Vec<&Signature> = signatures.iter().map(|s| &s.inner).collect();
    let agg = AggregateSignature::aggregate(&sigs, true)?;

    Ok(BlsSignature { inner: agg.to_signature() })
}
```

### Aggregating Public Keys

```rust
pub fn aggregate_pubkeys(pubkeys: &[BlsPublicKey]) -> Result<BlsPublicKey> {
    if pubkeys.is_empty() {
        return Err(BlsError::NoSignatures);
    }

    let pks: Vec<&PublicKey> = pubkeys.iter().map(|pk| &pk.inner).collect();
    let agg = AggregatePublicKey::aggregate(&pks, true)?;

    Ok(BlsPublicKey { inner: agg.to_public_key() })
}
```

### Verifying Aggregated Signature

**IMPORTANT**: BLS aggregation only works when all signers sign the **same message**.

```rust
/// Verify aggregate signature when all signers signed the same message
pub fn verify_aggregate_same_message(
    message: &[u8],
    pubkeys: &[BlsPublicKey],
    agg_signature: &BlsSignature
) -> bool {
    if pubkeys.is_empty() {
        return false;
    }

    // Aggregate public keys
    let agg_pk = match aggregate_pubkeys(pubkeys) {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    agg_pk.verify(message, agg_signature)
}

/// For different messages, verify each signature individually
/// (Cannot use aggregation optimization)
pub fn verify_multi_message(
    pubkeys: &[BlsPublicKey],
    messages: &[Vec<u8>],
    signatures: &[BlsSignature]
) -> bool {
    if pubkeys.len() != messages.len() || messages.len() != signatures.len() {
        return false;
    }

    pubkeys.iter()
        .zip(messages.iter())
        .zip(signatures.iter())
        .all(|((pk, msg), sig)| pk.verify(msg, sig))
}
```

### Certificate with BLS

```rust
impl Certificate {
    pub fn new_bls(
        view: View,
        block_hash: Hash,
        app_hash: Hash,
        votes: Vec<Vote>,
        agg_signature: Vec<u8>
    ) -> Self {
        let voters = votes.iter().map(|v| v.voter).collect();
        let bls_pubkeys = votes.iter()
            .filter_map(|v| v.bls_pubkey.clone())
            .collect();

        Self {
            view,
            block_hash,
            votes: vec![],  // Don't store individual votes
            voters,
            bls_pubkeys,
            agg_signature,
            app_hash: Some(app_hash),  // Store for verification
        }
    }

    /// Verify BLS aggregate signature
    /// All voters must have signed the same message: (view, block_hash, app_hash)
    pub fn verify_bls(&self) -> bool {
        let message = Self::build_signing_message(
            self.view,
            self.block_hash,
            self.app_hash.unwrap_or([0u8; 32])
        );
        verify_aggregate_same_message(&message, &self.bls_pubkeys, &self.agg_signature)
    }
}
```

---

## EIP-712 Signing

### Overview

EIP-712 enables structured data signing:
- Type-safe signatures
- Human-readable signing prompts
- Replay protection via domain separator

### Domain Separator

```rust
pub struct EIP712Domain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: Option<String>,
}

impl EIP712Domain {
    pub fn hyperlicked() -> Self {
        Self {
            name: "Hyperlicked".to_string(),
            version: "1".to_string(),
            chain_id: 1337,  // Devnet
            verifying_contract: None,
        }
    }

    pub fn separator(&self) -> Hash {
        let type_hash = keccak256(
            "EIP712Domain(string name,string version,uint256 chainId)"
        );
        keccak256(abi_encode(&[
            type_hash,
            keccak256(&self.name),
            keccak256(&self.version),
            self.chain_id,
        ]))
    }
}
```

### Order Signing

```rust
pub struct OrderEIP712 {
    pub trader: String,
    pub symbol: String,
    pub side: u8,        // 0 = Bid, 1 = Ask
    pub price: i64,
    pub size: i64,
    pub order_type: u8,  // 0 = Gtc, 1 = Ioc, 2 = Alo
    pub reduce_only: bool,
    pub nonce: u64,
}

impl OrderEIP712 {
    pub fn type_hash() -> Hash {
        keccak256(
            "Order(address trader,string symbol,uint8 side,int64 price,\
             int64 size,uint8 orderType,bool reduceOnly,uint64 nonce)"
        )
    }

    pub fn struct_hash(&self) -> Hash {
        keccak256(abi_encode(&[
            Self::type_hash(),
            keccak256(&self.trader),
            keccak256(&self.symbol),
            self.side,
            self.price,
            self.size,
            self.order_type,
            self.reduce_only,
            self.nonce,
        ]))
    }

    pub fn signing_hash(&self, domain: &EIP712Domain) -> Hash {
        keccak256(abi_encode(&[
            0x19u8, 0x01u8,
            domain.separator(),
            self.struct_hash(),
        ]))
    }
}
```

---

## Agent Keys

### Overview

Agent keys enable gasless trading:
- User generates random keypair
- Delegates signing authority to agent key
- Orders signed with agent key (no popup)
- Main wallet only signs delegation once

### Delegation Structure

```rust
pub struct AgentDelegation {
    pub master: Address,       // Main wallet address
    pub agent: Address,        // Agent key address
    pub expiry: u64,           // Unix timestamp
    pub nonce: u64,            // Delegation nonce
    pub signature: Vec<u8>,    // Master wallet's signature
}
```

### Delegation Signing

```rust
impl AgentDelegation {
    pub fn type_hash() -> Hash {
        keccak256(
            "AgentDelegation(address master,address agent,uint64 expiry,uint64 nonce)"
        )
    }

    pub fn verify(&self, domain: &EIP712Domain) -> bool {
        let struct_hash = keccak256(abi_encode(&[
            Self::type_hash(),
            keccak256(&self.master),
            keccak256(&self.agent),
            self.expiry,
            self.nonce,
        ]));

        let signing_hash = keccak256(abi_encode(&[
            0x19u8, 0x01u8,
            domain.separator(),
            struct_hash,
        ]));

        // Recover signer from signature and verify it's the master
        let recovered = ecrecover(&signing_hash, &self.signature);
        recovered == self.master && self.expiry > current_timestamp()
    }
}
```

### Order with Agent Signature

```rust
pub struct SignedOrder {
    pub order: OrderEIP712,
    pub signature: Vec<u8>,
    pub signer: Address,        // Who signed (agent or master)
    pub delegation: Option<AgentDelegation>,  // If signed by agent
}

impl SignedOrder {
    pub fn verify(&self, domain: &EIP712Domain) -> bool {
        let signing_hash = self.order.signing_hash(domain);
        let recovered = ecrecover(&signing_hash, &self.signature);

        if recovered == self.order.trader {
            // Signed by master wallet directly
            return true;
        }

        // Check agent delegation
        if let Some(ref delegation) = self.delegation {
            if delegation.master == self.order.trader
               && delegation.agent == recovered
               && delegation.verify(domain)
            {
                return true;
            }
        }

        false
    }
}
```

---

## WebSocket Authentication

WebSocket user subscriptions require signature verification in production:

### Personal_Sign for Subscriptions

```rust
// In api/websocket.rs
// Message format: "Subscribe to {address} at {timestamp}"

async fn verify_subscription_auth(
    address: &str,
    signature: &Option<String>,
    timestamp: &Option<u64>,
    agent: &Option<String>,  // Optional: agent address if using agent key
    api_state: &ApiState,
) -> bool {
    // Dev mode: no auth required
    if Config::global().mode == Mode::Dev {
        return true;
    }

    // Verify timestamp freshness (5 min window)
    let now = current_timestamp();
    if now.abs_diff(*timestamp) > 300 {
        return false;
    }

    // Create EIP-191 message hash
    let message = format!("Subscribe to {} at {}", address, timestamp);
    let prefixed = format!("\x19Ethereum Signed Message:\n{}{}", message.len(), message);
    let hash = keccak256(prefixed.as_bytes());

    // Recover signer
    let recovered = ecrecover(&hash, &signature)?;

    // Case 1: Signed by wallet directly
    if recovered == address {
        return true;
    }

    // Case 2: Signed by delegated agent key
    if let Some(agent_addr) = agent {
        if recovered == agent_addr {
            // Verify agent has valid delegation for this wallet
            let delegations = api_state.delegations.read().await;
            if let Some(stored) = delegations.get(&address.to_lowercase()) {
                if stored.delegation.agent == agent_addr
                    && !stored.delegation.is_expired() {
                    return true;  // Valid agent signature
                }
            }
        }
    }

    false
}
```

### Frontend Usage

```typescript
// Using agent key (no popup)
const message = `Subscribe to ${address} at ${timestamp}`;
const signature = await agentSigner.signMessage(message);

ws.send(JSON.stringify({
  op: 'subscribe',
  address,
  signature,
  timestamp,
  agent: agentSigner.address  // Tell server to check delegation
}));
```

---

## API Rate Limiting

IP-based rate limiting for REST endpoints:

```rust
// In api/rate_limit.rs
pub struct RateLimiter {
    trading_limits: RwLock<HashMap<IpAddr, VecDeque<Instant>>>,
    read_limits: RwLock<HashMap<IpAddr, VecDeque<Instant>>>,
}

// Limits by endpoint type:
// - Trading (orders, cancels): 100 req/min
// - Read (orderbook, account): 1000 req/min
// - Heavy (sync, snapshots): 20 req/min

// Response headers:
// X-RateLimit-Limit: 100
// X-RateLimit-Remaining: 42
// Retry-After: 30 (when limited)
```

---

**Related Files:**
- [../SKILL.md](../SKILL.md) - Main skill guide
- [CONSENSUS.md](CONSENSUS.md) - Vote signatures
- [TYPES.md](TYPES.md) - Hash and signature types
