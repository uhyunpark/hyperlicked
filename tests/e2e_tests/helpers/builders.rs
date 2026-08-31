//! Test Builders
//!
//! Fluent builders for creating test transactions.

use hyperlicked::app::staking::{Evidence, EvidenceType};
use hyperlicked::app::{MarketConfig, OrderType, Side, Transaction, TriggerType};
use hyperlicked::crypto::bls::BlsSecretKey;
use hyperlicked::types::{Certificate, ConsensusContext};

use super::fixtures::{BTC_SYMBOL, DEFAULT_BTC_PRICE, ONE_BTC};
use hyperlicked::app::staking::HYCK_TREASURY_ADDRESS;

// =============================================================================
// OrderBuilder
// =============================================================================

/// Fluent builder for PlaceOrder transactions
pub struct OrderBuilder {
    trader: String,
    symbol: String,
    side: Side,
    price: i64,
    size: i64,
    order_type: OrderType,
    reduce_only: bool,
}

impl OrderBuilder {
    /// Create a bid (buy) order
    pub fn bid(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            symbol: BTC_SYMBOL.into(),
            side: Side::Bid,
            price: DEFAULT_BTC_PRICE,
            size: ONE_BTC,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }
    }

    /// Create an ask (sell) order
    pub fn ask(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            symbol: BTC_SYMBOL.into(),
            side: Side::Ask,
            price: DEFAULT_BTC_PRICE,
            size: ONE_BTC,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }
    }

    /// Set the trading symbol
    pub fn symbol(mut self, symbol: &str) -> Self {
        self.symbol = symbol.into();
        self
    }

    /// Set the price (in cents)
    pub fn at_price(mut self, price: i64) -> Self {
        self.price = price;
        self
    }

    /// Set the size (in satoshis)
    pub fn with_size(mut self, size: i64) -> Self {
        self.size = size;
        self
    }

    /// Set order type to IOC (Immediate or Cancel)
    pub fn ioc(mut self) -> Self {
        self.order_type = OrderType::Ioc;
        self
    }

    /// Set order type to GTC (Good Till Cancel)
    pub fn gtc(mut self) -> Self {
        self.order_type = OrderType::Gtc;
        self
    }

    /// Set order type to ALO (Add Liquidity Only)
    pub fn alo(mut self) -> Self {
        self.order_type = OrderType::Alo;
        self
    }

    /// Set reduce_only flag
    pub fn reduce_only(mut self) -> Self {
        self.reduce_only = true;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::PlaceOrder {
            trader: self.trader,
            symbol: self.symbol,
            side: self.side,
            price: self.price,
            size: self.size,
            order_type: self.order_type,
            reduce_only: self.reduce_only,
        }
    }
}

// =============================================================================
// DepositBuilder
// =============================================================================

/// Fluent builder for Deposit transactions
pub struct DepositBuilder {
    trader: String,
    amount: i64,
}

impl DepositBuilder {
    /// Create a deposit for a trader
    pub fn new(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            amount: 100_000_000, // $1M default
        }
    }

    /// Set the deposit amount
    pub fn amount(mut self, amount: i64) -> Self {
        self.amount = amount;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::Deposit {
            trader: self.trader,
            amount: self.amount,
        }
    }
}

// =============================================================================
// NativeHyckTransferBuilder
// =============================================================================

/// Fluent builder for local-dev treasury-funded native HYCK transfers.
///
/// Staking tests must fund the native HYCK ledger rather than perp collateral.
/// The treasury is protocol-owned, so this remains an explicit system-path
/// transaction in the development fixture.
pub struct NativeHyckTransferBuilder {
    to: String,
    amount: i64,
}

impl NativeHyckTransferBuilder {
    /// Create a treasury-funded transfer for a recipient.
    pub fn new(to: &str) -> Self {
        Self {
            to: to.into(),
            amount: 0,
        }
    }

    /// Set the transfer amount in native HYCK base units.
    pub fn amount(mut self, amount: i64) -> Self {
        self.amount = amount;
        self
    }

