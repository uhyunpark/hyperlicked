//! API Types
//!
//! Request and response types for the REST API.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};

use super::state::{PriceLevel, SharedState};
use crate::app::{AppError, SignedEnvelope};
use crate::config::Mode;
use crate::crypto::{AgentDelegation, AgentSigner, EIP712Signer};
use crate::storage::PersistentStore;

/// Stored delegation with signature
#[derive(Clone)]
pub struct StoredDelegation {
    pub delegation: AgentDelegation,
    pub signature: Vec<u8>,
}

/// Immutable security decisions captured when the API router is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiSecurityPolicy {
    pub mode: Mode,
    pub skip_signature_verification: bool,
}

impl ApiSecurityPolicy {
    /// Build the effective policy for an explicit runtime mode.
    pub fn for_mode(mode: Mode) -> Self {
        let requested_skip = std::env::var("SKIP_SIG_VERIFY")
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);
        Self::new(mode, requested_skip)
    }

    /// Construct a policy from a requested development-only skip flag.
    pub fn new(mode: Mode, requested_skip_signature_verification: bool) -> Self {
        Self {
            mode,
            skip_signature_verification: mode.is_dev() && requested_skip_signature_verification,
        }
    }
}

/// Extended shared state with delegations
#[derive(Clone)]
pub struct ApiState {
    pub shared: SharedState,
    pub delegations: Arc<RwLock<HashMap<String, StoredDelegation>>>,
    pub eip712_signer: Arc<EIP712Signer>,
    pub agent_signer: Arc<AgentSigner>,
    /// Optional persistent store for sync endpoints
    pub store: Option<Arc<dyn PersistentStore + Send + Sync>>,
    pub security_policy: ApiSecurityPolicy,
}

impl ApiState {
    pub fn new(shared: SharedState) -> Self {
        Self::with_policy(shared, ApiSecurityPolicy::for_mode(Mode::from_env()))
    }

    pub fn with_policy(shared: SharedState, security_policy: ApiSecurityPolicy) -> Self {
        Self {
            shared,
            delegations: Arc::new(RwLock::new(HashMap::new())),
            eip712_signer: Arc::new(EIP712Signer::default_domain()),
            agent_signer: Arc::new(AgentSigner::default_domain()),
            store: None,
            security_policy,
        }
    }

    pub fn with_store(shared: SharedState, store: Arc<dyn PersistentStore + Send + Sync>) -> Self {
        Self::with_store_and_policy(shared, store, ApiSecurityPolicy::for_mode(Mode::from_env()))
    }

    pub fn with_store_and_policy(
        shared: SharedState,
        store: Arc<dyn PersistentStore + Send + Sync>,
        security_policy: ApiSecurityPolicy,
    ) -> Self {
        Self {
            shared,
            delegations: Arc::new(RwLock::new(HashMap::new())),
            eip712_signer: Arc::new(EIP712Signer::default_domain()),
            agent_signer: Arc::new(AgentSigner::default_domain()),
            store: Some(store),
            security_policy,
        }
    }

