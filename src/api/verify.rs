//! Transaction Signature Verification
//!
//! Verifies EIP-712 signatures for orders and cancels.
//! Supports both direct wallet signing and agent delegation.

use alloy_primitives::{Address, U256};

use crate::config::Config;
use crate::crypto::{
    verify_agent_order, AddMarketEIP712, AgentSigner, CancelEIP712, CancelTriggerOrderEIP712,
    EIP712Signer, OrderEIP712, TriggerOrderEIP712,
};

use super::types::{
    AddMarketRequest, CancelTriggerOrderRequest, OrderDetails, PlaceTriggerOrderRequest,
    SignedTransaction, StoredDelegation,
};

/// Result of successful order verification
pub struct VerifiedOrder {
    pub owner: Address,
    pub symbol: String,
    pub side: u8,
    pub order_type: u8,
    pub price: i64,
    pub size: i64,
    pub nonce: u64,
    pub reduce_only: bool,
}

/// Result of successful cancel verification
pub struct VerifiedCancel {
    pub owner: Address,
    pub order_id: String,
    pub nonce: u64,
}

/// Result of successful trigger order verification
pub struct VerifiedTriggerOrder {
    pub owner: Address,
    pub symbol: String,
    pub trigger_type: u8, // 1 = StopLoss, 2 = TakeProfit
    pub trigger_price: i64,
    pub size: i64,
    pub limit_price: Option<i64>,
    #[allow(dead_code)] // Used for verification only, not consumed by route
    pub nonce: u64,
    pub cloid: Option<String>,
}

/// Result of successful cancel trigger order verification
pub struct VerifiedCancelTriggerOrder {
    pub owner: Address,
    pub trigger_order_id: Option<String>,
    pub symbol: Option<String>,
    pub cloid: Option<String>,
    #[allow(dead_code)] // Used for verification only, not consumed by route
    pub nonce: u64,
}

/// Verification errors
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("invalid signature")]
    InvalidSignature,
    #[error("signature recovery failed: {0}")]
    RecoveryFailed(String),
    #[error("missing order details")]
    MissingOrder,
    #[error("missing cancel details")]
    MissingCancel,
    #[error("invalid trigger price format")]
    InvalidTriggerPrice,
    #[error("invalid price format")]
    InvalidPrice,
    #[error("invalid quantity format")]
    InvalidQuantity,
    #[error("invalid size format")]
    InvalidSize,
    #[error("invalid nonce format")]
    InvalidNonce,
    #[error("invalid owner address")]
    InvalidOwner,
    #[error("delegation not found")]
    DelegationNotFound,
    #[error("agent error: {0}")]
    AgentError(String),
}

/// Verify an order signature and extract verified data
///
/// Parameters:
/// - `block_timestamp_ms`: Block timestamp in milliseconds for agent delegation expiration check
pub fn verify_order(
    tx: &SignedTransaction,
    eip712: &EIP712Signer,
    agent_signer: &AgentSigner,
    delegation: Option<&StoredDelegation>,
    block_timestamp_ms: u64,
) -> Result<VerifiedOrder, VerifyError> {
    let order = tx.order.as_ref().ok_or(VerifyError::MissingOrder)?;

    // Parse signature
    let sig_hex = tx.signature.strip_prefix("0x").unwrap_or(&tx.signature);
    let signature = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;

    // Parse owner address
    let owner: Address = order.owner.parse().map_err(|_| VerifyError::InvalidOwner)?;

    // Build OrderEIP712 for verification
    let order_eip712 = build_order_eip712(order, owner)?;

    // Verify based on mode (direct or agent)
    if tx.agent_mode.unwrap_or(false) {
        verify_agent_order_sig(&order_eip712, &signature, delegation, eip712, agent_signer, block_timestamp_ms)?;
    } else {
        // Skip verification in dev mode if configured
        if !Config::global().skip_signature_verification {
            let valid = eip712
                .verify_order_signature(&order_eip712, &signature)
                .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;
            if !valid {
                return Err(VerifyError::InvalidSignature);
            }
        }
    }

    // Parse numeric fields
    let price: i64 = order.price.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let size: i64 = order.qty.parse().map_err(|_| VerifyError::InvalidQuantity)?;
    let nonce: u64 = order.nonce.parse().map_err(|_| VerifyError::InvalidNonce)?;

    Ok(VerifiedOrder {
        owner,
        symbol: order.symbol.clone(),
        side: order.side,
        order_type: order.order_type,
        price,
        size,
        nonce,
        reduce_only: order.reduce_only.unwrap_or(false),
    })
}

