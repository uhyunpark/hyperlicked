//! Deterministic execution artifacts for indexers.
//!
//! The receipt/event primitives live in `crate::types::commitment`; this
//! module only binds those primitives to the existing `AppState` execution
//! queues. The combined root is authenticated by the block's dedicated
//! `commitment_root` field and remains distinct from the schema-v3 state hash.

use crate::app::staking::rewards::{RewardCompounding, RewardCredit};
use crate::app::{orderbook::Fill, ConsensusTransaction};
use crate::types::{
    hash, CommitmentError, ErrorCode, EventRecord, EventType, Hash, ReceiptStatus, ResourceUsage,
    TransactionReceipt, TransactionType,
};
use serde::{Deserialize, Serialize};

/// Canonical payload for a successful withdrawal event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WithdrawalInfo {
    pub trader: String,
    pub amount: i64,
}

/// Canonical payload for a successful native HYCK transfer event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyckTransferInfo {
    pub from: String,
    pub to: String,
    pub amount: i64,
}

/// Canonical payload for a successful oracle update event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleUpdateInfo {
    pub operator: String,
    pub symbol: String,
    pub sources: Vec<crate::app::PriceSource>,
}

/// Canonical payload for a successful market creation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketAddedInfo {
    pub admin: String,
    pub symbol: String,
    pub initial_mark_price: i64,
}

/// Canonical block-scoped record for one staking reward settlement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingRewardEpochInfo {
    pub schema_version: u16,
    pub accrual_timestamp: u64,
    pub epochs_processed: u64,
    pub total_distributed: i64,
    pub emissions_reserve_remaining: i64,
    pub auto_compounded: i64,
    pub validator_rewards: Vec<(crate::types::NodeId, i64)>,
    /// Canonical per-recipient credits.  This is distinct from ClaimRewards,
    /// which is a user transaction receipt that consumes pending balances.
    pub credits: Vec<RewardCredit>,
    /// Canonical per-recipient pending balances moved into bonded stake.
    pub compoundings: Vec<RewardCompounding>,
}

/// One executed payload entry plus the receipt schema and the exact bytes an
/// indexer needs to verify the transaction identity.
#[derive(Debug, Clone)]
pub struct ExecutionTransactionArtifact {
    pub receipt: TransactionReceipt,
    /// Exact canonical identity bytes: the envelope bytes for a signed entry,
    /// or the canonical bincode action bytes for an explicit system entry.
    /// The receipt tx_id is `hash(canonical_bytes)`.
    pub canonical_bytes: Vec<u8>,
    /// Exact canonical bincode encoding of this `ConsensusTransaction` item
    /// as embedded in the block payload.  This is retained separately because
    /// the enum wrapper is transport framing, not the user transaction ID.
    pub payload_entry_bytes: Vec<u8>,
    /// Exact canonical bincode encoding of the signed envelope, when present.
    pub envelope_bytes: Option<Vec<u8>>,
    /// Authenticated signer for signed entries, or the system action owner for
    /// explicit system entries.
    pub signer: String,
}

/// Compatibility alias for callers that used the initial collector name.
pub type TransactionArtifact = ExecutionTransactionArtifact;

/// All execution artifacts produced by one valid block execution.
#[derive(Debug, Clone)]
pub struct BlockExecutionArtifacts {
    pub height: u64,
    /// Observation metadata only.  It is intentionally excluded from
    /// `transaction_commitment()` so an app hash/block hash cannot create a
    /// circular commitment dependency during proposal execution.
    pub block_hash: Hash,
    pub timestamp: u64,
    pub transactions: Vec<ExecutionTransactionArtifact>,
    /// Events produced after the transaction loop.  They are intentionally
    /// kept separate from transaction receipts: liquidation, ADL, funding,
    /// and trigger processing are deterministic block/system phases, not the
    /// side effect of the last user transaction.
    pub block_events: Vec<EventRecord>,
}

impl BlockExecutionArtifacts {
    pub(crate) fn new(height: u64, block_hash: Hash, timestamp: u64) -> Self {
        Self {
            height,
            block_hash,
            timestamp,
            transactions: Vec::new(),
            block_events: Vec::new(),
        }
    }