    /// Build the native HYCK transfer transaction.
    pub fn build(self) -> Transaction {
        Transaction::TransferHyck {
            from: HYCK_TREASURY_ADDRESS.to_string(),
            to: self.to,
            amount: self.amount,
        }
    }
}

// =============================================================================
// WithdrawBuilder
// =============================================================================

/// Fluent builder for Withdraw transactions
pub struct WithdrawBuilder {
    trader: String,
    amount: i64,
}

impl WithdrawBuilder {
    /// Create a withdrawal for a trader
    pub fn new(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            amount: 0,
        }
    }

    /// Set the withdrawal amount
    pub fn amount(mut self, amount: i64) -> Self {
        self.amount = amount;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::Withdraw {
            trader: self.trader,
            amount: self.amount,
        }
    }
}

// =============================================================================
// CancelBuilder
// =============================================================================

/// Fluent builder for CancelOrder transactions
pub struct CancelBuilder {
    trader: String,
    order_id: String,
}

impl CancelBuilder {
    /// Create a cancel for a trader
    pub fn new(trader: &str, order_id: &str) -> Self {
        Self {
            trader: trader.into(),
            order_id: order_id.into(),
        }
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::CancelOrder {
            trader: self.trader,
            order_id: self.order_id,
        }
    }
}

// =============================================================================
// TriggerOrderBuilder
// =============================================================================

/// Fluent builder for PlaceTriggerOrder transactions
pub struct TriggerOrderBuilder {
    trader: String,
    symbol: String,
    trigger_type: TriggerType,
    trigger_price: i64,
    size: i64,
    limit_price: Option<i64>,
    cloid: Option<String>,
}

impl TriggerOrderBuilder {
    /// Create a stop-loss trigger order
    pub fn stop_loss(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            symbol: BTC_SYMBOL.into(),
            trigger_type: TriggerType::StopLoss,
            trigger_price: DEFAULT_BTC_PRICE - 100_000, // $1k below default
            size: ONE_BTC,
            limit_price: None,
            cloid: None,
        }
    }

    /// Create a take-profit trigger order
    pub fn take_profit(trader: &str) -> Self {
        Self {
            trader: trader.into(),
            symbol: BTC_SYMBOL.into(),
            trigger_type: TriggerType::TakeProfit,
            trigger_price: DEFAULT_BTC_PRICE + 100_000, // $1k above default
            size: ONE_BTC,
            limit_price: None,
            cloid: None,
        }
    }

    /// Set the trading symbol
    pub fn symbol(mut self, symbol: &str) -> Self {
        self.symbol = symbol.into();
        self
    }

    /// Set the trigger price
    pub fn at_trigger_price(mut self, price: i64) -> Self {
        self.trigger_price = price;
        self
    }

    /// Set the size
    pub fn with_size(mut self, size: i64) -> Self {
        self.size = size;
        self
    }

    /// Set the limit price (for limit trigger orders)
    pub fn with_limit_price(mut self, price: i64) -> Self {
        self.limit_price = Some(price);
        self
    }

    /// Set the client order ID
    pub fn with_cloid(mut self, cloid: &str) -> Self {
        self.cloid = Some(cloid.into());
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::PlaceTriggerOrder {
            trader: self.trader,
            symbol: self.symbol,
            trigger_type: self.trigger_type,
            trigger_price: self.trigger_price,
            size: self.size,
            limit_price: self.limit_price,
            cloid: self.cloid,
        }
    }
}

// =============================================================================
// StakingBuilders
// =============================================================================

/// Fluent builder for RegisterValidator transactions
pub struct ValidatorBuilder {
    operator: String,
    node_id: [u8; 32],
    bls_secret_key: BlsSecretKey,
    bls_pubkey: Vec<u8>,
    self_stake: i64,
    commission_bps: i64,
}

fn deterministic_bls_secret_key(node_id: [u8; 32]) -> BlsSecretKey {
    // Test-only deterministic credentials.  The seed convention matches
    // `test_bls_keypair`, while the PoP still binds the complete node ID and
    // the local development chain domain.
    let mut seed = [0u8; 32];
    seed[0] = node_id[0].max(1);
    BlsSecretKey::from_seed(&seed)
}

