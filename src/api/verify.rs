//! Transaction Signature Verification
//!
//! Verifies EIP-712 signatures for orders and cancels.
//! Supports both direct wallet signing and agent delegation.

use alloy_primitives::{Address, U256};

use crate::config::Config;
use crate::crypto::{
    verify_agent_order, AgentSigner, CancelEIP712, EIP712Signer, OrderEIP712,
};

use super::types::{OrderDetails, SignedTransaction, StoredDelegation};

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
    #[error("invalid price format")]
    InvalidPrice,
    #[error("invalid quantity format")]
    InvalidQuantity,
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
