//! Commitment v2 application artifacts.
//!
//! This module defines the block-local receipt/event format that an indexer
//! can consume without depending on execution internals.  It is deliberately
//! authenticated by the block's separate `commitment_root` field.  The wire
//! encoding is bincode only:
//! event payloads are opaque, canonical bytes supplied by the executor and
//! are never converted through JSON here.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Hash;

/// Schema version for the artifact fields.
pub const COMMITMENT_SCHEMA_VERSION: u16 = 1;
/// Consensus-authenticated commitment family/version.
pub const COMMITMENT_VERSION: u16 = 2;

/// Domain tags are distinct for leaves, each root, and the combined root.
/// The trailing NUL is part of each tag and prevents accidental concatenation
/// with an adjacent field when these tags are reused elsewhere.
pub const RECEIPT_LEAF_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_RECEIPT_LEAF\0";
pub const EVENT_LEAF_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_EVENT_LEAF\0";
pub const SYSTEM_EVENT_LEAF_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_SYSTEM_EVENT_LEAF\0";
pub const RECEIPT_ROOT_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_RECEIPT_ROOT\0";
pub const EVENT_ROOT_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_EVENT_ROOT\0";
pub const COMMITMENT_ROOT_DOMAIN: &[u8] = b"HYPERLICKED_COMMITMENT_V2_ROOT\0";

/// Maximum number of transactions represented by one commitment artifact.
pub const MAX_RECEIPTS_PER_COMMITMENT: usize = 10_000;
/// Maximum number of typed events attached to one transaction.
pub const MAX_EVENTS_PER_RECEIPT: usize = 4_096;
/// Maximum number of deterministic block/system events.
pub const MAX_SYSTEM_EVENTS_PER_COMMITMENT: usize = 4_096;
/// Maximum size of one canonical event payload.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 64 * 1024;
/// Maximum sum of event payload bytes in one commitment artifact.
pub const MAX_EVENT_PAYLOAD_BYTES_PER_COMMITMENT: usize = 4 * 1024 * 1024;
/// Maximum canonical size of one receipt.
pub const MAX_RECEIPT_BYTES: usize = 256 * 1024;
/// Maximum canonical size of one commitment artifact.
pub const MAX_COMMITMENT_BYTES: usize = 8 * 1024 * 1024;

/// Small, forward-compatible numeric transaction type identifier.
///
/// A numeric newtype is used instead of a Rust enum so an indexer can retain
/// an unknown type introduced by a later schema version.  The values below
/// are stable protocol identifiers, not Rust enum ordinals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransactionType(pub u16);

impl TransactionType {
    pub const UNKNOWN: Self = Self(0);
    pub const PLACE_ORDER: Self = Self(1);
    pub const CANCEL_ORDER: Self = Self(2);
    pub const DEPOSIT: Self = Self(3);
    pub const WITHDRAW: Self = Self(4);
    pub const REGISTER_VALIDATOR: Self = Self(5);
    pub const ROTATE_VALIDATOR_KEY: Self = Self(6);
    pub const DELEGATE: Self = Self(7);
    pub const UNDELEGATE: Self = Self(8);
    pub const CLAIM_UNSTAKED: Self = Self(9);
    pub const CLAIM_REWARDS: Self = Self(10);
    pub const UNJAIL: Self = Self(11);
    pub const SUBMIT_EVIDENCE: Self = Self(12);
    pub const PLACE_TRIGGER_ORDER: Self = Self(13);
    pub const CANCEL_TRIGGER_ORDER: Self = Self(14);
    pub const CANCEL_TRIGGER_ORDER_BY_CLOID: Self = Self(15);
    pub const ORACLE_PRICE_UPDATE: Self = Self(16);
    pub const ADD_MARKET: Self = Self(17);
    pub const TRANSFER_HYCK: Self = Self(18);
}

/// Small, forward-compatible numeric event type identifier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventType(pub u16);