/// Return valid test credentials for a validator on the local development
/// chain. The proof binds the non-zero test chain domain used by
/// `TestContext`.
pub fn test_validator_credentials(node_id: [u8; 32]) -> (Vec<u8>, Vec<u8>) {
    let secret_key = deterministic_bls_secret_key(node_id);
    let public_key = secret_key.public_key().to_bytes().to_vec();
    let proof = secret_key
        .create_proof_of_possession(&[9u8; 32], &node_id)
        .to_bytes()
        .to_vec();
    (public_key, proof)
}

impl ValidatorBuilder {
    /// Create a validator registration
    pub fn new(operator: &str) -> Self {
        let mut node_id = [0u8; 32];
        node_id[0] = operator.len() as u8;
        let bls_secret_key = deterministic_bls_secret_key(node_id);
        let bls_pubkey = bls_secret_key.public_key().to_bytes().to_vec();

        Self {
            operator: operator.into(),
            node_id,
            bls_secret_key,
            bls_pubkey,
            self_stake: 10_000_000, // $100k default
            commission_bps: 1000,   // 10% default
        }
    }

    /// Set the self-stake amount
    pub fn with_stake(mut self, amount: i64) -> Self {
        self.self_stake = amount;
        self
    }

    /// Set the commission rate in basis points
    pub fn with_commission(mut self, bps: i64) -> Self {
        self.commission_bps = bps;
        self
    }

    /// Set a specific node ID
    pub fn with_node_id(mut self, node_id: [u8; 32]) -> Self {
        self.node_id = node_id;
        self.bls_secret_key = deterministic_bls_secret_key(node_id);
        self.bls_pubkey = self.bls_secret_key.public_key().to_bytes().to_vec();
        self
    }

    /// Set BLS public key bytes
    pub fn with_bls_pubkey(mut self, bls_pubkey: Vec<u8>) -> Self {
        assert_eq!(
            bls_pubkey,
            self.bls_secret_key.public_key().to_bytes().to_vec(),
            "test validator public key must match the deterministic test secret"
        );
        self.bls_pubkey = bls_pubkey;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::RegisterValidator {
            operator: self.operator,
            node_id: self.node_id,
            bls_pubkey: self.bls_pubkey,
            bls_proof_of_possession: self
                .bls_secret_key
                .create_proof_of_possession(&[9u8; 32], &self.node_id)
                .to_bytes()
                .to_vec(),
            self_stake: self.self_stake,
            commission_bps: self.commission_bps,
        }
    }
}

/// Fluent builder for Delegate transactions
pub struct DelegateBuilder {
    delegator: String,
    validator: String,
    amount: i64,
}

impl DelegateBuilder {
    /// Create a delegation
    pub fn new(delegator: &str, validator: &str) -> Self {
        Self {
            delegator: delegator.into(),
            validator: validator.into(),
            amount: 1_000_000, // $10k default
        }
    }

    /// Set the delegation amount
    pub fn amount(mut self, amount: i64) -> Self {
        self.amount = amount;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::Delegate {
            delegator: self.delegator,
            validator: self.validator,
            amount: self.amount,
        }
    }
}

// =============================================================================
// AddMarketBuilder
// =============================================================================

/// Fluent builder for AddMarket transactions
pub struct AddMarketBuilder {
    admin: String,
    config: MarketConfig,
    initial_mark_price: i64,
}

impl AddMarketBuilder {
    /// Create an AddMarket transaction for a new symbol
    pub fn new(admin: &str, symbol: &str) -> Self {
        let mut config = MarketConfig::default();
        config.symbol = symbol.to_string();
        Self {
            admin: admin.into(),
            config,
            initial_mark_price: super::fixtures::DEFAULT_ETH_PRICE,
        }
    }

    /// Set the initial mark price
    pub fn with_mark_price(mut self, price: i64) -> Self {
        self.initial_mark_price = price;
        self
    }