    /// Admit a canonical signed envelope locally, then publish it through the
    /// live node's outbound transport handle after releasing the app lock.
    /// Network publication is best-effort: local canonical admission remains
    /// the API result, while a connected validator can still relay the exact
    /// signed envelope to the current leader.
    pub async fn submit_user_envelope(
        &self,
        envelope: SignedEnvelope,
        admission_timestamp: u64,
    ) -> Result<crate::types::Hash, AppError> {
        let hash = {
            let mut app =
                self.shared.app.write().map_err(|_| {
                    AppError::InvalidEnvelope("application state lock poisoned".into())
                })?;
            app.submit_envelope_at(envelope.clone(), admission_timestamp)?
        };

        if let Err(error) = self.shared.publish_user_transaction(envelope).await {
            tracing::warn!(
                tx_hash = %hex::encode(hash),
                error = %error,
                "admitted user transaction could not be published"
            );
        }

        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn security_policy_only_skips_signatures_in_dev() {
        assert!(ApiSecurityPolicy::new(Mode::Dev, true).skip_signature_verification);
        assert!(!ApiSecurityPolicy::new(Mode::Testnet, true).skip_signature_verification);
        assert!(!ApiSecurityPolicy::new(Mode::Mainnet, true).skip_signature_verification);
    }

    #[test]
    fn submission_responses_expose_pending_full_transaction_hash() {
        let tx_hash = "ab".repeat(32);

        let order = serde_json::to_value(SubmitOrderResponse {
            status: "pending".to_string(),
            tx_hash: tx_hash.clone(),
            message: None,
        })
        .unwrap();
        assert_eq!(order["status"], "pending");
        assert_eq!(order["tx_hash"], tx_hash);
        assert!(order.get("orderId").is_none());
        assert!(order.get("message").is_none());

        let trigger = serde_json::to_value(PlaceTriggerOrderResponse {
            status: "pending".to_string(),
            tx_hash: "cd".repeat(32),
        })
        .unwrap();
        assert_eq!(trigger["status"], "pending");
        assert_eq!(trigger["tx_hash"].as_str().unwrap().len(), 64);
        assert!(trigger.get("triggerOrderId").is_none());
    }
}

// =============================================================================
// Market Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct MarketInfo {
    pub symbol: String,
    #[serde(rename = "baseAsset")]
    pub base_asset: String,
    #[serde(rename = "quoteAsset")]
    pub quote_asset: String,
    #[serde(rename = "type")]
    pub market_type: String,
    pub status: String,
    #[serde(rename = "tickSize")]
    pub tick_size: i64,
    #[serde(rename = "lotSize")]
    pub lot_size: i64,
    #[serde(rename = "maxLeverage")]
    pub max_leverage: i32,
    #[serde(rename = "takerFeeBps")]
    pub taker_fee_bps: i64,
    #[serde(rename = "makerFeeBps")]
    pub maker_fee_bps: i64,
}

impl Default for MarketInfo {
    fn default() -> Self {
        Self {
            symbol: "BTC-USDT".to_string(),
            base_asset: "BTC".to_string(),
            quote_asset: "USDT".to_string(),
            market_type: "perp".to_string(),
            status: "active".to_string(),
            tick_size: 1,
            lot_size: 1,
            max_leverage: 50,
            taker_fee_bps: 5,
            maker_fee_bps: 2,
        }
    }
}