impl EventType {
    pub const UNKNOWN: Self = Self(0);
    pub const ORDER_UPDATE: Self = Self(1);
    pub const FILL: Self = Self(2);
    /// Alias useful to indexers that call a fill a trade.
    pub const TRADE: Self = Self::FILL;
    pub const DEPOSIT: Self = Self(3);
    pub const WITHDRAW: Self = Self(4);
    pub const LIQUIDATION: Self = Self(5);
    pub const FUNDING: Self = Self(6);
    pub const STAKING: Self = Self(7);
    pub const TRIGGER: Self = Self(8);
    pub const ADL: Self = Self(9);
    pub const EPOCH: Self = Self(10);
    pub const ORACLE: Self = Self(11);
    pub const MARKET: Self = Self(12);
    pub const TRANSFER_HYCK: Self = Self(13);
}

/// Stable numeric execution error code.
///
/// Error display strings are intentionally absent.  Executors map their
/// local error values to one of these codes before constructing a receipt;
/// only this numeric code is committed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ErrorCode(pub u16);

impl ErrorCode {
    pub const NONE: Self = Self(0);
    pub const UNKNOWN: Self = Self(1);
    pub const INVALID_ENVELOPE: Self = Self(2);
    pub const MEMPOOL: Self = Self(3);
    pub const ACCOUNT: Self = Self(4);
    pub const ORDER_BOOK: Self = Self(5);
    pub const STAKING: Self = Self(6);
    pub const TRIGGER: Self = Self(7);
    pub const ORACLE: Self = Self(8);
    pub const MARKET_NOT_FOUND: Self = Self(9);
    pub const ORDER_NOT_FOUND: Self = Self(10);
    pub const INSUFFICIENT_MARGIN: Self = Self(11);
    pub const REDUCE_ONLY_VIOLATION: Self = Self(12);
    pub const POSITION_TOO_LARGE: Self = Self(13);
    pub const UNAUTHORIZED: Self = Self(14);

    pub const fn is_none(self) -> bool {
        self.0 == Self::NONE.0
    }
}

/// Success/failure marker for a transaction execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReceiptStatus(pub u8);

impl ReceiptStatus {
    pub const SUCCESS: Self = Self(0);
    pub const FAILURE: Self = Self(1);

    pub const fn is_success(self) -> bool {
        self.0 == Self::SUCCESS.0
    }

    pub const fn is_failure(self) -> bool {
        self.0 == Self::FAILURE.0
    }
}

/// Deterministic execution/resource counters.
///
/// Zero is the safe default until the executor has real metering.  Counters
/// are fixed-width integers, so they do not depend on platform word size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub compute_units: u64,
    pub storage_read_bytes: u64,
    pub storage_write_bytes: u64,
}

/// One typed event in a transaction receipt.
///
/// `payload` must be the exact canonical bincode bytes of the event payload
/// schema selected by `event_type`.  The bytes are opaque to this module and
/// are hashed exactly as supplied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    pub event_index: u32,
    pub event_type: EventType,
    pub payload: Vec<u8>,
}

impl EventRecord {
    /// Construct an event from already-canonical payload bytes.
    pub fn new(
        event_index: u32,
        event_type: EventType,
        payload: Vec<u8>,
    ) -> Result<Self, CommitmentError> {
        validate_payload_size(payload.len())?;
        Ok(Self {
            event_index,
            event_type,
            payload,
        })
    }

    /// Encode a typed payload using the protocol's bincode encoding and wrap
    /// it as an event.  Callers must provide canonical values (for example,
    /// sort map-like data before serializing it).
    pub fn from_bincode<T: Serialize>(
        event_index: u32,
        event_type: EventType,
        payload: &T,
    ) -> Result<Self, CommitmentError> {
        let bytes = bincode::serialize(payload)
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        Self::new(event_index, event_type, bytes)
    }