    /// Return the transaction-only commitment view. Block-scoped events stay
    /// outside this view.
    pub fn transaction_commitment(&self) -> Result<crate::types::CommitmentV2, CommitmentError> {
        crate::types::CommitmentV2::new(
            self.transactions
                .iter()
                .map(|transaction| transaction.receipt.clone())
                .collect(),
        )
    }

    /// Return a Commitment v2 view that also commits deterministic
    /// post-transaction work.
    ///
    /// Block-scoped events are kept in Commitment v2's first-class
    /// `system_events` field and therefore do not consume a transaction index
    /// or receive a synthetic transaction ID.
    pub fn commitment_with_block_events(
        &self,
    ) -> Result<crate::types::CommitmentV2, CommitmentError> {
        let receipts: Vec<TransactionReceipt> = self
            .transactions
            .iter()
            .map(|transaction| transaction.receipt.clone())
            .collect();

        crate::types::CommitmentV2::new_with_system_events(receipts, self.block_events.clone())
    }
}

/// Cursor into pending event queues.  It lets the executor collect only
/// events emitted by the current transaction, including when a signed action
/// succeeds by replacing `self` with its transactional trial state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingEventCursor {
    pub(crate) order_updates: usize,
    pub(crate) deposits: usize,
    pub(crate) staking: usize,
    pub(crate) triggers: usize,
}

impl PendingEventCursor {
    pub(crate) fn capture(state: &super::AppState) -> Self {
        Self {
            order_updates: state.pending_order_updates.len(),
            deposits: state.pending_deposits.len(),
            staking: state.pending_staking_events.len(),
            triggers: state.pending_trigger_events.len(),
        }
    }
}

fn queue_tail<'a, T>(
    queue: &'a [T],
    cursor: usize,
    name: &'static str,
) -> Result<&'a [T], CommitmentError> {
    queue.get(cursor..).ok_or_else(|| {
        CommitmentError::Encoding(format!(
            "event queue {name} cursor {cursor} exceeds current length {}",
            queue.len()
        ))
    })
}

fn transaction_type(transaction: &ConsensusTransaction) -> TransactionType {
    use crate::app::Transaction;

    match transaction.action() {
        Transaction::PlaceOrder { .. } => TransactionType::PLACE_ORDER,
        Transaction::CancelOrder { .. } => TransactionType::CANCEL_ORDER,
        Transaction::Deposit { .. } => TransactionType::DEPOSIT,
        Transaction::Withdraw { .. } => TransactionType::WITHDRAW,
        Transaction::TransferHyck { .. } => TransactionType::TRANSFER_HYCK,
        Transaction::RegisterValidator { .. } => TransactionType::REGISTER_VALIDATOR,
        Transaction::RotateValidatorKey { .. } => TransactionType::ROTATE_VALIDATOR_KEY,
        Transaction::Delegate { .. } => TransactionType::DELEGATE,
        Transaction::Undelegate { .. } => TransactionType::UNDELEGATE,
        Transaction::ClaimUnstaked { .. } => TransactionType::CLAIM_UNSTAKED,
        Transaction::ClaimRewards { .. } => TransactionType::CLAIM_REWARDS,
        Transaction::Unjail { .. } => TransactionType::UNJAIL,
        Transaction::SubmitEvidence { .. } => TransactionType::SUBMIT_EVIDENCE,
        Transaction::PlaceTriggerOrder { .. } => TransactionType::PLACE_TRIGGER_ORDER,
        Transaction::CancelTriggerOrder { .. } => TransactionType::CANCEL_TRIGGER_ORDER,
        Transaction::CancelTriggerOrderByCloid { .. } => {
            TransactionType::CANCEL_TRIGGER_ORDER_BY_CLOID
        }
        Transaction::OraclePriceUpdate { .. } => TransactionType::ORACLE_PRICE_UPDATE,
        Transaction::AddMarket { .. } => TransactionType::ADD_MARKET,
    }
}

