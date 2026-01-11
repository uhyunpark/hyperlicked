//! BLS12-381 Signature Aggregation
//!
//! Provides BLS signatures for efficient vote aggregation in consensus.
//! A single aggregated signature proves 2f+1 validators voted for a block.
//!
//! ## Usage
//!
//! ```ignore
//! // Generate key pair
//! let sk = BlsSecretKey::generate();
//! let pk = sk.public_key();
//!
//! // Sign a message
//! let sig = sk.sign(b"vote for block");
//!
//! // Verify
//! assert!(pk.verify(b"vote for block", &sig));
//!
//! // Aggregate multiple signatures
//! let agg_sig = aggregate_signatures(&[sig1, sig2, sig3])?;
//! assert!(verify_aggregate(message, &agg_sig, &[pk1, pk2, pk3]));
//! ```

use blst::min_pk::{AggregatePublicKey, AggregateSignature, PublicKey, SecretKey, Signature};
use blst::BLST_ERROR;

/// Domain separation tag for BLS signatures
const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_HYPERLICKED_";

/// BLS secret key wrapper
#[derive(Clone)]
pub struct BlsSecretKey {
    inner: SecretKey,
}

impl BlsSecretKey {
    /// Generate a new random BLS key
    pub fn generate() -> Self {
        let mut ikm = [0u8; 32];
        getrandom::getrandom(&mut ikm).expect("RNG failure");
        let sk = SecretKey::key_gen(&ikm, &[]).expect("key gen failed");
        Self { inner: sk }
    }

    /// Create from 32-byte seed (deterministic)
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let sk = SecretKey::key_gen(seed, &[]).expect("key gen failed");
        Self { inner: sk }
    }

    /// Get the corresponding public key
    pub fn public_key(&self) -> BlsPublicKey {
        BlsPublicKey {
            inner: self.inner.sk_to_pk(),
        }
    }

    /// Sign a message
    pub fn sign(&self, message: &[u8]) -> BlsSignature {
        let sig = self.inner.sign(message, DST, &[]);
        BlsSignature { inner: sig }
    }

    /// Serialize to 32 bytes
    pub fn to_bytes(&self) -> [u8; 32] {
        self.inner.to_bytes()
    }
}

/// BLS public key wrapper (48 bytes compressed G1 point)
#[derive(Clone, Debug)]
pub struct BlsPublicKey {
    inner: PublicKey,
}

impl BlsPublicKey {
    /// Serialize to 48 bytes
    pub fn to_bytes(&self) -> [u8; 48] {
        self.inner.to_bytes()
    }

    /// Deserialize from 48 bytes
    pub fn from_bytes(bytes: &[u8; 48]) -> Result<Self, BlsError> {
        let pk = PublicKey::from_bytes(bytes).map_err(|_| BlsError::InvalidPublicKey)?;
        Ok(Self { inner: pk })
    }

    /// Verify a signature
    pub fn verify(&self, message: &[u8], signature: &BlsSignature) -> bool {
        signature
            .inner
            .verify(true, message, DST, &[], &self.inner, true)
            == BLST_ERROR::BLST_SUCCESS
    }
}

/// BLS signature wrapper (96 bytes compressed G2 point)
#[derive(Clone, Debug)]
pub struct BlsSignature {
    inner: Signature,
}

impl BlsSignature {
    /// Serialize to 96 bytes
    pub fn to_bytes(&self) -> [u8; 96] {
        self.inner.to_bytes()
    }

    /// Deserialize from 96 bytes
    pub fn from_bytes(bytes: &[u8; 96]) -> Result<Self, BlsError> {
        let sig = Signature::from_bytes(bytes).map_err(|_| BlsError::InvalidSignature)?;
        Ok(Self { inner: sig })
    }

    /// Deserialize from slice (must be 96 bytes)
    pub fn from_slice(bytes: &[u8]) -> Result<Self, BlsError> {
        if bytes.len() != 96 {
            return Err(BlsError::InvalidSignature);
        }
        let mut arr = [0u8; 96];
        arr.copy_from_slice(bytes);
        Self::from_bytes(&arr)
    }
}

/// Aggregate multiple BLS signatures into one
///
/// All signatures must be over the same message for the aggregated
/// signature to verify correctly with the aggregated public key.
pub fn aggregate_signatures(signatures: &[BlsSignature]) -> Result<BlsSignature, BlsError> {
    if signatures.is_empty() {
        return Err(BlsError::NoSignatures);
    }

    let sigs: Vec<&Signature> = signatures.iter().map(|s| &s.inner).collect();
    let agg = AggregateSignature::aggregate(&sigs, true).map_err(|_| BlsError::AggregationFailed)?;

    Ok(BlsSignature {
        inner: agg.to_signature(),
    })
}

