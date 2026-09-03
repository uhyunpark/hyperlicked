//! Canonical, signed application transactions.
//!
//! A [`SignedEnvelope`] is the wire object for user transactions.  It is
//! deliberately independent from the HTTP representation.  The signature is
//! an explicit EIP-712 v1 digest over the chain domain, validity window, and
//! exact canonical action bytes.  Legacy `Transaction` values
//! remain available only through the explicit [`ConsensusTransaction::System`]
//! variant used by local fixtures and protocol-owned actions.

use alloy_primitives::keccak256;
use serde::{Deserialize, Serialize};

use crate::app::Transaction;
use crate::crypto::{recover_address, Signer};
use crate::types::{hash, Hash};

/// Current canonical envelope format version.
pub const ENVELOPE_VERSION: u8 = 1;

/// Maximum encoded envelope size accepted by the application layer.
pub const MAX_ENVELOPE_BYTES: usize = 64 * 1024;

/// Maximum encoded action payload size accepted by the application layer.
pub const MAX_ACTION_BYTES: usize = 48 * 1024;

/// Fixed tag included before the exact canonical bincode action bytes.
pub const ACTION_DOMAIN_TAG: &[u8] = b"HYPERLICKED-ACTION-V1\0";
pub const EIP712_V1_DOMAIN_TYPE: &str = "EIP712Domain(string name,string version,bytes32 salt)";
pub const EIP712_V1_TRANSACTION_TYPE: &str = "HyperLickedTransaction(bytes32 chainDomain,address signer,uint64 nonce,uint64 validAfter,uint64 validUntil,bytes32 actionHash)";
pub const EIP712_V1_NAME: &str = "HyperLicked";
pub const EIP712_V1_VERSION: &str = "1";

fn u256_word(value: u64) -> [u8; 32] {
    let mut word = [0u8; 32];
    word[24..].copy_from_slice(&value.to_be_bytes());
    word
}

/// Signature algorithms understood by consensus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SignatureScheme {
    /// EIP-712 v1 recoverable secp256k1 signature (`r || s || v`, 65 bytes).
    Eip712V1 = 1,
    /// Explicit local-only marker.  It is never valid unless the application
    /// was constructed with development envelopes enabled.
    Dev = 255,
}

/// A versioned, authenticated user action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub version: u8,
    /// Chain/application domain.  This value must be supplied by node
    /// configuration; it is not inferred from process-global environment.
    pub chain_domain: [u8; 32],
    /// Ethereum-style account identifier.
    pub signer: [u8; 20],
    pub nonce: u64,
    /// Earliest block timestamp at which the envelope is valid (milliseconds).
    pub valid_after: u64,
    /// Latest block timestamp at which the envelope is valid (milliseconds).
    pub valid_until: u64,
    /// Exact deterministic application action.
    pub action: Transaction,
    pub signature_scheme: SignatureScheme,
    /// Signature over [`Self::eip712_digest`].
    pub signature: Vec<u8>,
}

/// Consensus payload item.  Unsigned legacy actions are explicit system
/// transactions and are not accepted as user envelopes. In particular,
/// `SubmitEvidence` is privileged and must use the validated system path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsensusTransaction {
    /// Authenticated user action.
    Signed(SignedEnvelope),
    /// Protocol-owned or local fixture action.  API user routes must not use
    /// this variant for trading or account mutations.
    System(Transaction),
}

/// Envelope validation failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("unsupported envelope version {0}")]
    UnsupportedVersion(u8),
    #[error("chain domain mismatch")]
    WrongDomain,
    #[error("invalid validity interval")]
    InvalidValidity,
    #[error("envelope is not yet valid")]
    NotYetValid,
    #[error("envelope expired")]
    Expired,
    #[error("action payload exceeds limit")]
    ActionTooLarge,
    #[error("encoded envelope exceeds limit")]
    EnvelopeTooLarge,
    #[error("invalid signer/action binding")]
    SignerMismatch,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("development envelope is disabled")]
    DevEnvelopeDisabled,
    #[error("privileged system action must use the system consensus path")]
    SystemActionNotAllowed,
    #[error("canonical encoding failed: {0}")]
    Encoding(String),
}

