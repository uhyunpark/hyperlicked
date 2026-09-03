use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

use crate::app::{OrderType, Side, SignedEnvelope, Transaction};
use crate::crypto::Signer;

#[derive(Debug, Deserialize)]
pub(crate) struct GenesisBlock {
    #[serde(rename = "genesisHash")]
    pub(crate) genesis_hash: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AssetContext {
    #[serde(rename = "markPrice")]
    pub(crate) mark_price: i64,
    #[serde(rename = "oraclePrice")]
    pub(crate) oracle_price: Option<i64>,
    #[serde(rename = "midPrice")]
    pub(crate) mid_price: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PriceLevel {
    pub(crate) price: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Orderbook {
    pub(crate) bids: Vec<PriceLevel>,
    pub(crate) asks: Vec<PriceLevel>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AccountSnapshot {
    pub(crate) balance: i64,
    #[serde(rename = "lockedCollateral")]
    pub(crate) locked_collateral: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NonceResponse {
    pub(crate) nonce: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenOrder {
    pub(crate) id: String,
    pub(crate) symbol: String,
    pub(crate) status: String,
    pub(crate) timestamp: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DepositResponse {
    pub(crate) success: bool,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SubmitResponse {
    pub(crate) status: String,
    pub(crate) tx_hash: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReceiptResponse {
    pub(crate) receipt_status: u8,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct SignedRequest {
    #[serde(skip)]
    pub(crate) expected_hash: String,
    #[serde(rename = "type")]
    pub(crate) tx_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) order: Option<OrderRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cancel: Option<CancelRequest>,
    pub(crate) signature: String,
    #[serde(rename = "signatureScheme")]
    pub(crate) signature_scheme: &'static str,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct OrderRequest {
    pub(crate) symbol: String,
    pub(crate) side: u8,
    #[serde(rename = "type")]
    pub(crate) order_type: u8,
    pub(crate) price: String,
    pub(crate) qty: String,
    pub(crate) nonce: String,
    pub(crate) deadline: String,
    #[serde(rename = "validAfter")]
    pub(crate) valid_after: String,
    pub(crate) leverage: u8,
    pub(crate) owner: String,
    pub(crate) reduce_only: bool,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct CancelRequest {
    pub(crate) order_id: String,
    pub(crate) symbol: String,
    pub(crate) nonce: String,
    pub(crate) owner: String,
    pub(crate) deadline: String,
    #[serde(rename = "validAfter")]
    pub(crate) valid_after: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct DepositRequest<'a> {
    pub(crate) trader: &'a str,
    pub(crate) amount: i64,
}

pub(crate) fn signed_request(
    chain_domain: [u8; 32],
    signer: &Signer,
    nonce: u64,
    valid_until: u64,
    action: Transaction,
    cancel_symbol: Option<String>,
) -> Result<SignedRequest> {
    let owner = format!("{:?}", signer.address());
    if !action.trader_address().eq_ignore_ascii_case(&owner) {
        bail!("market-maker action trader does not match its signer");
    }
    let envelope =
        SignedEnvelope::sign(chain_domain, signer, nonce, 0, valid_until, action.clone())
            .map_err(|error| anyhow!(error.to_string()))?;
    let expected_hash = hex::encode(
        envelope
            .hash()
            .map_err(|error| anyhow!(error.to_string()))?,
    );
    let signature = format!("0x{}", hex::encode(&envelope.signature));
    match action {
        Transaction::PlaceOrder {
            symbol,
            side,
            price,
            size,
            order_type,
            reduce_only,
            ..
        } => Ok(SignedRequest {
            expected_hash,
            tx_type: "order",
            order: Some(OrderRequest {
                symbol,
                side: side_code(side),
                order_type: order_type_code(order_type),
                price: price.to_string(),
                qty: size.to_string(),
                nonce: nonce.to_string(),
                deadline: valid_until.to_string(),
                valid_after: "0".to_string(),
                leverage: 1,
                owner,
                reduce_only,
            }),
            cancel: None,
            signature,
            signature_scheme: "eip712-v1",
        }),
        Transaction::CancelOrder { order_id, .. } => Ok(SignedRequest {
            expected_hash,
            tx_type: "cancel",
            order: None,
            cancel: Some(CancelRequest {
                order_id,
                symbol: cancel_symbol.unwrap_or_default(),
                nonce: nonce.to_string(),
                owner,
                deadline: valid_until.to_string(),
                valid_after: "0".to_string(),
            }),
            signature,
            signature_scheme: "eip712-v1",
        }),
        _ => bail!("market-maker service only supports order and cancel actions"),
    }
}

fn side_code(side: Side) -> u8 {
    match side {
        Side::Bid => 1,
        Side::Ask => 2,
    }
}

fn order_type_code(order_type: OrderType) -> u8 {
    match order_type {
        OrderType::Gtc => 1,
        OrderType::Ioc => 2,
        OrderType::Alo => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{OrderType, Side, SignatureScheme};

    #[test]
    fn signed_request_uses_canonical_wire_mapping() {
        let signer =
            Signer::from_hex("0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap();
        let owner = format!("{:?}", signer.address());
        let action = Transaction::PlaceOrder {
            trader: owner.clone(),
            symbol: "BTC-USDT".to_string(),
            side: Side::Ask,
            price: 5_000_000,
            size: 100,
            order_type: OrderType::Alo,
            reduce_only: true,
        };
        let request = signed_request([3u8; 32], &signer, 4, 99, action.clone(), None).unwrap();
        let signature = hex::decode(request.signature.trim_start_matches("0x")).unwrap();
        let envelope = SignedEnvelope::new(
            [3u8; 32],
            signer.address().into_array(),
            4,
            0,
            99,
            action,
            SignatureScheme::Eip712V1,
            signature,
        )
        .unwrap();
        assert_eq!(request.expected_hash, hex::encode(envelope.hash().unwrap()));
        envelope.validate_for_block([3u8; 32], 50, false).unwrap();
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["type"], "order");
        assert_eq!(json["signatureScheme"], "eip712-v1");
        assert_eq!(json["order"]["side"], 2);
        assert_eq!(json["order"]["type"], 3);
        assert_eq!(json["order"]["nonce"], "4");
        assert_eq!(json["order"]["owner"], owner);
        assert!(json["signature"].as_str().unwrap().starts_with("0x"));
        assert_eq!(request.expected_hash.len(), 64);
        assert!(json.get("expected_hash").is_none());
    }
}