/// Aggregate multiple BLS public keys
pub fn aggregate_public_keys(public_keys: &[BlsPublicKey]) -> Result<BlsPublicKey, BlsError> {
    if public_keys.is_empty() {
        return Err(BlsError::NoPublicKeys);
    }

    let pks: Vec<&PublicKey> = public_keys.iter().map(|p| &p.inner).collect();
    let agg = AggregatePublicKey::aggregate(&pks, true).map_err(|_| BlsError::AggregationFailed)?;

    Ok(BlsPublicKey {
        inner: agg.to_public_key(),
    })
}

/// Verify an aggregated signature against aggregated public key
///
/// All signers must have signed the same message.
pub fn verify_aggregate(
    message: &[u8],
    signature: &BlsSignature,
    public_keys: &[BlsPublicKey],
) -> bool {
    match aggregate_public_keys(public_keys) {
        Ok(agg_pk) => agg_pk.verify(message, signature),
        Err(_) => false,
    }
}

/// Errors that can occur during BLS operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum BlsError {
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("no signatures to aggregate")]
    NoSignatures,
    #[error("no public keys to aggregate")]
    NoPublicKeys,
    #[error("aggregation failed")]
    AggregationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_generation() {
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();

        // Public key should be 48 bytes
        assert_eq!(pk.to_bytes().len(), 48);
    }

    #[test]
    fn test_deterministic_key() {
        let seed = [42u8; 32];
        let sk1 = BlsSecretKey::from_seed(&seed);
        let sk2 = BlsSecretKey::from_seed(&seed);

        let pk1 = sk1.public_key();
        let pk2 = sk2.public_key();

        assert_eq!(pk1.to_bytes(), pk2.to_bytes());
    }

    #[test]
    fn test_sign_and_verify() {
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();
        let message = b"test message";

        let sig = sk.sign(message);
        assert!(pk.verify(message, &sig));

        // Wrong message should fail
        assert!(!pk.verify(b"wrong message", &sig));
    }

    #[test]
    fn test_signature_serialization() {
        let sk = BlsSecretKey::generate();
        let sig = sk.sign(b"test");

        let bytes = sig.to_bytes();
        assert_eq!(bytes.len(), 96);

        let sig2 = BlsSignature::from_bytes(&bytes).unwrap();
        assert_eq!(sig.to_bytes(), sig2.to_bytes());
    }

    #[test]
    fn test_public_key_serialization() {
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();

        let bytes = pk.to_bytes();
        let pk2 = BlsPublicKey::from_bytes(&bytes).unwrap();

        assert_eq!(pk.to_bytes(), pk2.to_bytes());
    }

    #[test]
    fn test_aggregate_signatures() {
        let message = b"consensus vote";

        // Generate 3 key pairs
        let keys: Vec<BlsSecretKey> = (0..3)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();
        let signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

        // Aggregate
        let agg_sig = aggregate_signatures(&signatures).unwrap();

        // Verify aggregated signature
        assert!(verify_aggregate(message, &agg_sig, &public_keys));

        // Wrong message should fail
        assert!(!verify_aggregate(b"wrong message", &agg_sig, &public_keys));
    }

    #[test]
    fn test_aggregate_single_signature() {
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();
        let message = b"single signer";

        let sig = sk.sign(message);
        let agg_sig = aggregate_signatures(&[sig]).unwrap();

        assert!(verify_aggregate(message, &agg_sig, &[pk]));
    }

    #[test]
    fn test_aggregate_empty_fails() {
        let result = aggregate_signatures(&[]);
        assert!(matches!(result, Err(BlsError::NoSignatures)));

        let result = aggregate_public_keys(&[]);
        assert!(matches!(result, Err(BlsError::NoPublicKeys)));
    }

    #[test]
    fn test_signature_size_vs_ecdsa() {
        // BLS signature: 96 bytes
        let sk = BlsSecretKey::generate();
        let sig = sk.sign(b"test");
        assert_eq!(sig.to_bytes().len(), 96);

        // BLS public key: 48 bytes
        let pk = sk.public_key();
        assert_eq!(pk.to_bytes().len(), 48);

        // Compare to ECDSA: sig ~65 bytes, pubkey 33/65 bytes
        // BLS aggregation of N signatures = 96 bytes (constant)
        // ECDSA concatenation of N signatures = 65*N bytes
    }
}