    /// Return the exact bincode bytes committed for this event record.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CommitmentError> {
        validate_payload_size(self.payload.len())?;
        self.canonical_bytes_validated()
    }

    fn canonical_bytes_validated(&self) -> Result<Vec<u8>, CommitmentError> {
        bincode::serialize(self).map_err(|error| CommitmentError::Encoding(error.to_string()))
    }

    /// Hash this event in its transaction's position in the block.
    pub fn hash(&self, tx_index: u32) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes()?;
        Ok(hash_domain(
            EVENT_LEAF_DOMAIN,
            &tx_index.to_le_bytes(),
            &bytes,
        ))
    }

    /// Hash this event in the block/system scope.  System events have no
    /// transaction ID or transaction index; their separate commitment field
    /// and domain keep them distinct from transaction-scoped events.
    pub fn system_hash(&self) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes()?;
        Ok(hash_domain(
            SYSTEM_EVENT_LEAF_DOMAIN,
            &self.event_index.to_le_bytes(),
            &bytes,
        ))
    }

    fn hash_validated(&self, tx_index: u32) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes_validated()?;
        Ok(hash_domain(
            EVENT_LEAF_DOMAIN,
            &tx_index.to_le_bytes(),
            &bytes,
        ))
    }

    fn system_hash_validated(&self) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes_validated()?;
        Ok(hash_domain(
            SYSTEM_EVENT_LEAF_DOMAIN,
            &self.event_index.to_le_bytes(),
            &bytes,
        ))
    }
}

/// Receipt for one transaction at a specific block transaction index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionReceipt {
    pub tx_index: u32,
    pub tx_id: Hash,
    pub tx_type: TransactionType,
    pub status: ReceiptStatus,
    pub error_code: ErrorCode,
    pub resource_usage: ResourceUsage,
    /// Events are committed in this vector order and must carry contiguous
    /// `event_index` values starting at zero.
    pub events: Vec<EventRecord>,
}

impl TransactionReceipt {
    /// Construct a successful receipt.  Successful receipts cannot carry an
    /// error code in the canonical representation.
    pub fn success(
        tx_index: u32,
        tx_id: Hash,
        tx_type: TransactionType,
        resource_usage: ResourceUsage,
        events: Vec<EventRecord>,
    ) -> Result<Self, CommitmentError> {
        Self::new(
            tx_index,
            tx_id,
            tx_type,
            ReceiptStatus::SUCCESS,
            ErrorCode::NONE,
            resource_usage,
            events,
        )
    }

    /// Construct a failed receipt.  The unstable display string of an
    /// execution error is not accepted by this schema; callers provide only
    /// the stable numeric code.
    pub fn failure(
        tx_index: u32,
        tx_id: Hash,
        tx_type: TransactionType,
        error_code: ErrorCode,
        resource_usage: ResourceUsage,
        events: Vec<EventRecord>,
    ) -> Result<Self, CommitmentError> {
        Self::new(
            tx_index,
            tx_id,
            tx_type,
            ReceiptStatus::FAILURE,
            error_code,
            resource_usage,
            events,
        )
    }