fn error_code(error: &super::AppError) -> ErrorCode {
    use super::AppError;

    match error {
        AppError::InvalidEnvelope(_) => ErrorCode::INVALID_ENVELOPE,
        AppError::Mempool(_) => ErrorCode::MEMPOOL,
        AppError::Account(_) => ErrorCode::ACCOUNT,
        AppError::OrderBook(_) => ErrorCode::ORDER_BOOK,
        AppError::Staking(_) => ErrorCode::STAKING,
        AppError::Trigger(_) => ErrorCode::TRIGGER,
        AppError::Oracle(_) => ErrorCode::ORACLE,
        AppError::MarketNotFound => ErrorCode::MARKET_NOT_FOUND,
        AppError::OrderNotFound => ErrorCode::ORDER_NOT_FOUND,
        AppError::InsufficientMargin => ErrorCode::INSUFFICIENT_MARGIN,
        AppError::ReduceOnlyViolation => ErrorCode::REDUCE_ONLY_VIOLATION,
        AppError::PositionTooLarge { .. } => ErrorCode::POSITION_TOO_LARGE,
        AppError::Unauthorized(_) => ErrorCode::UNAUTHORIZED,
    }
}

fn event<T: serde::Serialize>(
    event_index: u32,
    event_type: EventType,
    payload: &T,
) -> Result<EventRecord, CommitmentError> {
    EventRecord::from_bincode(event_index, event_type, payload)
}

impl super::AppState {
    /// Build the receipt artifact for one transaction from queue deltas and
    /// the fills returned by its action.
    pub(crate) fn transaction_artifact(
        &self,
        tx_index: u32,
        entry: &ConsensusTransaction,
        canonical_bytes: Vec<u8>,
        payload_entry_bytes: Vec<u8>,
        cursor: PendingEventCursor,
        fills: &[Fill],
        status: ReceiptStatus,
        failure: Option<&super::AppError>,
    ) -> Result<ExecutionTransactionArtifact, CommitmentError> {
        let signer = entry.trader_address();
        let envelope_bytes = match entry {
            ConsensusTransaction::Signed(envelope) => envelope.encoded_bytes().ok(),
            ConsensusTransaction::System(_) => None,
        };

        // The current action paths have a stable side-effect order: order
        // updates are queued before direct fills, while the other action
        // families emit one queue item.  Keep this explicit so event_index is
        // independent of HashMap iteration and future queue refactors.
        let mut events = Vec::new();
        for update in queue_tail(
            &self.pending_order_updates,
            cursor.order_updates,
            "order_updates",
        )? {
            events.push(event(events.len() as u32, EventType::ORDER_UPDATE, update)?);
        }
        for fill in fills {
            events.push(event(events.len() as u32, EventType::FILL, fill)?);
        }
        for deposit in queue_tail(&self.pending_deposits, cursor.deposits, "deposits")? {
            events.push(event(events.len() as u32, EventType::DEPOSIT, deposit)?);
        }
        for staking in queue_tail(&self.pending_staking_events, cursor.staking, "staking")? {
            events.push(event(events.len() as u32, EventType::STAKING, staking)?);
        }
        for trigger in queue_tail(&self.pending_trigger_events, cursor.triggers, "triggers")? {
            events.push(event(events.len() as u32, EventType::TRIGGER, trigger)?);
        }

        // These successful actions mutate state but do not currently enqueue
        // a WebSocket-style pending event. Add their explicit typed records so
        // a receipt is sufficient to reconstruct every action family without
        // inventing a synthetic transaction or relying on display strings.
        if status == ReceiptStatus::SUCCESS {
            use crate::app::Transaction;

            match entry.action() {
                Transaction::Withdraw { trader, amount } => {
                    events.push(event(
                        events.len() as u32,
                        EventType::WITHDRAW,
                        &WithdrawalInfo {
                            trader: trader.clone(),
                            amount: *amount,
                        },
                    )?);
                }
                Transaction::TransferHyck { from, to, amount } => {
                    events.push(event(
                        events.len() as u32,
                        EventType::TRANSFER_HYCK,
                        &HyckTransferInfo {
                            from: from.clone(),
                            to: to.clone(),
                            amount: *amount,
                        },
                    )?);
                }
                Transaction::OraclePriceUpdate {
                    operator,
                    symbol,
                    sources,
                    ..
                } => {
                    events.push(event(
                        events.len() as u32,
                        EventType::ORACLE,
                        &OracleUpdateInfo {
                            operator: operator.clone(),
                            symbol: symbol.clone(),
                            sources: sources.clone(),
                        },
                    )?);
                }
                Transaction::AddMarket {
                    admin,
                    config,
                    initial_mark_price,
                } => {
                    events.push(event(
                        events.len() as u32,
                        EventType::MARKET,
                        &MarketAddedInfo {
                            admin: admin.clone(),
                            symbol: config.symbol.clone(),
                            initial_mark_price: *initial_mark_price,
                        },
                    )?);
                }
                _ => {}
            }
        }

        let tx_id = entry
            .hash()
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        if tx_id != hash(&canonical_bytes) {
            return Err(CommitmentError::TransactionIdMismatch);
        }
        let tx_type = transaction_type(entry);
        let receipt = if status == ReceiptStatus::SUCCESS {
            TransactionReceipt::success(tx_index, tx_id, tx_type, ResourceUsage::default(), events)?
        } else {
            TransactionReceipt::failure(
                tx_index,
                tx_id,
                tx_type,
                failure.map(error_code).unwrap_or(ErrorCode::UNKNOWN),
                ResourceUsage::default(),
                events,
            )?
        };

        Ok(ExecutionTransactionArtifact {
            receipt,
            canonical_bytes,
            payload_entry_bytes,
            envelope_bytes,
            signer,
        })
    }