impl SignedEnvelope {
    /// Construct an envelope without performing signature verification.
    /// Call [`Self::validate_for_block`] before admitting it to execution.
    pub fn new(
        chain_domain: [u8; 32],
        signer: [u8; 20],
        nonce: u64,
        valid_after: u64,
        valid_until: u64,
        action: Transaction,
        signature_scheme: SignatureScheme,
        signature: Vec<u8>,
    ) -> Result<Self, EnvelopeError> {
        let envelope = Self {
            version: ENVELOPE_VERSION,
            chain_domain,
            signer,
            nonce,
            valid_after,
            valid_until,
            action,
            signature_scheme,
            signature,
        };
        envelope.validate_structure()?;
        Ok(envelope)
    }

    /// Construct and sign a canonical EIP-712 v1 envelope.
    pub fn sign(
        chain_domain: [u8; 32],
        signer: &Signer,
        nonce: u64,
        valid_after: u64,
        valid_until: u64,
        action: Transaction,
    ) -> Result<Self, EnvelopeError> {
        let signer_address = signer.address();
        let mut envelope = Self {
            version: ENVELOPE_VERSION,
            chain_domain,
            signer: signer_address.into_array(),
            nonce,
            valid_after,
            valid_until,
            action,
            signature_scheme: SignatureScheme::Eip712V1,
            signature: Vec::new(),
        };
        envelope.validate_structure()?;
        let digest = envelope.eip712_digest()?;
        envelope.signature = signer.sign(&digest).to_vec();
        envelope.validate_structure()?;
        Ok(envelope)
    }

    /// Exact canonical action bytes used by the EIP-712 `actionHash` field.
    /// The encoding is `ACTION_DOMAIN_TAG || bincode(Transaction)`.  The
    /// fixed protocol tag prevents signatures from being replayed as a
    /// different protocol's opaque payload.
    pub fn action_bytes(&self) -> Result<Vec<u8>, EnvelopeError> {
        let action =
            bincode::serialize(&self.action).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        if action.len() > MAX_ACTION_BYTES {
            return Err(EnvelopeError::ActionTooLarge);
        }
        let mut bytes = Vec::with_capacity(ACTION_DOMAIN_TAG.len() + action.len());
        bytes.extend_from_slice(ACTION_DOMAIN_TAG);
        bytes.extend_from_slice(&action);
        Ok(bytes)
    }

    /// Keccak hash of the fixed-tagged, length-prefixed action bytes.
    pub fn action_hash(&self) -> Result<[u8; 32], EnvelopeError> {
        Ok(keccak256(self.action_bytes()?.as_slice()).into())
    }

    /// EIP-712 domain separator with chain domain as `bytes32 salt`.
    pub fn eip712_domain_separator(&self) -> [u8; 32] {
        let mut encoded = Vec::with_capacity(32 * 4);
        encoded.extend_from_slice(keccak256(EIP712_V1_DOMAIN_TYPE.as_bytes()).as_slice());
        encoded.extend_from_slice(keccak256(EIP712_V1_NAME.as_bytes()).as_slice());
        encoded.extend_from_slice(keccak256(EIP712_V1_VERSION.as_bytes()).as_slice());
        encoded.extend_from_slice(&self.chain_domain);
        keccak256(encoded).into()
    }

    /// EIP-712 struct hash for `HyperLickedTransaction`.
    pub fn eip712_struct_hash(&self) -> Result<[u8; 32], EnvelopeError> {
        let mut encoded = Vec::with_capacity(32 * 7);
        encoded.extend_from_slice(keccak256(EIP712_V1_TRANSACTION_TYPE.as_bytes()).as_slice());
        encoded.extend_from_slice(&self.chain_domain);
        encoded.extend_from_slice(&[0u8; 12]);
        encoded.extend_from_slice(&self.signer);
        encoded.extend_from_slice(&u256_word(self.nonce));
        encoded.extend_from_slice(&u256_word(self.valid_after));
        encoded.extend_from_slice(&u256_word(self.valid_until));
        encoded.extend_from_slice(&self.action_hash()?);
        Ok(keccak256(encoded).into())
    }