    /// Set max_position_size on the market config
    pub fn with_max_position_size(mut self, max: i64) -> Self {
        self.config.max_position_size = max;
        self
    }

    /// Set tick_size (for validation tests)
    pub fn with_tick_size(mut self, tick: i64) -> Self {
        self.config.tick_size = tick;
        self
    }

    /// Set lot_size (for validation tests)
    pub fn with_lot_size(mut self, lot: i64) -> Self {
        self.config.lot_size = lot;
        self
    }

    /// Set min_notional (for validation tests)
    pub fn with_min_notional(mut self, notional: i64) -> Self {
        self.config.min_notional = notional;
        self
    }

    /// Build the transaction
    pub fn build(self) -> Transaction {
        Transaction::AddMarket {
            admin: self.admin,
            config: self.config,
            initial_mark_price: self.initial_mark_price,
        }
    }
}

// =============================================================================
// BLS Test Helpers
// =============================================================================

/// Create a deterministic BLS keypair for testing
///
/// Returns (secret_key, pubkey_bytes, node_id)
pub fn test_bls_keypair(seed: u8) -> (BlsSecretKey, Vec<u8>, [u8; 32]) {
    let mut seed_bytes = [0u8; 32];
    seed_bytes[0] = seed;
    let sk = BlsSecretKey::from_seed(&seed_bytes);
    let pk = sk.public_key();
    let mut node_id = [0u8; 32];
    node_id[0] = seed;
    (sk, pk.to_bytes().to_vec(), node_id)
}

/// Create valid double-vote evidence with real BLS signatures
pub fn create_valid_evidence(
    context: ConsensusContext,
    sk: &BlsSecretKey,
    node_id: [u8; 32],
    view: u64,
) -> Evidence {
    let hash_a = [1u8; 32];
    let hash_b = [2u8; 32];
    let app_hash_a = [0x11u8; 32];
    let app_hash_b = [0x22u8; 32];

    let msg_a = Certificate::build_signing_message(context, view, &hash_a, &app_hash_a);
    let msg_b = Certificate::build_signing_message(context, view, &hash_b, &app_hash_b);

    let sig_a = sk.sign(&msg_a);
    let sig_b = sk.sign(&msg_b);

    Evidence {
        evidence_type: EvidenceType::DoubleVote,
        offender: node_id,
        view,
        timestamp: 0,
        context,
        hash_a,
        app_hash_a,
        hash_b,
        app_hash_b,
        signature_a: sig_a.to_bytes().to_vec(),
        signature_b: sig_b.to_bytes().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_builder_bid() {
        let tx = OrderBuilder::bid("alice")
            .at_price(5_000_000)
            .with_size(100_000_000)
            .build();

        match tx {
            Transaction::PlaceOrder {
                trader,
                side,
                price,
                size,
                ..
            } => {
                assert_eq!(trader, "alice");
                assert_eq!(side, Side::Bid);
                assert_eq!(price, 5_000_000);
                assert_eq!(size, 100_000_000);
            }
            _ => panic!("Expected PlaceOrder"),
        }
    }

    #[test]
    fn test_order_builder_ask_ioc() {
        let tx = OrderBuilder::ask("bob").ioc().build();

        match tx {
            Transaction::PlaceOrder { order_type, .. } => {
                assert_eq!(order_type, OrderType::Ioc);
            }
            _ => panic!("Expected PlaceOrder"),
        }
    }

    #[test]
    fn test_trigger_builder() {
        let tx = TriggerOrderBuilder::stop_loss("alice")
            .at_trigger_price(4_500_000)
            .with_cloid("sl-1")
            .build();

        match tx {
            Transaction::PlaceTriggerOrder {
                trigger_type,
                trigger_price,
                cloid,
                ..
            } => {
                assert_eq!(trigger_type, TriggerType::StopLoss);
                assert_eq!(trigger_price, 4_500_000);
                assert_eq!(cloid, Some("sl-1".to_string()));
            }
            _ => panic!("Expected PlaceTriggerOrder"),
        }
    }
}