impl MarketInfo {
    /// Build MarketInfo from a MarketConfig
    pub fn from_config(config: &crate::app::MarketConfig) -> Self {
        let (base, quote) = config
            .symbol
            .split_once('-')
            .unwrap_or((&config.symbol, "USDT"));
        Self {
            symbol: config.symbol.clone(),
            base_asset: base.to_string(),
            quote_asset: quote.to_string(),
            market_type: "perp".to_string(),
            status: "active".to_string(),
            tick_size: config.tick_size,
            lot_size: config.lot_size,
            max_leverage: 50,
            taker_fee_bps: config.taker_fee,
            maker_fee_bps: config.maker_fee,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct OrderbookSnapshot {
    pub symbol: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
    pub timestamp: i64,
}

// =============================================================================
// Account Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct AccountInfo {
    pub address: String,
    pub balance: i64,
    /// Liquid native HYCK in base units.
    pub hyck_balance: i64,
    /// Liquid native HYCK expressed in whole HYCK for display clients.
    pub hyck_balance_hyck: f64,
    pub nonce: u64,
    #[serde(rename = "lockedCollateral")]
    pub locked_collateral: i64,
    #[serde(rename = "availableBalance")]
    pub available_balance: i64,
    #[serde(rename = "unrealizedPnL")]
    pub unrealized_pnl: i64,
    #[serde(rename = "totalEquity")]
    pub total_equity: i64,
}

#[derive(Debug, Serialize)]
pub struct PositionInfo {
    pub symbol: String,
    pub size: i64,
    #[serde(rename = "entryPrice")]
    pub entry_price: i64,
    #[serde(rename = "markPrice")]
    pub mark_price: i64,
    #[serde(rename = "liquidationPrice")]
    pub liquidation_price: i64,
    #[serde(rename = "unrealizedPnl")]
    pub unrealized_pnl: i64,
    pub margin: i64,
    pub leverage: f64,
}

#[derive(Debug, Serialize)]
pub struct OrderInfo {
    pub id: String,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub price: i64,
    pub size: i64,
    pub filled: i64,
    pub status: String,
    pub timestamp: u64,
}

// =============================================================================
// Chain Status
// =============================================================================

#[derive(Debug, Serialize)]
pub struct ChainStatus {
    pub height: u64,
    pub view: u64,
    #[serde(rename = "avgBlockTime")]
    pub avg_block_time: f64,
    #[serde(rename = "mempoolSize")]
    pub mempool_size: usize,
    pub validators: usize,
}

// =============================================================================
// Candle (OHLCV) Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct CandleInfo {
    /// Candle open time (ms since epoch)
    #[serde(rename = "t")]
    pub time: u64,
    /// Open price (cents)
    #[serde(rename = "o")]
    pub open: i64,
    /// High price (cents)
    #[serde(rename = "h")]
    pub high: i64,
    /// Low price (cents)
    #[serde(rename = "l")]
    pub low: i64,
    /// Close price (cents)
    #[serde(rename = "c")]
    pub close: i64,
    /// Volume (satoshis)
    #[serde(rename = "v")]
    pub volume: i64,
    /// Number of trades
    #[serde(rename = "n")]
    pub trades: u32,
}

// =============================================================================
// Funding Types
// =============================================================================

#[derive(Debug, Serialize)]
pub struct FundingInfo {
    pub symbol: String,
    #[serde(rename = "fundingRate")]
    pub funding_rate: f64,
    #[serde(rename = "fundingRateBps")]
    pub funding_rate_bps: i64,
    #[serde(rename = "nextFundingTime")]
    pub next_funding_time: u64,
    #[serde(rename = "lastFundingTime")]
    pub last_funding_time: u64,
}

#[derive(Debug, Serialize)]
pub struct FundingPayment {
    pub symbol: String,
    pub payment: i64,
    #[serde(rename = "paymentUsd")]
    pub payment_usd: f64,
    #[serde(rename = "fundingRate")]
    pub funding_rate_bps: i64,
    pub timestamp: u64,
}

// =============================================================================
// Order Submission Types
// =============================================================================

/// Order details within SignedTransaction
#[derive(Debug, Deserialize)]
pub struct OrderDetails {
    pub symbol: String,
    pub side: u8,
    #[serde(rename = "type")]
    pub order_type: u8,
    pub price: String,
    pub qty: String,
    pub nonce: String,
    pub deadline: String,
    /// Optional canonical envelope lower validity bound (milliseconds).
    #[serde(default, rename = "validAfter", alias = "valid_after")]
    pub valid_after: Option<String>,
    pub leverage: u8,
    pub owner: String,
    pub reduce_only: Option<bool>,
}

/// Cancel details within SignedTransaction
#[derive(Debug, Deserialize)]
pub struct CancelDetails {
    pub order_id: String,
    pub symbol: String,
    pub nonce: String,
    pub owner: String,
    /// Required by canonical envelope submissions; absent in legacy EIP-712.
    #[serde(default)]
    pub deadline: Option<String>,
    /// Optional canonical envelope lower validity bound (milliseconds).
    #[serde(default, rename = "validAfter", alias = "valid_after")]
    pub valid_after: Option<String>,
}

/// Signed transaction from frontend
#[derive(Debug, Deserialize)]
pub struct SignedTransaction {
    #[serde(rename = "type")]
    pub tx_type: String,
    pub order: Option<OrderDetails>,
    pub cancel: Option<CancelDetails>,
    pub signature: String,
    /// `eip712-v1` authenticates the canonical consensus envelope
    /// bytes.  When omitted, the request uses the legacy EIP-712 format,
    /// which cannot be placed into a production consensus block by itself.
    #[serde(default, rename = "signatureScheme", alias = "signature_scheme")]
    pub signature_scheme: Option<String>,
    pub agent_mode: Option<bool>,
    pub delegation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitOrderResponse {
    pub status: String,
    /// Canonical hash of the admitted signed envelope, encoded as 64
    /// lowercase hexadecimal characters.
    pub tx_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// =============================================================================
// Delegation Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct RegisterDelegationRequest {
    pub wallet: String,
    pub agent: String,
    pub expiration: String,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterDelegationResponse {
    pub status: String,
    #[serde(rename = "delegationId")]
    pub delegation_id: String,
    pub message: String,
}

// =============================================================================
// Legacy Types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct LegacyOrderRequest {
    pub trader: String,
    pub symbol: String,
    pub side: String,
    pub price: i64,
    pub size: i64,
    pub order_type: String,
}

#[derive(Debug, Deserialize)]
pub struct DepositRequest {
    pub trader: String,
    pub amount: i64,
}

#[derive(Debug, Deserialize)]
pub struct WithdrawRequest {
    pub trader: String,
    pub amount: i64,
}

// =============================================================================
// Trigger Order Types
// =============================================================================

/// Trigger order info for API response
#[derive(Debug, Serialize)]
pub struct TriggerOrderInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cloid: Option<String>,
    pub symbol: String,
    pub side: String,
    #[serde(rename = "triggerType")]
    pub trigger_type: String,
    #[serde(rename = "triggerPrice")]
    pub trigger_price: i64,
    pub size: i64,
    #[serde(rename = "limitPrice", skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<i64>,
    pub status: String,
    pub timestamp: u64,
}

/// Trigger order details within signed request
#[derive(Debug, Deserialize)]
pub struct TriggerOrderDetails {
    pub symbol: String,
    #[serde(rename = "triggerType")]
    pub trigger_type: u8, // 1 = StopLoss, 2 = TakeProfit
    #[serde(rename = "triggerPrice")]
    pub trigger_price: String, // BigInt as string
    pub size: String, // BigInt as string
    #[serde(rename = "limitPrice")]
    pub limit_price: String, // BigInt as string (0 = no limit)
    pub nonce: String, // BigInt as string
    pub owner: String, // Address
    /// Canonical envelope validity deadline (milliseconds).  The legacy
    /// TriggerOrder EIP-712 struct does not authenticate this field.
    #[serde(default)]
    pub deadline: Option<String>,
    /// Optional canonical envelope lower validity bound (milliseconds).
    #[serde(default, rename = "validAfter", alias = "valid_after")]
    pub valid_after: Option<String>,
    pub cloid: Option<String>,
}

/// Cancel trigger order details within signed request
#[derive(Debug, Deserialize)]
pub struct CancelTriggerOrderDetails {
    #[serde(rename = "triggerOrderId")]
    pub trigger_order_id: Option<String>,
    pub symbol: Option<String>,
    pub nonce: String, // BigInt as string
    pub owner: String, // Address
    /// Canonical envelope validity deadline (milliseconds).  The legacy
    /// trigger-cancel EIP-712 struct does not authenticate this field.
    #[serde(default)]
    pub deadline: Option<String>,
    /// Optional canonical envelope lower validity bound (milliseconds).
    #[serde(default, rename = "validAfter", alias = "valid_after")]
    pub valid_after: Option<String>,
    pub cloid: Option<String>,
}

/// Request to place a trigger order (signed)
#[derive(Debug, Deserialize)]
pub struct PlaceTriggerOrderRequest {
    pub trigger: TriggerOrderDetails,
    pub signature: String,
    #[serde(default, rename = "signatureScheme", alias = "signature_scheme")]
    pub signature_scheme: Option<String>,
    pub agent_mode: Option<bool>,
    pub delegation_id: Option<String>,
}

/// Response after placing a trigger order
#[derive(Debug, Serialize)]
pub struct PlaceTriggerOrderResponse {
    pub status: String,
    /// Canonical hash of the admitted signed envelope, encoded as 64
    /// lowercase hexadecimal characters.
    pub tx_hash: String,
}

/// Request to cancel a trigger order (signed)
#[derive(Debug, Deserialize)]
pub struct CancelTriggerOrderRequest {
    pub cancel: CancelTriggerOrderDetails,
    pub signature: String,
    #[serde(default, rename = "signatureScheme", alias = "signature_scheme")]
    pub signature_scheme: Option<String>,
    pub agent_mode: Option<bool>,
    pub delegation_id: Option<String>,
}

// =============================================================================
// Fill Types
// =============================================================================

/// User fill info for API response
#[derive(Debug, Serialize)]
pub struct FillInfo {
    pub id: String,
    pub symbol: String,
    pub side: String,
    pub price: i64,
    pub size: i64,
    pub fee: i64,
    #[serde(rename = "isMaker")]
    pub is_maker: bool,
    pub timestamp: u64,
}

// =============================================================================
// Sync Types
// =============================================================================

/// Node sync status
#[derive(Debug, Serialize)]
pub struct SyncStatus {
    pub height: u64,
    pub view: u64,
    #[serde(rename = "committedHash")]
    pub committed_hash: String,
    #[serde(rename = "stateHash")]
    pub state_hash: String,
    pub timestamp: u64,
    #[serde(rename = "latestSnapshotHeight")]
    pub latest_snapshot_height: Option<u64>,
    #[serde(rename = "isPersistent")]
    pub is_persistent: bool,
}

/// Certificate export for sync (QC verification)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertificateExport {
    pub epoch: u64,
    #[serde(rename = "committeeHash")]
    pub committee_hash: String,
    #[serde(rename = "genesisHash")]
    pub genesis_hash: String,
    pub view: u64,
    #[serde(rename = "blockHash")]
    pub block_hash: String, // hex
    /// App state hash that all voters agreed on (required for BLS verification)
    #[serde(rename = "appHash", skip_serializing_if = "Option::is_none", default)]
    pub app_hash: Option<String>, // hex
    /// Voters who contributed (NodeId hex strings)
    pub voters: Vec<String>,
    /// BLS public keys (hex, 48 bytes each)
    #[serde(rename = "blsPubkeys", skip_serializing_if = "Vec::is_empty", default)]
    pub bls_pubkeys: Vec<String>,
    /// Aggregated signature (hex, 96 bytes for BLS)
    #[serde(rename = "aggSignature")]
    pub agg_signature: String,
}