    /// EIP-712 v1 digest signed by the secp256k1 signer.
    pub fn eip712_digest(&self) -> Result<[u8; 32], EnvelopeError> {
        let mut encoded = Vec::with_capacity(66);
        encoded.extend_from_slice(&[0x19, 0x01]);
        encoded.extend_from_slice(&self.eip712_domain_separator());
        encoded.extend_from_slice(&self.eip712_struct_hash()?);
        Ok(keccak256(encoded).into())
    }

    /// Return the exact 32-byte digest authenticated by the signature.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, EnvelopeError> {
        Ok(self.eip712_digest()?.to_vec())
    }

    /// Canonical encoded envelope including the signature.
    pub fn encoded_bytes(&self) -> Result<Vec<u8>, EnvelopeError> {
        let bytes = bincode::serialize(self).map_err(|e| EnvelopeError::Encoding(e.to_string()))?;
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(EnvelopeError::EnvelopeTooLarge);
        }
        Ok(bytes)
    }

    /// Hash used for mempool identity and response IDs.
    pub fn hash(&self) -> Result<Hash, EnvelopeError> {
        Ok(hash(&self.encoded_bytes()?))
    }

    pub fn signer_address(&self) -> String {
        format!("0x{}", hex::encode(self.signer))
    }

    /// Validate invariant fields that do not depend on a block.
    pub fn validate_structure(&self) -> Result<(), EnvelopeError> {
        if self.version != ENVELOPE_VERSION {
            return Err(EnvelopeError::UnsupportedVersion(self.version));
        }
        if self.valid_until == 0 || self.valid_after > self.valid_until {
            return Err(EnvelopeError::InvalidValidity);
        }
        self.action_bytes()?;
        self.encoded_bytes()?;
        Ok(())
    }

    /// Validate domain, timestamp, action binding, size, and cryptographic
    /// signature against the exact block execution context.
    pub fn validate_for_block(
        &self,
        chain_domain: [u8; 32],
        block_timestamp: u64,
        allow_dev: bool,
    ) -> Result<(), EnvelopeError> {
        self.validate_structure()?;
        if matches!(&self.action, Transaction::SubmitEvidence { .. }) {
            return Err(EnvelopeError::SystemActionNotAllowed);
        }
        if self.chain_domain != chain_domain {
            return Err(EnvelopeError::WrongDomain);
        }
        if block_timestamp < self.valid_after {
            return Err(EnvelopeError::NotYetValid);
        }
        if block_timestamp > self.valid_until {
            return Err(EnvelopeError::Expired);
        }
        if !self.action_matches_signer() {
            return Err(EnvelopeError::SignerMismatch);
        }

        match self.signature_scheme {
            SignatureScheme::Eip712V1 => {
                if self.signature.len() != 65 {
                    return Err(EnvelopeError::InvalidSignature);
                }
                let digest = self.eip712_digest()?;
                let recovered = recover_address(&digest, &self.signature)
                    .map_err(|_| EnvelopeError::InvalidSignature)?;
                if recovered.into_array() != self.signer {
                    return Err(EnvelopeError::InvalidSignature);
                }
            }
            SignatureScheme::Dev => {
                if !allow_dev {
                    return Err(EnvelopeError::DevEnvelopeDisabled);
                }
                if self.signature != b"dev" {
                    return Err(EnvelopeError::InvalidSignature);
                }
            }
        }

        Ok(())
    }

    /// Ensure the action's authenticated owner is exactly the envelope signer.
    pub fn action_matches_signer(&self) -> bool {
        let owner = self.action.trader_address();
        let expected = self.signer_address();
        owner.eq_ignore_ascii_case(&expected)
    }
}

