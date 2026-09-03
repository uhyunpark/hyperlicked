//! Finalized transaction receipt response types.
//!
//! The durable receipt index is the source of truth for this response.  A
//! lookup only succeeds after storage has re-authenticated the receipt against
//! its finalized block and Commitment v2 artifact.

use serde::Serialize;
use serde_json::Value;

use crate::app::{Fill, OrderUpdateInfo};
use crate::storage::TransactionReceiptLookup;
use crate::types::{EventType, ResourceUsage};

/// Public response for one finalized transaction.
///
/// `status` describes the lookup lifecycle, while `receipt_status` retains
/// the numeric Commitment v2 execution result (success or failure).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FinalizedTransactionResponse {
    pub status: String,
    pub tx_hash: String,
    pub tx_index: u32,
    pub tx_type: u16,
    pub receipt_status: u8,
    pub error_code: u16,
    pub resource_usage: ResourceUsage,
    pub events: Vec<FinalizedEventResponse>,
    pub block: FinalizedBlockInfo,
}

/// Finalized block location for a transaction receipt.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FinalizedBlockInfo {
    pub hash: String,
    pub height: u64,
}

/// One event attached to a finalized transaction receipt.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FinalizedEventResponse {
    pub event_index: u32,
    pub event_type: u16,
    pub event_name: String,
    pub payload_hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// Convert an authenticated durable receipt lookup into its API/WebSocket
/// representation.
pub(crate) fn finalized_transaction_response(
    lookup: &TransactionReceiptLookup,
) -> FinalizedTransactionResponse {
    let receipt = &lookup.receipt;
    FinalizedTransactionResponse {
        status: "finalized".to_string(),
        tx_hash: hex::encode(lookup.tx_id),
        tx_index: lookup.tx_index,
        tx_type: receipt.tx_type.0,
        receipt_status: receipt.status.0,
        error_code: receipt.error_code.0,
        resource_usage: receipt.resource_usage,
        events: receipt
            .events
            .iter()
            .map(finalized_event_response)
            .collect(),
        block: FinalizedBlockInfo {
            hash: hex::encode(lookup.block_hash),
            height: lookup.block_height,
        },
    }
}

fn finalized_event_response(event: &crate::types::EventRecord) -> FinalizedEventResponse {
    FinalizedEventResponse {
        event_index: event.event_index,
        event_type: event.event_type.0,
        event_name: event_name(event.event_type).to_string(),
        payload_hex: hex::encode(&event.payload),
        payload: decode_known_payload(event.event_type, &event.payload),
    }
}

fn event_name(event_type: EventType) -> &'static str {
    match event_type.0 {
        1 => "ORDER_UPDATE",
        2 => "FILL",
        3 => "DEPOSIT",
        4 => "WITHDRAW",
        5 => "LIQUIDATION",
        6 => "FUNDING",
        7 => "STAKING",
        8 => "TRIGGER",
        9 => "ADL",
        10 => "EPOCH",
        11 => "ORACLE",
        12 => "MARKET",
        13 => "TRANSFER_HYCK",
        _ => "UNKNOWN",
    }
}

fn decode_known_payload(event_type: EventType, payload: &[u8]) -> Option<Value> {
    match event_type.0 {
        value if value == EventType::ORDER_UPDATE.0 => decode_bincode::<OrderUpdateInfo>(payload),
        value if value == EventType::FILL.0 => decode_bincode::<Fill>(payload),
        _ => None,
    }
}