/// Block export for sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockExport {
    pub epoch: u64,
    #[serde(rename = "committeeHash")]
    pub committee_hash: String,
    #[serde(rename = "genesisHash")]
    pub genesis_hash: String,
    pub height: u64,
    pub view: u64,
    pub hash: String,
    #[serde(rename = "parentHash")]
    pub parent_hash: String,
    #[serde(rename = "appHash")]
    pub app_hash: String,
    #[serde(rename = "commitmentRoot")]
    pub commitment_root: String,
    pub proposer: String,
    pub timestamp: u64,
    #[serde(rename = "payloadSize")]
    pub payload_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// QC that justifies this block (proves parent was certified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justify: Option<CertificateExport>,
}

/// Two-chain finality proof for a committed target block.
///
/// `target.justify` certifies the target's parent and `commitQc` certifies the
/// exact child.  The endpoint only emits this envelope after both certificates
/// have been checked against the node's trusted committee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalityProofExport {
    pub target: BlockExport,
    pub child: BlockExport,
    #[serde(rename = "commitQc")]
    pub commit_qc: CertificateExport,
}

/// Short alias used by callers that refer to the response as a finality proof.
pub type FinalityProof = FinalityProofExport;

/// Block range query parameters
#[derive(Debug, Deserialize)]
pub struct BlockRangeQuery {
    pub from: u64,
    pub to: Option<u64>,
    pub limit: Option<u64>,
    #[serde(rename = "includePayload", default)]
    pub include_payload: bool,
}