    /// Convert all post-transaction pending queues into block-scoped events.
    /// The order is deterministic execution phase order:
    /// liquidation, ADL, funding, then trigger-generated order/fill/trigger
    /// events.  The direct transaction prefixes are excluded by the supplied
    /// offsets/counts.
    pub(crate) fn block_execution_events(
        &self,
        reward_epoch: Option<&StakingRewardEpochInfo>,
        order_updates_start: usize,
        fills_start: usize,
        triggers_start: usize,
    ) -> Result<Vec<EventRecord>, CommitmentError> {
        let mut events = Vec::new();
        if let Some(reward_epoch) = reward_epoch {
            events.push(event(events.len() as u32, EventType::EPOCH, reward_epoch)?);
        }
        for liquidation in &self.pending_liquidations {
            events.push(event(
                events.len() as u32,
                EventType::LIQUIDATION,
                liquidation,
            )?);
        }
        for adl in &self.pending_adl_events {
            events.push(event(events.len() as u32, EventType::ADL, adl)?);
        }
        for funding in &self.pending_funding {
            events.push(event(events.len() as u32, EventType::FUNDING, funding)?);
        }
        for update in queue_tail(
            &self.pending_order_updates,
            order_updates_start,
            "order_updates",
        )? {
            events.push(event(events.len() as u32, EventType::ORDER_UPDATE, update)?);
        }
        for fill in queue_tail(&self.pending_fills, fills_start, "fills")? {
            events.push(event(events.len() as u32, EventType::FILL, fill)?);
        }
        for trigger in queue_tail(&self.pending_trigger_events, triggers_start, "triggers")? {
            events.push(event(events.len() as u32, EventType::TRIGGER, trigger)?);
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{OrderType, Side, Transaction};
    use crate::consensus::AppHook;
    use crate::types::{Block, ConsensusContext};

    fn context() -> ConsensusContext {
        ConsensusContext::new(0, [3u8; 32])
    }

    fn block(height: u64, parent: Hash, txs: Vec<Transaction>) -> Block {
        let context = context();
        Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: height,
            height,
            parent,
            payload: bincode::serialize(
                &txs.into_iter()
                    .map(ConsensusTransaction::System)
                    .collect::<Vec<_>>(),
            )
            .expect("payload encodes"),
            proposer: [0u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: height,
            justify: None,
        }
    }

    #[test]
    fn transaction_and_event_indices_follow_payload_order() {
        let genesis = Block::genesis(context());
        let mut state =
            super::super::AppState::new_with_chain_domain_and_dev(context().genesis_hash, true);
        let txs = vec![
            Transaction::Deposit {
                trader: "alice".into(),
                amount: 100_000_000,
            },
            Transaction::Deposit {
                trader: "bob".into(),
                amount: 100_000_000,
            },
            Transaction::PlaceOrder {
                trader: "alice".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Bid,
                price: 5_000_000,
                size: 100_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            },
            Transaction::PlaceOrder {
                trader: "bob".into(),
                symbol: "BTC-USDT".into(),
                side: Side::Ask,
                price: 5_000_000,
                size: 100_000,
                order_type: OrderType::Gtc,
                reduce_only: false,
            },
        ];
        let block = block(1, genesis.hash(), txs);
        <super::super::AppState as AppHook>::execute(&mut state, &block);

        let artifacts = state
            .take_execution_artifacts()
            .expect("execution artifacts");
        assert_eq!(
            artifacts
                .transactions
                .iter()
                .map(|transaction| transaction.receipt.tx_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert!(artifacts.transactions.iter().all(|transaction| transaction
            .receipt
            .events
            .iter()
            .enumerate()
            .all(|(index, event)| event.event_index == index as u32)));

        let matching = &artifacts.transactions[3].receipt;
        assert_eq!(matching.events.len(), 2);
        assert_eq!(matching.events[0].event_type, EventType::ORDER_UPDATE);
        assert_eq!(matching.events[1].event_type, EventType::FILL);
        assert_eq!(
            artifacts.transactions[3].receipt.tx_id,
            hash(&artifacts.transactions[3].canonical_bytes)
        );
        let first_entry = ConsensusTransaction::System(Transaction::Deposit {
            trader: "alice".into(),
            amount: 100_000_000,
        });
        assert_eq!(
            artifacts.transactions[0].receipt.tx_id,
            first_entry.hash().expect("system identity hashes")
        );
    }

    #[test]
    fn failed_action_keeps_a_failure_receipt_without_action_events() {
        let genesis = Block::genesis(context());
        let mut state =
            super::super::AppState::new_with_chain_domain_and_dev(context().genesis_hash, true);
        let block = block(
            1,
            genesis.hash(),
            vec![Transaction::Withdraw {
                trader: "alice".into(),
                amount: 1,
            }],
        );
        <super::super::AppState as AppHook>::execute(&mut state, &block);

        let artifacts = state
            .take_execution_artifacts()
            .expect("execution artifacts");
        let receipt = &artifacts.transactions[0].receipt;
        assert_eq!(receipt.status, ReceiptStatus::FAILURE);
        assert_eq!(receipt.error_code, ErrorCode::ACCOUNT);
        assert!(receipt.events.is_empty());
    }

    #[test]
    fn signed_transaction_id_uses_the_exact_envelope_bytes() {
        let context = context();
        let genesis = Block::genesis(context);
        let signer = crate::crypto::Signer::generate();
        let envelope = crate::app::SignedEnvelope::sign(
            context.genesis_hash,
            &signer,
            0,
            0,
            100,
            Transaction::Deposit {
                trader: format!("{:?}", signer.address()),
                amount: 42,
            },
        )
        .expect("envelope signs");
        let envelope_bytes = envelope.encoded_bytes().expect("envelope encodes");
        let payload = bincode::serialize(&vec![ConsensusTransaction::Signed(envelope.clone())])
            .expect("payload encodes");
        let block = Block {
            epoch: context.epoch,
            committee_hash: context.committee_hash,
            genesis_hash: context.genesis_hash,
            view: 1,
            height: 1,
            parent: genesis.hash(),
            payload,
            proposer: [0u8; 32],
            commitment_root: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 1,
            justify: None,
        };
        let mut state =
            super::super::AppState::new_with_chain_domain_and_dev(context.genesis_hash, false);
        <super::super::AppState as AppHook>::execute(&mut state, &block);

        let artifacts = state
            .take_execution_artifacts()
            .expect("execution artifacts");
        let transaction = &artifacts.transactions[0];
        assert_eq!(transaction.canonical_bytes, envelope_bytes);
        assert_eq!(transaction.envelope_bytes, Some(envelope_bytes.clone()));
        assert_eq!(transaction.receipt.tx_id, hash(&envelope_bytes));
    }

    #[test]
    fn artifacts_are_replaced_per_block_and_do_not_leak() {
        let genesis = Block::genesis(context());
        let mut state =
            super::super::AppState::new_with_chain_domain_and_dev(context().genesis_hash, true);
        let first = block(
            1,
            genesis.hash(),
            vec![Transaction::Deposit {
                trader: "alice".into(),
                amount: 42,
            }],
        );
        <super::super::AppState as AppHook>::execute(&mut state, &first);

        let second = block(2, first.hash(), Vec::new());
        <super::super::AppState as AppHook>::execute(&mut state, &second);

        let artifacts = state
            .take_execution_artifacts()
            .expect("second block artifacts");
        assert_eq!(artifacts.height, 2);
        assert_eq!(artifacts.block_hash, second.hash());
        assert!(artifacts.transactions.is_empty());
        assert!(artifacts.block_events.is_empty());
        assert!(state.take_execution_artifacts().is_none());
        assert!(state.take_pending_deposits().is_empty());
    }

    #[test]
    fn block_events_stay_separate_without_block_hash_dependency() {
        let event =
            EventRecord::new(0, EventType::FUNDING, b"funding".to_vec()).expect("event encodes");
        let mut first = BlockExecutionArtifacts::new(7, [1u8; 32], 700);
        first.block_events.push(event.clone());
        let mut second = first.clone();
        second.block_hash = [2u8; 32];

        let first_commitment = first
            .commitment_with_block_events()
            .expect("commitment encodes");
        let second_commitment = second
            .commitment_with_block_events()
            .expect("commitment encodes");
        assert_eq!(first_commitment, second_commitment);
        assert_eq!(first_commitment.receipts.len(), 0);
        assert_eq!(first_commitment.system_events.len(), 1);
        assert_eq!(first_commitment.system_events[0].event_index, 0);
    }

    #[test]
    fn successful_actions_without_pending_queues_emit_typed_events() {
        let state =
            super::super::AppState::new_with_chain_domain_and_dev(context().genesis_hash, true);
        let entries = vec![
            ConsensusTransaction::System(Transaction::Withdraw {
                trader: "alice".into(),
                amount: 7,
            }),
            ConsensusTransaction::System(Transaction::OraclePriceUpdate {
                operator: "oracle".into(),
                symbol: "BTC-USDT".into(),
                sources: vec![],
                signature: vec![],
            }),
            ConsensusTransaction::System(Transaction::AddMarket {
                admin: "admin".into(),
                config: crate::app::MarketConfig::default(),
                initial_mark_price: 5_000_000,
            }),
        ];

        let event_types: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let canonical = entry.canonical_bytes().expect("canonical identity");
                state
                    .transaction_artifact(
                        index as u32,
                        entry,
                        canonical,
                        bincode::serialize(entry).expect("payload entry"),
                        PendingEventCursor::capture(&state),
                        &[],
                        ReceiptStatus::SUCCESS,
                        None,
                    )
                    .expect("artifact")
                    .receipt
                    .events
                    .into_iter()
                    .map(|event| event.event_type)
                    .collect::<Vec<_>>()
            })
            .flatten()
            .collect();

        assert_eq!(
            event_types,
            vec![EventType::WITHDRAW, EventType::ORACLE, EventType::MARKET]
        );
    }

    #[test]
    fn stale_event_cursor_is_rejected_without_panicking() {
        let state =
            super::super::AppState::new_with_chain_domain_and_dev(context().genesis_hash, true);
        let entry = ConsensusTransaction::System(Transaction::Deposit {
            trader: "alice".into(),
            amount: 7,
        });
        let canonical = entry.canonical_bytes().expect("canonical identity");

        assert!(matches!(
            state.transaction_artifact(
                0,
                &entry,
                canonical,
                bincode::serialize(&entry).expect("payload entry"),
                PendingEventCursor {
                    order_updates: 1,
                    deposits: 0,
                    staking: 0,
                    triggers: 0,
                },
                &[],
                ReceiptStatus::SUCCESS,
                None,
            ),
            Err(CommitmentError::Encoding(_))
        ));
    }
}