fn decode_bincode<T>(payload: &[u8]) -> Option<Value>
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let decoded = bincode::deserialize::<T>(payload).ok()?;
    if bincode::serialize(&decoded).ok()?.as_slice() != payload {
        return None;
    }
    serde_json::to_value(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ErrorCode, EventRecord, ReceiptStatus, TransactionReceipt, TransactionType,
    };

    fn lookup(
        events: Vec<EventRecord>,
        status: ReceiptStatus,
        error_code: ErrorCode,
    ) -> TransactionReceiptLookup {
        let tx_id = [0x11; 32];
        let receipt = TransactionReceipt::new(
            3,
            tx_id,
            TransactionType::PLACE_ORDER,
            status,
            error_code,
            ResourceUsage {
                compute_units: 7,
                storage_read_bytes: 8,
                storage_write_bytes: 9,
            },
            events,
        )
        .expect("receipt should be valid");
        TransactionReceiptLookup {
            tx_id,
            block_hash: [0x22; 32],
            block_height: 42,
            tx_index: 3,
            receipt,
        }
    }

    #[test]
    fn response_contains_full_receipt_and_block_metadata() {
        let response = finalized_transaction_response(&lookup(
            Vec::new(),
            ReceiptStatus::SUCCESS,
            ErrorCode::NONE,
        ));
        let json = serde_json::to_value(response).expect("response serializes");

        assert_eq!(json["status"], "finalized");
        assert_eq!(json["tx_hash"], "11".repeat(32));
        assert_eq!(json["tx_index"], 3);
        assert_eq!(json["tx_type"], TransactionType::PLACE_ORDER.0);
        assert_eq!(json["receipt_status"], ReceiptStatus::SUCCESS.0);
        assert_eq!(json["error_code"], ErrorCode::NONE.0);
        assert_eq!(json["resource_usage"]["compute_units"], 7);
        assert_eq!(json["block"]["hash"], "22".repeat(32));
        assert_eq!(json["block"]["height"], 42);
    }

    #[test]
    fn known_event_payloads_decode_and_unknown_payloads_keep_raw_bytes() {
        let fill = Fill {
            taker_order_id: "taker-1".to_string(),
            maker_order_id: "maker-1".to_string(),
            taker: "0xtaker".to_string(),
            maker: "0xmaker".to_string(),
            symbol: "BTC-USDT".to_string(),
            side: crate::app::Side::Bid,
            price: 5_000_000,
            size: 10,
            timestamp: 99,
            maker_locked_margin: 1,
            maker_original_size: 10,
        };
        let update = OrderUpdateInfo {
            trader: "alice".to_string(),
            order_id: "order-42".to_string(),
            symbol: "BTC-USDT".to_string(),
            side: "buy".to_string(),
            price: 5_000_000,
            original_size: 10,
            status: "open".to_string(),
            filled: 0,
            remaining: 10,
        };
        let update_event = EventRecord::from_bincode(0, EventType::ORDER_UPDATE, &update)
            .expect("order update encodes");
        let fill_event =
            EventRecord::from_bincode(1, EventType::FILL, &fill).expect("fill encodes");
        let unknown_event = EventRecord::new(2, EventType(999), vec![0xde, 0xad])
            .expect("unknown event payload is structurally valid");
        let response = finalized_transaction_response(&lookup(
            vec![update_event, fill_event, unknown_event],
            ReceiptStatus::FAILURE,
            ErrorCode::ORDER_BOOK,
        ));
        let json = serde_json::to_value(response).expect("response serializes");

        assert_eq!(json["events"][0]["event_index"], 0);
        assert_eq!(json["events"][0]["event_type"], EventType::ORDER_UPDATE.0);
        assert_eq!(json["events"][0]["event_name"], "ORDER_UPDATE");
        assert_eq!(json["events"][0]["payload"]["order_id"], "order-42");
        assert_eq!(
            json["events"][0]["payload_hex"],
            hex::encode(bincode::serialize(&update).unwrap())
        );

        assert_eq!(json["events"][1]["event_name"], "FILL");
        assert_eq!(json["events"][1]["payload"]["taker_order_id"], "taker-1");
        assert_eq!(json["events"][1]["payload"]["maker_order_id"], "maker-1");
        assert_eq!(json["events"][1]["payload"]["size"], 10);
        assert_eq!(
            json["events"][1]["payload_hex"],
            hex::encode(bincode::serialize(&fill).unwrap())
        );

        assert_eq!(json["events"][2]["event_name"], "UNKNOWN");
        assert_eq!(json["events"][2]["payload_hex"], "dead");
        assert!(json["events"][2].get("payload").is_none());
        assert_eq!(json["receipt_status"], ReceiptStatus::FAILURE.0);
        assert_eq!(json["error_code"], ErrorCode::ORDER_BOOK.0);
    }
}
