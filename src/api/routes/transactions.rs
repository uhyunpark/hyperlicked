//! Finalized transaction receipt endpoint.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::api::transaction::{finalized_transaction_response, FinalizedTransactionResponse};
use crate::api::types::ApiState;
use crate::types::Hash;

/// Return one finalized transaction receipt by its canonical transaction hash.
pub async fn get_transaction(
    State(state): State<ApiState>,
    Path(tx_hash): Path<String>,
) -> Result<Json<FinalizedTransactionResponse>, (StatusCode, String)> {
    let tx_id = parse_hash(&tx_hash).map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    let store = state.store.as_ref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "transaction receipt lookup requires a persistent store".to_string(),
        )
    })?;
    let store = Arc::clone(store);
    let lookup = tokio::task::spawn_blocking(move || store.load_transaction_receipt(&tx_id))
        .await
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("transaction receipt lookup task failed: {error}"),
            )
        })?
        .map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("transaction receipt lookup failed: {error}"),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "transaction has not been finalized".to_string(),
            )
        })?;

    Ok(Json(finalized_transaction_response(&lookup)))
}

fn parse_hash(value: &str) -> Result<Hash, String> {
    let encoded = value.strip_prefix("0x").unwrap_or(value);
    let bytes = hex::decode(encoded)
        .map_err(|_| "invalid transaction hash: expected 32-byte hexadecimal".to_string())?;
    bytes.try_into().map_err(|bytes: Vec<u8>| {
        format!(
            "invalid transaction hash: expected 32 bytes, got {}",
            bytes.len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::state::SharedState;
    use crate::app::{AppState, ConsensusTransaction, SignedEnvelope, Transaction};
    use crate::storage::{ConsensusState, PersistentStore, RocksDbStore};
    use crate::types::{
        Block, CommitmentV2, ConsensusContext, ResourceUsage, TransactionReceipt, TransactionType,
    };
    use std::sync::Arc;
    use tempfile::TempDir;

    fn state_without_store() -> ApiState {
        ApiState::new(SharedState::new(AppState::new()))
    }

    #[tokio::test]
    async fn invalid_hash_is_bad_request_before_store_lookup() {
        let result =
            get_transaction(State(state_without_store()), Path("not-a-hash".to_string())).await;
        assert_eq!(result.unwrap_err().0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_store_is_service_unavailable() {
        let result = get_transaction(State(state_without_store()), Path("00".repeat(32))).await;
        assert_eq!(result.unwrap_err().0, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn absent_receipt_is_not_found() {
        let directory = TempDir::new().expect("temp directory");
        let store = Arc::new(RocksDbStore::open(directory.path()).expect("open store"));
        let shared = SharedState::new(AppState::new());
        let state = ApiState::with_store(shared, store);

        let result = get_transaction(State(state), Path("00".repeat(32))).await;
        assert_eq!(result.unwrap_err().0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn finalized_receipt_survives_reopen_and_is_returned_by_hash() {
        let directory = TempDir::new().expect("temp directory");
        let context = ConsensusContext::with_genesis(0, [7; 32], [8; 32]);
        let genesis = Block::genesis(context);
        let genesis_state = ConsensusState {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            high_qc: None,
            locked_qc: None,
            voted_views: Vec::new(),
            current_view: 0,
            committed_height: 0,
            committed_hash: genesis.hash(),
            consecutive_timeouts: 0,
            vc_sent_for_view: None,
        };
        let signer = crate::crypto::Signer::generate();
        let envelope = SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: format!("{:?}", signer.address()),
                amount: 10,
            },
        )
        .expect("signed envelope");
        let tx_id = envelope.hash().expect("transaction ID");
        let payload =
            bincode::serialize(&vec![ConsensusTransaction::Signed(envelope)]).expect("payload");
        let receipt = TransactionReceipt::success(
            0,
            tx_id,
            TransactionType::DEPOSIT,
            ResourceUsage::default(),
            Vec::new(),
        )
        .expect("receipt");
        let commitment = CommitmentV2::new(vec![receipt]).expect("commitment");
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload,
            proposer: [1; 32],
            commitment_root: commitment.root().expect("commitment root"),
            app_hash: [9; 32],
            timestamp: 1,
            justify: None,
        };
        let committed_state = ConsensusState {
            committed_height: 1,
            committed_hash: block.hash(),
            current_view: 1,
            ..genesis_state.clone()
        };

        {
            let store = RocksDbStore::open(directory.path()).expect("open store");
            store
                .commit_block(&genesis, &genesis_state)
                .expect("commit genesis");
            store
                .commit_block_with_commitment_and_state_root(
                    &block,
                    &committed_state,
                    Some(&commitment),
                    Some(&block.app_hash),
                )
                .expect("commit finalized receipt");
        }

        let store = Arc::new(RocksDbStore::open(directory.path()).expect("reopen store"));
        let state = ApiState::with_store(SharedState::new(AppState::new()), store);
        let response = get_transaction(State(state), Path(hex::encode(tx_id)))
            .await
            .expect("finalized receipt response")
            .0;
        assert_eq!(response.status, "finalized");
        assert_eq!(response.tx_hash, hex::encode(tx_id));
        assert_eq!(response.block.height, 1);
        assert_eq!(
            response.receipt_status,
            crate::types::ReceiptStatus::SUCCESS.0
        );
    }

    #[test]
    fn hash_parser_accepts_optional_prefix_and_requires_exact_length() {
        let hash = parse_hash(&format!("0x{}", "ab".repeat(32))).expect("hash parses");
        assert_eq!(hash, [0xab; 32]);
        assert!(parse_hash("ab").is_err());
        assert!(parse_hash(&"ab".repeat(33)).is_err());
    }
}
