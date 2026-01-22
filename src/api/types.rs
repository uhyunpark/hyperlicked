//! API Types
//!
//! Request and response types for the REST API.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde::{Deserialize, Serialize};

use super::state::{PriceLevel, SharedState};
use crate::crypto::{AgentDelegation, AgentSigner, EIP712Signer};
use crate::storage::PersistentStore;

/// Stored delegation with signature
#[derive(Clone)]
pub struct StoredDelegation {
    pub delegation: AgentDelegation,
    pub signature: Vec<u8>,
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
}

impl ApiState {
    pub fn new(shared: SharedState) -> Self {
        Self {
            shared,
            delegations: Arc::new(RwLock::new(HashMap::new())),
            eip712_signer: Arc::new(EIP712Signer::default_domain()),
            agent_signer: Arc::new(AgentSigner::default_domain()),
            store: None,
        }
    }

    pub fn with_store(shared: SharedState, store: Arc<dyn PersistentStore + Send + Sync>) -> Self {
        Self {
            shared,
            delegations: Arc::new(RwLock::new(HashMap::new())),
            eip712_signer: Arc::new(EIP712Signer::default_domain()),
            agent_signer: Arc::new(AgentSigner::default_domain()),
            store: Some(store),
        }
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
}

/// Signed transaction from frontend
#[derive(Debug, Deserialize)]
pub struct SignedTransaction {
    #[serde(rename = "type")]
    pub tx_type: String,
    pub order: Option<OrderDetails>,
    pub cancel: Option<CancelDetails>,
    pub signature: String,
    pub agent_mode: Option<bool>,
    pub delegation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitOrderResponse {
    pub status: String,
    #[serde(rename = "orderId")]
    pub order_id: String,
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

/// Request to place a trigger order
#[derive(Debug, Deserialize)]
pub struct PlaceTriggerOrderRequest {
    pub trader: String,
    pub symbol: String,
    #[serde(rename = "triggerType")]
    pub trigger_type: String,  // "sl" or "tp"
    #[serde(rename = "triggerPrice")]
    pub trigger_price: i64,
    pub size: i64,
    #[serde(rename = "limitPrice")]
    pub limit_price: Option<i64>,
    pub cloid: Option<String>,
}

/// Response after placing a trigger order
#[derive(Debug, Serialize)]
pub struct PlaceTriggerOrderResponse {
    pub status: String,
    #[serde(rename = "triggerOrderId")]
    pub trigger_order_id: String,
}

/// Request to cancel a trigger order
#[derive(Debug, Deserialize)]
pub struct CancelTriggerOrderRequest {
    pub trader: String,
    #[serde(rename = "triggerOrderId")]
    pub trigger_order_id: Option<String>,
    pub symbol: Option<String>,
    pub cloid: Option<String>,
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

/// Block export for sync
#[derive(Debug, Serialize)]
pub struct BlockExport {
    pub height: u64,
    pub view: u64,
    pub hash: String,
    #[serde(rename = "parentHash")]
    pub parent_hash: String,
    #[serde(rename = "appHash")]
    pub app_hash: String,
    pub proposer: String,
    pub timestamp: u64,
    #[serde(rename = "payloadSize")]
    pub payload_size: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

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
#[derive(Debug, Serialize)]
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