/// Block range response with pagination
#[derive(Debug, Serialize)]
pub struct BlockRangeResponse {
    pub blocks: Vec<BlockExport>,
    #[serde(rename = "nextHeight")]
    pub next_height: Option<u64>,
    #[serde(rename = "totalAvailable")]
    pub total_available: u64,
}

/// Snapshot metadata
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotMetadata {
    pub height: u64,
    pub timestamp: u64,
    #[serde(rename = "stateHash")]
    pub state_hash: String,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    #[serde(rename = "accountCount")]
    pub account_count: usize,
    #[serde(rename = "marketCount")]
    pub market_count: usize,
}

/// Full snapshot export
#[derive(Debug, Serialize)]
pub struct SnapshotExport {
    pub metadata: SnapshotMetadata,
    pub data: String, // base64 encoded
}

// =============================================================================
// Admin Types
// =============================================================================

/// Market config details for AddMarket request (all strings for EIP-712 compat)
#[derive(Debug, Deserialize)]
pub struct AddMarketConfigDetails {
    pub symbol: String,
    pub tick_size: String,
    pub lot_size: String,
    pub min_notional: String,
    pub maker_fee: String,
    pub taker_fee: String,
}

/// Request to add a new market (admin only, EIP-712 signed)
#[derive(Debug, Deserialize)]
pub struct AddMarketRequest {
    pub admin: String,
    pub config: AddMarketConfigDetails,
    pub initial_mark_price: String,
    pub nonce: String,
    pub signature: String,
    #[serde(default, rename = "signatureScheme", alias = "signature_scheme")]
    pub signature_scheme: Option<String>,
    /// Canonical envelope expiry in milliseconds.
    #[serde(default)]
    pub deadline: Option<String>,
    /// Optional canonical envelope lower validity bound (milliseconds).
    #[serde(default, rename = "validAfter", alias = "valid_after")]
    pub valid_after: Option<String>,
}

/// Response after adding a market
#[derive(Debug, Serialize)]
pub struct AddMarketResponse {
    pub status: String,
    pub symbol: String,
}