/// Verify a cancel signature and extract verified data
pub fn verify_cancel(
    tx: &SignedTransaction,
    eip712: &EIP712Signer,
) -> Result<VerifiedCancel, VerifyError> {
    let cancel = tx.cancel.as_ref().ok_or(VerifyError::MissingCancel)?;

    // Parse signature
    let sig_hex = tx.signature.strip_prefix("0x").unwrap_or(&tx.signature);
    let signature = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;

    // Parse owner address
    let owner: Address = cancel.owner.parse().map_err(|_| VerifyError::InvalidOwner)?;

    // Build CancelEIP712
    let cancel_eip712 = CancelEIP712 {
        order_id: cancel.order_id.clone(),
        symbol: cancel.symbol.clone(),
        nonce: U256::from_str_radix(&cancel.nonce, 10).map_err(|_| VerifyError::InvalidNonce)?,
        owner,
    };

    // Skip verification in dev mode if configured
    if !Config::global().skip_signature_verification {
        let valid = eip712
            .verify_cancel_signature(&cancel_eip712, &signature)
            .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;
        if !valid {
            return Err(VerifyError::InvalidSignature);
        }
    }

    let nonce: u64 = cancel.nonce.parse().map_err(|_| VerifyError::InvalidNonce)?;

    Ok(VerifiedCancel {
        owner,
        order_id: cancel.order_id.clone(),
        nonce,
    })
}

/// Build OrderEIP712 from API request
fn build_order_eip712(order: &OrderDetails, owner: Address) -> Result<OrderEIP712, VerifyError> {
    Ok(OrderEIP712 {
        symbol: order.symbol.clone(),
        side: order.side,
        order_type: order.order_type,
        price: U256::from_str_radix(&order.price, 10).map_err(|_| VerifyError::InvalidPrice)?,
        qty: U256::from_str_radix(&order.qty, 10).map_err(|_| VerifyError::InvalidQuantity)?,
        nonce: U256::from_str_radix(&order.nonce, 10).map_err(|_| VerifyError::InvalidNonce)?,
        deadline: U256::from_str_radix(&order.deadline, 10).unwrap_or(U256::ZERO),
        leverage: order.leverage,
        owner,
    })
}