    /// Construct a receipt with an explicit status/code pair.
    pub fn new(
        tx_index: u32,
        tx_id: Hash,
        tx_type: TransactionType,
        status: ReceiptStatus,
        error_code: ErrorCode,
        resource_usage: ResourceUsage,
        events: Vec<EventRecord>,
    ) -> Result<Self, CommitmentError> {
        let receipt = Self {
            tx_index,
            tx_id,
            tx_type,
            status,
            error_code,
            resource_usage,
            events,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    /// Validate local receipt invariants independent of block position.
    pub fn validate(&self) -> Result<(), CommitmentError> {
        if self.status == ReceiptStatus::SUCCESS {
            if !self.error_code.is_none() {
                return Err(CommitmentError::SuccessHasErrorCode {
                    code: self.error_code.0,
                });
            }
        } else if self.status == ReceiptStatus::FAILURE {
            if self.error_code.is_none() {
                return Err(CommitmentError::FailureMissingErrorCode);
            }
        } else {
            return Err(CommitmentError::InvalidStatus(self.status.0));
        }

        if self.events.len() > MAX_EVENTS_PER_RECEIPT {
            return Err(CommitmentError::TooManyEvents {
                count: self.events.len(),
                max: MAX_EVENTS_PER_RECEIPT,
            });
        }
        for (expected, event) in self.events.iter().enumerate() {
            let expected = expected as u32;
            if event.event_index != expected {
                return Err(CommitmentError::EventIndex {
                    expected,
                    actual: event.event_index,
                });
            }
            validate_payload_size(event.payload.len())?;
        }
        Ok(())
    }

    /// Return the exact bincode bytes committed for this receipt.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CommitmentError> {
        self.validate()?;
        self.canonical_bytes_validated()
    }

    fn canonical_bytes_validated(&self) -> Result<Vec<u8>, CommitmentError> {
        let bytes = bincode::serialize(self)
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        if bytes.len() > MAX_RECEIPT_BYTES {
            return Err(CommitmentError::ReceiptTooLarge {
                size: bytes.len(),
                max: MAX_RECEIPT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Hash this receipt as an ordered transaction leaf.
    pub fn hash(&self) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes()?;
        Ok(hash_domain(RECEIPT_LEAF_DOMAIN, &[], &bytes))
    }

    fn hash_validated(&self) -> Result<Hash, CommitmentError> {
        let bytes = self.canonical_bytes_validated()?;
        Ok(hash_domain(RECEIPT_LEAF_DOMAIN, &[], &bytes))
    }
}

/// Block-local Commitment v2 artifact.
///
/// Transaction indices are required to be `0..receipts.len()` and event
/// indices are required to be `0..events.len()` within each receipt.  This
/// makes the indexer's natural key `(block_height, tx_index, event_index)`
/// unambiguous once the enclosing block supplies its height.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitmentV2 {
    pub schema_version: u16,
    pub commitment_version: u16,
    pub receipts: Vec<TransactionReceipt>,
    /// Deterministic block-scoped events emitted by protocol phases after the
    /// transaction loop (for example funding, liquidation, ADL, or trigger
    /// processing).  This is deliberately separate from receipts: a system
    /// event has no `tx_id`, so it cannot collide with a user transaction.
    pub system_events: Vec<EventRecord>,
}

impl CommitmentV2 {
    /// Build a current-version artifact and validate all ordering/size rules.
    pub fn new(receipts: Vec<TransactionReceipt>) -> Result<Self, CommitmentError> {
        Self::new_with_system_events(receipts, Vec::new())
    }

    /// Build a current-version artifact including block-scoped system events.
    pub fn new_with_system_events(
        receipts: Vec<TransactionReceipt>,
        system_events: Vec<EventRecord>,
    ) -> Result<Self, CommitmentError> {
        let commitment = Self {
            schema_version: COMMITMENT_SCHEMA_VERSION,
            commitment_version: COMMITMENT_VERSION,
            receipts,
            system_events,
        };
        commitment.validate()?;
        Ok(commitment)
    }

    /// Validate versions, transaction ordering, event ordering, and bounds.
    pub fn validate(&self) -> Result<(), CommitmentError> {
        if self.schema_version != COMMITMENT_SCHEMA_VERSION {
            return Err(CommitmentError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.commitment_version != COMMITMENT_VERSION {
            return Err(CommitmentError::UnsupportedCommitmentVersion(
                self.commitment_version,
            ));
        }
        if self.receipts.len() > MAX_RECEIPTS_PER_COMMITMENT {
            return Err(CommitmentError::TooManyReceipts {
                count: self.receipts.len(),
                max: MAX_RECEIPTS_PER_COMMITMENT,
            });
        }

        if self.system_events.len() > MAX_SYSTEM_EVENTS_PER_COMMITMENT {
            return Err(CommitmentError::TooManySystemEvents {
                count: self.system_events.len(),
                max: MAX_SYSTEM_EVENTS_PER_COMMITMENT,
            });
        }

        let mut event_payload_bytes = 0usize;
        for (expected, receipt) in self.receipts.iter().enumerate() {
            let expected = expected as u32;
            if receipt.tx_index != expected {
                return Err(CommitmentError::TransactionIndex {
                    expected,
                    actual: receipt.tx_index,
                });
            }
            receipt.validate()?;
            // Check the serialized receipt here, rather than deferring this
            // bound to `canonical_bytes()`.  Constructors are used during
            // execution preflight, so a commitment that has passed
            // validation must not become unencodable later.
            let receipt_bytes = bincode::serialize(receipt)
                .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
            if receipt_bytes.len() > MAX_RECEIPT_BYTES {
                return Err(CommitmentError::ReceiptTooLarge {
                    size: receipt_bytes.len(),
                    max: MAX_RECEIPT_BYTES,
                });
            }
            for event in &receipt.events {
                event_payload_bytes = event_payload_bytes.checked_add(event.payload.len()).ok_or(
                    CommitmentError::EventPayloadBytesTooLarge {
                        size: usize::MAX,
                        max: MAX_EVENT_PAYLOAD_BYTES_PER_COMMITMENT,
                    },
                )?;
            }
        }
        for (expected, event) in self.system_events.iter().enumerate() {
            let expected = expected as u32;
            if event.event_index != expected {
                return Err(CommitmentError::SystemEventIndex {
                    expected,
                    actual: event.event_index,
                });
            }
            validate_payload_size(event.payload.len())?;
            event_payload_bytes = event_payload_bytes.checked_add(event.payload.len()).ok_or(
                CommitmentError::EventPayloadBytesTooLarge {
                    size: usize::MAX,
                    max: MAX_EVENT_PAYLOAD_BYTES_PER_COMMITMENT,
                },
            )?;
        }
        if event_payload_bytes > MAX_EVENT_PAYLOAD_BYTES_PER_COMMITMENT {
            return Err(CommitmentError::EventPayloadBytesTooLarge {
                size: event_payload_bytes,
                max: MAX_EVENT_PAYLOAD_BYTES_PER_COMMITMENT,
            });
        }

        // As above, perform the final size check during structural
        // validation. This makes `new_with_system_events()` a complete
        // preflight instead of allowing an oversized artifact to survive
        // until a later persistence/transport call.
        let commitment_bytes = bincode::serialize(self)
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        if commitment_bytes.len() > MAX_COMMITMENT_BYTES {
            return Err(CommitmentError::CommitmentTooLarge {
                size: commitment_bytes.len(),
                max: MAX_COMMITMENT_BYTES,
            });
        }
        Ok(())
    }

    /// Return canonical bincode bytes for persistence or transport.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CommitmentError> {
        self.validate()?;
        let bytes = bincode::serialize(self)
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        if bytes.len() > MAX_COMMITMENT_BYTES {
            return Err(CommitmentError::CommitmentTooLarge {
                size: bytes.len(),
                max: MAX_COMMITMENT_BYTES,
            });
        }
        Ok(bytes)
    }

    /// Decode only canonical bincode (no trailing bytes or alternate form).
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, CommitmentError> {
        if bytes.len() > MAX_COMMITMENT_BYTES {
            return Err(CommitmentError::CommitmentTooLarge {
                size: bytes.len(),
                max: MAX_COMMITMENT_BYTES,
            });
        }
        let commitment: Self = bincode::deserialize(bytes)
            .map_err(|error| CommitmentError::Encoding(error.to_string()))?;
        let canonical = commitment.canonical_bytes()?;
        if canonical != bytes {
            return Err(CommitmentError::NonCanonicalEncoding);
        }
        Ok(commitment)
    }

    /// Root over ordered receipt leaves.
    pub fn receipts_root(&self) -> Result<Hash, CommitmentError> {
        self.validate()?;
        self.receipts_root_validated()
    }

    fn receipts_root_validated(&self) -> Result<Hash, CommitmentError> {
        let mut leaves = Vec::with_capacity(self.receipts.len());
        for receipt in &self.receipts {
            leaves.push(receipt.hash_validated()?);
        }
        Ok(root_hash(
            RECEIPT_ROOT_DOMAIN,
            self.schema_version,
            self.commitment_version,
            &leaves,
        ))
    }

    /// Root over ordered `(tx_index, event_index)` event leaves.
    pub fn events_root(&self) -> Result<Hash, CommitmentError> {
        self.validate()?;
        self.events_root_validated()
    }

    fn events_root_validated(&self) -> Result<Hash, CommitmentError> {
        let event_count = self
            .receipts
            .iter()
            .map(|receipt| receipt.events.len())
            .sum::<usize>();
        let mut leaves = Vec::with_capacity(event_count);
        for receipt in &self.receipts {
            for event in &receipt.events {
                leaves.push(event.hash_validated(receipt.tx_index)?);
            }
        }
        for event in &self.system_events {
            leaves.push(event.system_hash_validated()?);
        }
        Ok(root_hash(
            EVENT_ROOT_DOMAIN,
            self.schema_version,
            self.commitment_version,
            &leaves,
        ))
    }

    /// Combined root for the complete receipt/event artifact.
    pub fn root(&self) -> Result<Hash, CommitmentError> {
        self.validate()?;
        let receipts_root = self.receipts_root_validated()?;
        let events_root = self.events_root_validated()?;
        let mut body = Vec::with_capacity(16 + 32 + 32);
        body.extend_from_slice(&(self.receipts.len() as u64).to_le_bytes());
        body.extend_from_slice(&(self.system_events.len() as u64).to_le_bytes());
        body.extend_from_slice(&receipts_root);
        body.extend_from_slice(&events_root);
        Ok(hash_domain(COMMITMENT_ROOT_DOMAIN, &[], &body))
    }
}

impl Default for CommitmentV2 {
    fn default() -> Self {
        Self {
            schema_version: COMMITMENT_SCHEMA_VERSION,
            commitment_version: COMMITMENT_VERSION,
            receipts: Vec::new(),
            system_events: Vec::new(),
        }
    }
}

/// Compute a receipt root without first naming a commitment artifact.
pub fn receipts_root(receipts: &[TransactionReceipt]) -> Result<Hash, CommitmentError> {
    CommitmentV2::new(receipts.to_vec())?.receipts_root()
}

/// Compute an event root without first naming a commitment artifact.
pub fn events_root(receipts: &[TransactionReceipt]) -> Result<Hash, CommitmentError> {
    CommitmentV2::new(receipts.to_vec())?.events_root()
}

/// Compute an event root including block-scoped system events.
pub fn events_root_with_system_events(
    receipts: &[TransactionReceipt],
    system_events: &[EventRecord],
) -> Result<Hash, CommitmentError> {
    CommitmentV2::new_with_system_events(receipts.to_vec(), system_events.to_vec())?.events_root()
}

/// Compute the combined Commitment v2 root without first naming an artifact.
pub fn commitment_root(receipts: &[TransactionReceipt]) -> Result<Hash, CommitmentError> {
    CommitmentV2::new(receipts.to_vec())?.root()
}

/// Compute a combined root including block-scoped system events.
pub fn commitment_root_with_system_events(
    receipts: &[TransactionReceipt],
    system_events: &[EventRecord],
) -> Result<Hash, CommitmentError> {
    CommitmentV2::new_with_system_events(receipts.to_vec(), system_events.to_vec())?.root()
}

fn validate_payload_size(size: usize) -> Result<(), CommitmentError> {
    if size > MAX_EVENT_PAYLOAD_BYTES {
        return Err(CommitmentError::EventPayloadTooLarge {
            size,
            max: MAX_EVENT_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

fn hash_domain(domain: &[u8], context: &[u8], body: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(COMMITMENT_SCHEMA_VERSION.to_le_bytes());
    hasher.update(COMMITMENT_VERSION.to_le_bytes());
    hasher.update((context.len() as u64).to_le_bytes());
    hasher.update(context);
    hasher.update((body.len() as u64).to_le_bytes());
    hasher.update(body);
    hasher.finalize().into()
}

fn root_hash(domain: &[u8], schema_version: u16, commitment_version: u16, leaves: &[Hash]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(schema_version.to_le_bytes());
    hasher.update(commitment_version.to_le_bytes());
    hasher.update((leaves.len() as u64).to_le_bytes());
    for leaf in leaves {
        hasher.update(leaf);
    }
    hasher.finalize().into()
}

/// Errors returned while constructing or validating an artifact.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitmentError {
    #[error("unsupported commitment schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("unsupported commitment version {0}")]
    UnsupportedCommitmentVersion(u16),
    #[error("too many receipts: {count} > {max}")]
    TooManyReceipts { count: usize, max: usize },
    #[error("transaction index is not contiguous: expected {expected}, got {actual}")]
    TransactionIndex { expected: u32, actual: u32 },
    #[error("too many events: {count} > {max}")]
    TooManyEvents { count: usize, max: usize },
    #[error("event index is not contiguous: expected {expected}, got {actual}")]
    EventIndex { expected: u32, actual: u32 },
    #[error("too many system events: {count} > {max}")]
    TooManySystemEvents { count: usize, max: usize },
    #[error("system event index is not contiguous: expected {expected}, got {actual}")]
    SystemEventIndex { expected: u32, actual: u32 },
    #[error("event payload too large: {size} > {max} bytes")]
    EventPayloadTooLarge { size: usize, max: usize },
    #[error("event payload bytes too large: {size} > {max} bytes")]
    EventPayloadBytesTooLarge { size: usize, max: usize },
    #[error("receipt too large: {size} > {max} bytes")]
    ReceiptTooLarge { size: usize, max: usize },
    #[error("commitment too large: {size} > {max} bytes")]
    CommitmentTooLarge { size: usize, max: usize },
    #[error("successful receipt has error code {code}")]
    SuccessHasErrorCode { code: u16 },
    #[error("failed receipt has no error code")]
    FailureMissingErrorCode,
    #[error("transaction ID does not match the canonical consensus identity")]
    TransactionIdMismatch,
    #[error("invalid receipt status {0}")]
    InvalidStatus(u8),
    #[error("canonical commitment encoding failed: {0}")]
    Encoding(String),
    #[error("commitment bytes are not canonical")]
    NonCanonicalEncoding,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(index: u32, event_payload: &[u8]) -> TransactionReceipt {
        let event = EventRecord::new(0, EventType::FILL, event_payload.to_vec())
            .expect("event within bounds");
        TransactionReceipt::success(
            index,
            [index as u8; 32],
            TransactionType::PLACE_ORDER,
            ResourceUsage::default(),
            vec![event],
        )
        .expect("receipt within bounds")
    }

    #[test]
    fn ordering_is_explicit_and_checked() {
        let mut invalid = receipt(1, b"fill");
        invalid.events[0].event_index = 1;
        assert!(matches!(
            invalid.validate(),
            Err(CommitmentError::EventIndex {
                expected: 0,
                actual: 1
            })
        ));

        let invalid_commitment = CommitmentV2 {
            schema_version: COMMITMENT_SCHEMA_VERSION,
            commitment_version: COMMITMENT_VERSION,
            receipts: vec![receipt(1, b"fill")],
            system_events: vec![],
        };
        assert!(matches!(
            invalid_commitment.validate(),
            Err(CommitmentError::TransactionIndex {
                expected: 0,
                actual: 1
            })
        ));
    }

    #[test]
    fn canonical_roundtrip_rejects_trailing_bytes() {
        let commitment = CommitmentV2::new(vec![receipt(0, b"fill")]).expect("valid");
        let bytes = commitment.canonical_bytes().expect("encodes");
        let decoded = CommitmentV2::from_canonical_bytes(&bytes).expect("canonical");
        assert_eq!(decoded, commitment);

        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            CommitmentV2::from_canonical_bytes(&trailing),
            Err(CommitmentError::NonCanonicalEncoding)
        );
    }

    #[test]
    fn error_display_text_is_not_part_of_the_hash() {
        let a = TransactionReceipt::failure(
            0,
            [9u8; 32],
            TransactionType::WITHDRAW,
            ErrorCode::ACCOUNT,
            ResourceUsage::default(),
            vec![],
        )
        .expect("valid failure");
        let b = a.clone();
        assert_eq!(a.hash().expect("hash"), b.hash().expect("hash"));
        assert_eq!(a.error_code, ErrorCode::ACCOUNT);
    }

    #[test]
    fn roots_are_ordered_and_domain_separated() {
        let first = CommitmentV2::new(vec![receipt(0, b"first")]).expect("valid");
        let second = CommitmentV2::new(vec![receipt(0, b"second")]).expect("valid");
        assert_ne!(
            first.receipts_root().expect("root"),
            second.receipts_root().expect("root")
        );
        assert_ne!(
            first.events_root().expect("root"),
            second.events_root().expect("root")
        );
        assert_ne!(
            first.root().expect("root"),
            first.events_root().expect("root")
        );

        let system =
            EventRecord::new(0, EventType::FUNDING, b"funding".to_vec()).expect("system event");
        let with_system =
            CommitmentV2::new_with_system_events(vec![receipt(0, b"first")], vec![system.clone()])
                .expect("valid system event");
        assert_ne!(
            first.events_root().expect("root"),
            with_system.events_root().expect("root")
        );
        assert_ne!(
            first.root().expect("root"),
            with_system.root().expect("root")
        );
        assert_ne!(
            system.hash(0).expect("transaction hash"),
            system.system_hash().expect("system hash")
        );

        let mut non_contiguous = system;
        non_contiguous.event_index = 1;
        assert!(matches!(
            CommitmentV2::new_with_system_events(vec![], vec![non_contiguous]),
            Err(CommitmentError::SystemEventIndex {
                expected: 0,
                actual: 1
            })
        ));
    }

    #[test]
    fn golden_root_is_stable() {
        let event =
            EventRecord::from_bincode(0, EventType::DEPOSIT, &("alice", 42i64)).expect("event");
        let receipt = TransactionReceipt::success(
            0,
            [0x11u8; 32],
            TransactionType::DEPOSIT,
            ResourceUsage {
                compute_units: 7,
                storage_read_bytes: 8,
                storage_write_bytes: 9,
            },
            vec![event],
        )
        .expect("receipt");
        let commitment = CommitmentV2::new(vec![receipt]).expect("commitment");
        assert_eq!(
            hex::encode(commitment.root().expect("root")),
            "71968dbf1d4c75a1a09387c205e59c89b21eb8ac7fda192bfdbfa80400192283"
        );
    }

    #[test]
    fn default_resource_usage_is_zero() {
        assert_eq!(
            ResourceUsage::default(),
            ResourceUsage {
                compute_units: 0,
                storage_read_bytes: 0,
                storage_write_bytes: 0,
            }
        );
    }

    #[test]
    fn constructor_rejects_oversized_canonical_receipts() {
        let events = (0..4)
            .map(|index| {
                EventRecord::new(index, EventType::FILL, vec![0u8; MAX_EVENT_PAYLOAD_BYTES])
                    .expect("event within per-event bound")
            })
            .collect();
        let receipt = TransactionReceipt::success(
            0,
            [0u8; 32],
            TransactionType::PLACE_ORDER,
            ResourceUsage::default(),
            events,
        )
        .expect("receipt fields are structurally valid");

        assert!(matches!(
            CommitmentV2::new(vec![receipt]),
            Err(CommitmentError::ReceiptTooLarge { .. })
        ));
    }
}