impl ConsensusTransaction {
    pub fn action(&self) -> &Transaction {
        match self {
            Self::Signed(envelope) => &envelope.action,
            Self::System(tx) => tx,
        }
    }

    pub fn bucket(&self) -> u8 {
        match self {
            Self::Signed(envelope) => envelope.action.bucket(),
            Self::System(tx) => tx.bucket(),
        }
    }

    pub fn trader_address(&self) -> String {
        match self {
            Self::Signed(envelope) => envelope.signer_address(),
            Self::System(tx) => tx.trader_address().to_string(),
        }
    }

    /// Exact canonical identity bytes used by mempool/proposal identity and
    /// execution receipts.  System transactions use bincode here as well as
    /// signed envelopes; the legacy JSON `Transaction::to_bytes()` helper is
    /// a presentation/compatibility format, not a consensus identity.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, EnvelopeError> {
        match self {
            Self::Signed(envelope) => envelope.encoded_bytes(),
            Self::System(tx) => {
                bincode::serialize(tx).map_err(|error| EnvelopeError::Encoding(error.to_string()))
            }
        }
    }

    pub fn hash(&self) -> Result<Hash, EnvelopeError> {
        Ok(hash(&self.canonical_bytes()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{OrderType, Side, TriggerType};

    fn domain() -> [u8; 32] {
        [7u8; 32]
    }

    fn action(address: String) -> Transaction {
        Transaction::PlaceOrder {
            trader: address,
            symbol: "BTC-USDT".to_string(),
            side: Side::Bid,
            price: 5_000_000,
            size: 100,
            order_type: OrderType::Gtc,
            reduce_only: false,
        }
    }

    #[test]
    fn canonical_encoding_is_stable_and_signature_verifies() {
        let signer =
            Signer::from_hex("0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80")
                .unwrap();
        let envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        assert_eq!(
            envelope.signing_bytes().unwrap(),
            envelope.signing_bytes().unwrap()
        );
        envelope.validate_for_block(domain(), 50, false).unwrap();
        assert!(envelope.encoded_bytes().unwrap().len() <= MAX_ENVELOPE_BYTES);
    }

    #[test]
    fn frontend_canonical_place_order_golden_vector_matches_rust() {
        let trader = "0x1111111111111111111111111111111111111111".to_string();
        let action = Transaction::PlaceOrder {
            trader,
            symbol: "BTC-USDT".to_string(),
            side: Side::Ask,
            price: -5_000_000,
            size: 100,
            order_type: OrderType::Alo,
            reduce_only: true,
        };
        let envelope = SignedEnvelope::new(
            [7u8; 32],
            [0x11u8; 20],
            3,
            2,
            100,
            action.clone(),
            SignatureScheme::Eip712V1,
            Vec::new(),
        )
        .expect("unsigned fixture should be structurally valid");

        assert_eq!(
            hex::encode(bincode::serialize(&action).unwrap()),
            "000000002a0000000000000030783131313131313131313131313131313131313131313131313131313131313131313131313131313108000000000000004254432d5553445401000000c0b4b3ffffffffff64000000000000000200000001"
        );
        assert_eq!(
            hex::encode(envelope.action_hash().unwrap()),
            "910d1b76d0c97401a44bcb0cb5363665a4985dda32432f82c9fcc1b9cdbc3ed7"
        );
        assert_eq!(
            hex::encode(envelope.eip712_digest().unwrap()),
            "1c2d5fdf66cec701989534bc738bc0692d5e520977c82872183aaa458418425f"
        );

        let action_hash = |action| {
            let envelope = SignedEnvelope::new(
                [7u8; 32],
                [0x11u8; 20],
                3,
                2,
                100,
                action,
                SignatureScheme::Eip712V1,
                Vec::new(),
            )
            .unwrap();
            hex::encode(envelope.action_hash().unwrap())
        };
        let trader = "0x1111111111111111111111111111111111111111".to_string();
        assert_eq!(
            action_hash(Transaction::CancelOrder {
                trader: trader.clone(),
                order_id: "42".to_string(),
            }),
            "acc76479ddf2bc1981dc0b4de2625d0061c6cbf1cde3d1559863df91a5bc485c"
        );
        assert_eq!(
            action_hash(Transaction::PlaceTriggerOrder {
                trader: trader.clone(),
                symbol: "BTC-USDT".to_string(),
                trigger_type: TriggerType::TakeProfit,
                trigger_price: 6_000_000,
                size: -100,
                limit_price: None,
                cloid: Some("client-1".to_string()),
            }),
            "93bfe12a7e20050ae21a0f10f96f1c6d7a247bd6f1eaf2c464a2a9f3d0c65c59"
        );
        assert_eq!(
            action_hash(Transaction::CancelTriggerOrder {
                trader: trader.clone(),
                trigger_order_id: "trig-42".to_string(),
            }),
            "0e39d22b1d0e329c2243e5a80a426a060ac685bcd608505e95e2fe8ab8e2de71"
        );
        assert_eq!(
            action_hash(Transaction::CancelTriggerOrderByCloid {
                trader,
                symbol: "BTC-USDT".to_string(),
                cloid: "client-1".to_string(),
            }),
            "346e3bc34aff59dee676fa8cf3b96edefae4e00626c1b8c12e131260bdc0e6f3"
        );
    }

    #[test]
    fn eip712_digest_binds_domain_signer_nonce_validity_and_action() {
        let signer = Signer::generate();
        let mut envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            1,
            2,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        let original = envelope.eip712_digest().unwrap();

        envelope.chain_domain[0] ^= 1;
        assert_ne!(original, envelope.eip712_digest().unwrap());
        envelope.chain_domain = domain();
        envelope.signer[0] ^= 1;
        assert_ne!(original, envelope.eip712_digest().unwrap());
        envelope.signer = signer.address().into_array();
        envelope.nonce += 1;
        assert_ne!(original, envelope.eip712_digest().unwrap());
        envelope.nonce = 1;
        envelope.valid_after += 1;
        assert_ne!(original, envelope.eip712_digest().unwrap());
        envelope.valid_after = 2;
        envelope.valid_until += 1;
        assert_ne!(original, envelope.eip712_digest().unwrap());
        envelope.valid_until = 100;
        envelope.action = Transaction::CancelOrder {
            trader: envelope.signer_address(),
            order_id: "different".to_string(),
        };
        assert_ne!(original, envelope.eip712_digest().unwrap());
    }

    #[test]
    fn altered_action_wrong_domain_and_bad_signature_are_rejected() {
        let signer = Signer::generate();
        let mut envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();

        envelope.action = Transaction::CancelOrder {
            trader: envelope.signer_address(),
            order_id: "other".into(),
        };
        assert!(matches!(
            envelope.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::InvalidSignature)
        ));

        let mut fresh = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        assert!(matches!(
            fresh.validate_for_block([8u8; 32], 50, false),
            Err(EnvelopeError::WrongDomain)
        ));
        fresh.signature[0] ^= 1;
        assert!(matches!(
            fresh.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::InvalidSignature)
        ));

        let mut mismatched = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        mismatched.signer = [9u8; 20];
        assert!(matches!(
            mismatched.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::SignerMismatch)
        ));
    }

    #[test]
    fn native_hyck_transfer_binds_envelope_signer_to_from() {
        let signer = Signer::generate();
        let signer_address = format!("{:?}", signer.address());
        let mut envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            Transaction::TransferHyck {
                from: signer_address.clone(),
                to: "0x1111111111111111111111111111111111111111".to_string(),
                amount: 1_000_000,
            },
        )
        .unwrap();
        envelope.validate_for_block(domain(), 50, false).unwrap();

        envelope.action = Transaction::TransferHyck {
            from: "0x2222222222222222222222222222222222222222".to_string(),
            to: "0x1111111111111111111111111111111111111111".to_string(),
            amount: 1_000_000,
        };
        assert!(matches!(
            envelope.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::SignerMismatch)
        ));
    }

    #[test]
    fn high_s_and_noncanonical_recovery_ids_are_rejected() {
        // secp256k1 group order, big-endian.
        const ORDER: [u8; 32] = [
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
            0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c,
            0xd0, 0x36, 0x41, 0x41,
        ];

        fn negate_scalar(scalar: [u8; 32]) -> [u8; 32] {
            let mut result = [0u8; 32];
            let mut borrow = 0i16;
            for index in (0..32).rev() {
                let difference = ORDER[index] as i16 - scalar[index] as i16 - borrow;
                if difference < 0 {
                    result[index] = (difference + 256) as u8;
                    borrow = 1;
                } else {
                    result[index] = difference as u8;
                    borrow = 0;
                }
            }
            result
        }

        let signer = Signer::generate();
        let envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        let original_hash = envelope.hash().unwrap();
        let mut high_s = envelope.clone();
        let mut high_signature = high_s.signature.clone();
        let mut low_s = [0u8; 32];
        low_s.copy_from_slice(&high_signature[32..64]);
        high_signature[32..64].copy_from_slice(&negate_scalar(low_s));
        high_signature[64] ^= 1;
        high_s.signature = high_signature;

        // The action and nonce are unchanged, but malleating s must not make
        // another valid envelope identity.
        assert_ne!(high_s.hash().unwrap(), original_hash);
        assert!(matches!(
            high_s.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::InvalidSignature)
        ));

        let signer = Signer::generate();
        let mut invalid_v = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        invalid_v.signature[64] = 2;
        assert!(matches!(
            invalid_v.validate_for_block(domain(), 50, false),
            Err(EnvelopeError::InvalidSignature)
        ));
    }

    #[test]
    fn expiry_and_size_limits_are_enforced() {
        let signer = Signer::generate();
        let envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            10,
            20,
            action(format!("{:?}", signer.address())),
        )
        .unwrap();
        assert!(matches!(
            envelope.validate_for_block(domain(), 21, false),
            Err(EnvelopeError::Expired)
        ));

        let mut oversized = envelope.clone();
        oversized.signature = vec![0; MAX_ENVELOPE_BYTES];
        assert!(matches!(
            oversized.encoded_bytes(),
            Err(EnvelopeError::EnvelopeTooLarge)
        ));

        let mut large_config = crate::app::MarketConfig::default();
        large_config.symbol = "x".repeat(MAX_ACTION_BYTES);
        assert!(matches!(
            SignedEnvelope::new(
                domain(),
                signer.address().into_array(),
                0,
                0,
                100,
                Transaction::AddMarket {
                    admin: format!("{:?}", signer.address()),
                    config: large_config,
                    initial_mark_price: 1,
                },
                SignatureScheme::Dev,
                b"dev".to_vec(),
            ),
            Err(EnvelopeError::ActionTooLarge)
        ));
    }

    #[test]
    fn development_envelope_is_explicit() {
        let signer = Signer::generate();
        let envelope = SignedEnvelope::new(
            domain(),
            signer.address().into_array(),
            0,
            0,
            10,
            action(format!("{:?}", signer.address())),
            SignatureScheme::Dev,
            b"dev".to_vec(),
        )
        .unwrap();
        assert!(matches!(
            envelope.validate_for_block(domain(), 1, false),
            Err(EnvelopeError::DevEnvelopeDisabled)
        ));
        envelope.validate_for_block(domain(), 1, true).unwrap();
    }

    #[test]
    fn execution_consumes_nonce_on_failed_action_and_rejects_replay() {
        let signer = Signer::generate();
        let address = format!("{:?}", signer.address());
        let mut state = crate::app::AppState::new_with_chain_domain(domain());
        state.set_allow_dev_envelopes(false);
        let envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            Transaction::Withdraw {
                trader: address,
                amount: 1,
            },
        )
        .unwrap();

        // Invalid action: there is no balance.  Action state stays untouched,
        // but the valid signed nonce is consumed for replay protection.
        assert!(state
            .execute_consensus_transaction(ConsensusTransaction::Signed(envelope.clone()), 50,)
            .is_err());
        assert_eq!(state.accounts().get_nonce(&envelope.signer_address()), 1);
        assert_eq!(
            state
                .account(&envelope.signer_address())
                .map(|account| account.balance),
            Some(0)
        );

        assert!(state
            .execute_consensus_transaction(ConsensusTransaction::Signed(envelope), 50)
            .is_err());
        assert_eq!(
            state
                .accounts()
                .get_nonce(&format!("0x{}", hex::encode(signer.address()))),
            1
        );
    }

    #[test]
    fn admission_revalidates_signature_and_rejects_pending_nonce_duplicate() {
        let signer = Signer::generate();
        let address = format!("{:?}", signer.address());
        let mut state = crate::app::AppState::new_with_chain_domain_and_dev(domain(), false);
        let envelope = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            10,
            100,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 100,
            },
        )
        .unwrap();
        state.submit_envelope_at(envelope.clone(), 50).unwrap();
        assert!(state.submit_envelope_at(envelope, 50).is_err());

        // A different action cannot reuse the same signer/nonce while the
        // first envelope is pending, even though it would have a different
        // transaction hash.
        let different_action_same_nonce = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            10,
            100,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 200,
            },
        )
        .unwrap();
        assert!(matches!(
            state.submit_envelope_at(different_action_same_nonce, 50),
            Err(crate::app::AppError::InvalidEnvelope(error))
                if error.contains("duplicate signer nonce")
        ));

        let mut bad = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            10,
            100,
            Transaction::Deposit {
                trader: format!("{:?}", signer.address()),
                amount: 200,
            },
        )
        .unwrap();
        bad.signature[0] ^= 1;
        assert!(matches!(
            state.submit_envelope_at(bad, 50),
            Err(crate::app::AppError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn nonce_gap_admission_defers_out_of_order_execution_until_predecessors_arrive() {
        let signer = Signer::generate();
        let address = format!("{:?}", signer.address());
        let mut state = crate::app::AppState::new_with_chain_domain(domain());

        let envelope = |nonce, amount| {
            SignedEnvelope::sign(
                domain(),
                &signer,
                nonce,
                0,
                100,
                Transaction::Deposit {
                    trader: address.clone(),
                    amount,
                },
            )
            .unwrap()
        };
        let nonce_two = envelope(2, 30);
        let nonce_zero = envelope(0, 10);
        let nonce_one = envelope(1, 20);

        // Admission does not reserve or mutate the committed nonce, so all
        // three distinct signer/nonce pairs can wait in the mempool.
        state.submit_envelope_at(nonce_two.clone(), 50).unwrap();
        state.submit_envelope_at(nonce_zero.clone(), 50).unwrap();
        state.submit_envelope_at(nonce_one.clone(), 50).unwrap();
        assert_eq!(state.accounts().get_nonce(&address), 0);

        // Gap tolerance is an admission/transport feature only. Consensus
        // execution remains strict and cannot consume nonce 2 first.
        assert!(state
            .execute_consensus_transaction(ConsensusTransaction::Signed(nonce_two.clone()), 50)
            .is_err());
        assert_eq!(state.accounts().get_nonce(&address), 0);
        assert!(state.account(&address).is_none());

        state
            .execute_consensus_transaction(ConsensusTransaction::Signed(nonce_zero), 50)
            .unwrap();
        state
            .execute_consensus_transaction(ConsensusTransaction::Signed(nonce_one), 50)
            .unwrap();
        state
            .execute_consensus_transaction(ConsensusTransaction::Signed(nonce_two), 50)
            .unwrap();

        let account = state.account(&address).unwrap();
        assert_eq!(account.nonce, 3);
        assert!(account.pending_nonces.is_empty());
        assert_eq!(account.balance, 60);
    }

    #[test]
    fn nonce_gap_admission_rejects_gap_above_bound_and_duplicate_pending_nonce() {
        let signer = Signer::generate();
        let address = format!("{:?}", signer.address());
        let mut state = crate::app::AppState::new_with_chain_domain(domain());
        let envelope = |nonce| {
            SignedEnvelope::sign(
                domain(),
                &signer,
                nonce,
                0,
                100,
                Transaction::Deposit {
                    trader: address.clone(),
                    amount: 1,
                },
            )
            .unwrap()
        };

        let pending = envelope(crate::app::accounts::MAX_NONCE_GAP);
        state.submit_envelope_at(pending.clone(), 50).unwrap();
        assert!(matches!(
            state.submit_envelope_at(pending, 50),
            Err(crate::app::AppError::InvalidEnvelope(error))
                if error.contains("duplicate signer nonce already pending")
        ));

        let too_far = envelope(crate::app::accounts::MAX_NONCE_GAP + 1);
        assert!(matches!(
            state.submit_envelope_at(too_far, 50),
            Err(crate::app::AppError::InvalidEnvelope(error))
                if error.contains("gap too large")
                    && error.contains("max gap is 10")
        ));
    }

    #[test]
    fn failed_strict_action_consumes_nonce_and_rejects_replay() {
        let signer = Signer::generate();
        let address = format!("{:?}", signer.address());
        let mut state = crate::app::AppState::new_with_chain_domain(domain());
        let failed = SignedEnvelope::sign(
            domain(),
            &signer,
            0,
            0,
            100,
            Transaction::Withdraw {
                trader: address.clone(),
                amount: 1,
            },
        )
        .unwrap();

        // A future nonce is rejected before action execution and does not
        // materialize an account or pending nonce marker.
        let future = SignedEnvelope::sign(
            domain(),
            &signer,
            2,
            0,
            100,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 1,
            },
        )
        .unwrap();
        assert!(state
            .execute_consensus_transaction(ConsensusTransaction::Signed(future), 50)
            .is_err());
        assert_eq!(state.accounts().get_nonce(&address), 0);
        assert!(state.account(&address).is_none());

        assert!(matches!(
            state.execute_consensus_transaction(ConsensusTransaction::Signed(failed.clone()), 50),
            Err(crate::app::AppError::Account(
                crate::app::accounts::AccountError::InsufficientBalance
            ))
        ));
        assert_eq!(state.accounts().get_nonce(&address), 1);

        // The failed action's nonce cannot be replayed, while the next
        // contiguous nonce remains executable.
        assert!(matches!(
            state.execute_consensus_transaction(ConsensusTransaction::Signed(failed), 50),
            Err(crate::app::AppError::Account(
                crate::app::accounts::AccountError::InvalidNonce {
                    expected: 1,
                    got: 0
                }
            ))
        ));
        let next = SignedEnvelope::sign(
            domain(),
            &signer,
            1,
            0,
            100,
            Transaction::Deposit {
                trader: address.clone(),
                amount: 1,
            },
        )
        .unwrap();
        state
            .execute_consensus_transaction(ConsensusTransaction::Signed(next), 50)
            .unwrap();
        assert_eq!(state.accounts().get_nonce(&address), 2);
        assert!(state.account(&address).unwrap().pending_nonces.is_empty());
    }

    #[test]
    fn system_identity_uses_the_same_bincode_bytes_as_receipts() {
        let entry = ConsensusTransaction::System(Transaction::Deposit {
            trader: "alice".to_string(),
            amount: 42,
        });
        let canonical = bincode::serialize(entry.action()).unwrap();

        assert_eq!(entry.canonical_bytes().unwrap(), canonical);
        assert_eq!(entry.hash().unwrap(), hash(&canonical));
        assert_ne!(entry.hash().unwrap(), hash(&entry.action().to_bytes()));
    }
}