/// Verify a trigger order signature and extract verified data
pub fn verify_trigger_order(
    req: &PlaceTriggerOrderRequest,
    eip712: &EIP712Signer,
    agent_signer: &AgentSigner,
    delegation: Option<&StoredDelegation>,
    block_timestamp_ms: u64,
) -> Result<VerifiedTriggerOrder, VerifyError> {
    let trigger = &req.trigger;

    // Parse signature
    let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
    let signature = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;

    // Parse owner address
    let owner: Address = trigger.owner.parse().map_err(|_| VerifyError::InvalidOwner)?;

    // Build TriggerOrderEIP712 for verification
    let trigger_eip712 = TriggerOrderEIP712 {
        symbol: trigger.symbol.clone(),
        trigger_type: trigger.trigger_type,
        trigger_price: U256::from_str_radix(&trigger.trigger_price, 10)
            .map_err(|_| VerifyError::InvalidTriggerPrice)?,
        size: U256::from_str_radix(&trigger.size, 10)
            .map_err(|_| VerifyError::InvalidSize)?,
        limit_price: U256::from_str_radix(&trigger.limit_price, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        nonce: U256::from_str_radix(&trigger.nonce, 10)
            .map_err(|_| VerifyError::InvalidNonce)?,
        owner,
    };

    // Verify based on mode (direct or agent)
    if req.agent_mode.unwrap_or(false) {
        verify_agent_trigger_order_sig(
            &trigger_eip712, &signature, delegation, eip712, agent_signer, block_timestamp_ms,
        )?;
    } else if !Config::global().skip_signature_verification {
        let valid = eip712
            .verify_trigger_order_signature(&trigger_eip712, &signature)
            .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;
        if !valid {
            return Err(VerifyError::InvalidSignature);
        }
    }

    // Parse numeric fields
    let trigger_price: i64 = trigger.trigger_price.parse().map_err(|_| VerifyError::InvalidTriggerPrice)?;
    let size: i64 = trigger.size.parse().map_err(|_| VerifyError::InvalidSize)?;
    let limit_price_raw: i64 = trigger.limit_price.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let limit_price = if limit_price_raw == 0 { None } else { Some(limit_price_raw) };
    let nonce: u64 = trigger.nonce.parse().map_err(|_| VerifyError::InvalidNonce)?;

    Ok(VerifiedTriggerOrder {
        owner,
        symbol: trigger.symbol.clone(),
        trigger_type: trigger.trigger_type,
        trigger_price,
        size,
        limit_price,
        nonce,
        cloid: trigger.cloid.clone(),
    })
}

/// Verify a cancel trigger order signature and extract verified data
pub fn verify_cancel_trigger_order(
    req: &CancelTriggerOrderRequest,
    eip712: &EIP712Signer,
) -> Result<VerifiedCancelTriggerOrder, VerifyError> {
    let cancel = &req.cancel;

    // Parse signature
    let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
    let signature = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;

    // Parse owner address
    let owner: Address = cancel.owner.parse().map_err(|_| VerifyError::InvalidOwner)?;

    // Build CancelTriggerOrderEIP712 — use trigger_order_id if present, else cloid
    let id_for_signing = cancel.trigger_order_id.clone().unwrap_or_default();
    let symbol_for_signing = cancel.symbol.clone().unwrap_or_default();

    let cancel_eip712 = CancelTriggerOrderEIP712 {
        trigger_order_id: id_for_signing,
        symbol: symbol_for_signing,
        nonce: U256::from_str_radix(&cancel.nonce, 10).map_err(|_| VerifyError::InvalidNonce)?,
        owner,
    };

    // Skip verification in dev mode if configured
    if !Config::global().skip_signature_verification {
        let valid = eip712
            .verify_cancel_trigger_order_signature(&cancel_eip712, &signature)
            .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;
        if !valid {
            return Err(VerifyError::InvalidSignature);
        }
    }

    let nonce: u64 = cancel.nonce.parse().map_err(|_| VerifyError::InvalidNonce)?;

    Ok(VerifiedCancelTriggerOrder {
        owner,
        trigger_order_id: cancel.trigger_order_id.clone(),
        symbol: cancel.symbol.clone(),
        cloid: cancel.cloid.clone(),
        nonce,
    })
}

/// Verify trigger order with agent delegation
fn verify_agent_trigger_order_sig(
    trigger: &TriggerOrderEIP712,
    agent_signature: &[u8],
    delegation: Option<&StoredDelegation>,
    eip712: &EIP712Signer,
    agent_signer: &AgentSigner,
    block_timestamp_ms: u64,
) -> Result<(), VerifyError> {
    let stored = delegation.ok_or(VerifyError::DelegationNotFound)?;

    // Recover agent address from trigger order signature
    let order_hash = eip712.hash_trigger_order(trigger);
    let agent_addr = crate::crypto::recover_address(&order_hash.into(), agent_signature)
        .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;

    // Verify the agent matches the delegation
    if agent_addr != stored.delegation.agent {
        return Err(VerifyError::AgentError(format!(
            "agent mismatch: expected {}, got {}",
            stored.delegation.agent, agent_addr
        )));
    }

    // Verify delegation is not expired
    if stored.delegation.is_expired_at(block_timestamp_ms) {
        return Err(VerifyError::AgentError(format!(
            "delegation expired at {}",
            stored.delegation.expiration
        )));
    }

    // Verify wallet signed the delegation
    let valid = agent_signer
        .verify_delegation(&stored.delegation, &stored.signature)
        .map_err(|e| VerifyError::AgentError(e.to_string()))?;
    if !valid {
        return Err(VerifyError::AgentError("invalid delegation signature".to_string()));
    }

    // Verify trigger order owner matches delegation wallet
    if trigger.owner != stored.delegation.wallet {
        return Err(VerifyError::AgentError(format!(
            "owner mismatch: trigger owner {} != delegation wallet {}",
            trigger.owner, stored.delegation.wallet
        )));
    }

    Ok(())
}

/// Result of successful add market verification
pub struct VerifiedAddMarket {
    pub owner: Address,
    pub symbol: String,
    pub tick_size: i64,
    pub lot_size: i64,
    pub min_notional: i64,
    pub maker_fee: i64,
    pub taker_fee: i64,
    pub initial_mark_price: i64,
}

/// Verify an add market request signature
pub fn verify_add_market(
    req: &AddMarketRequest,
    eip712: &EIP712Signer,
) -> Result<VerifiedAddMarket, VerifyError> {
    // Parse signature
    let sig_hex = req.signature.strip_prefix("0x").unwrap_or(&req.signature);
    let signature = hex::decode(sig_hex).map_err(|_| VerifyError::InvalidSignature)?;

    // Parse owner address
    let owner: Address = req.admin.parse().map_err(|_| VerifyError::InvalidOwner)?;

    // Build AddMarketEIP712
    let add_market_eip712 = AddMarketEIP712 {
        symbol: req.config.symbol.clone(),
        tick_size: U256::from_str_radix(&req.config.tick_size, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        lot_size: U256::from_str_radix(&req.config.lot_size, 10)
            .map_err(|_| VerifyError::InvalidSize)?,
        min_notional: U256::from_str_radix(&req.config.min_notional, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        maker_fee: U256::from_str_radix(&req.config.maker_fee, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        taker_fee: U256::from_str_radix(&req.config.taker_fee, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        initial_mark_price: U256::from_str_radix(&req.initial_mark_price, 10)
            .map_err(|_| VerifyError::InvalidPrice)?,
        nonce: U256::from_str_radix(&req.nonce, 10)
            .map_err(|_| VerifyError::InvalidNonce)?,
        owner,
    };

    // Skip verification in dev mode if configured
    if !Config::global().skip_signature_verification {
        let valid = eip712
            .verify_add_market_signature(&add_market_eip712, &signature)
            .map_err(|e| VerifyError::RecoveryFailed(e.to_string()))?;
        if !valid {
            return Err(VerifyError::InvalidSignature);
        }
    }

    // Parse numeric fields
    let tick_size: i64 = req.config.tick_size.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let lot_size: i64 = req.config.lot_size.parse().map_err(|_| VerifyError::InvalidSize)?;
    let min_notional: i64 = req.config.min_notional.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let maker_fee: i64 = req.config.maker_fee.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let taker_fee: i64 = req.config.taker_fee.parse().map_err(|_| VerifyError::InvalidPrice)?;
    let initial_mark_price: i64 = req.initial_mark_price.parse().map_err(|_| VerifyError::InvalidPrice)?;

    Ok(VerifiedAddMarket {
        owner,
        symbol: req.config.symbol.clone(),
        tick_size,
        lot_size,
        min_notional,
        maker_fee,
        taker_fee,
        initial_mark_price,
    })
}

/// Verify order with agent delegation
fn verify_agent_order_sig(
    order: &OrderEIP712,
    agent_signature: &[u8],
    delegation: Option<&StoredDelegation>,
    eip712: &EIP712Signer,
    agent_signer: &AgentSigner,
    block_timestamp_ms: u64,
) -> Result<(), VerifyError> {
    let stored = delegation.ok_or(VerifyError::DelegationNotFound)?;

    verify_agent_order(
        order,
        agent_signature,
        &stored.delegation,
        &stored.signature,
        eip712,
        agent_signer,
        block_timestamp_ms,
    )
    .map_err(|e| VerifyError::AgentError(e.to_string()))?;

    Ok(())
}
