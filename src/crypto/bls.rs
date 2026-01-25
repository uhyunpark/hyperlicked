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

/// Batch verify multiple BLS signatures over the SAME message
///
/// This is more efficient than verifying each signature individually
/// because it uses BLS's ability to aggregate and verify in one operation.
///
/// If batch verification fails, caller should fall back to individual
/// verification to identify which signature(s) are invalid (Byzantine detection).
///
/// Returns true if ALL signatures are valid, false otherwise.
#[cfg(feature = "bls_batch_verify")]
pub fn verify_batch(
    message: &[u8],
    signatures: &[&BlsSignature],
    public_keys: &[&BlsPublicKey],
) -> bool {
    if signatures.len() != public_keys.len() || signatures.is_empty() {
        return false;
    }

    // Use blst's multi-verification capability
    // For same-message batch verification, we can aggregate and verify
    let sigs: Vec<&Signature> = signatures.iter().map(|s| &s.inner).collect();
    let pks: Vec<&PublicKey> = public_keys.iter().map(|p| &p.inner).collect();

    // Try to aggregate signatures
    let agg_sig = match AggregateSignature::aggregate(&sigs, true) {
        Ok(agg) => agg.to_signature(),
        Err(_) => return false,
    };

    // Try to aggregate public keys
    let agg_pk = match AggregatePublicKey::aggregate(&pks, true) {
        Ok(agg) => agg.to_public_key(),
        Err(_) => return false,
    };

    // Verify aggregated signature against aggregated public key
    agg_sig.verify(true, message, DST, &[], &agg_pk, true) == BLST_ERROR::BLST_SUCCESS
}

/// Batch verify multiple BLS signatures (no-op when feature disabled)
#[cfg(not(feature = "bls_batch_verify"))]
pub fn verify_batch(
    message: &[u8],
    signatures: &[&BlsSignature],
    public_keys: &[&BlsPublicKey],
) -> bool {
    // Fall back to individual verification
    if signatures.len() != public_keys.len() || signatures.is_empty() {
        return false;
    }
    signatures
        .iter()
        .zip(public_keys.iter())
        .all(|(sig, pk)| pk.verify(message, sig))
}

/// Verify signatures individually and return indices of valid ones
///
/// Used as fallback after batch verification fails to identify
/// which signatures are invalid (Byzantine detection).
pub fn verify_individually(
    message: &[u8],
    signatures: &[&BlsSignature],
    public_keys: &[&BlsPublicKey],
) -> Vec<usize> {
    if signatures.len() != public_keys.len() {
        return vec![];
    }
    signatures
        .iter()
        .zip(public_keys.iter())
        .enumerate()
        .filter_map(|(i, (sig, pk))| if pk.verify(message, *sig) { Some(i) } else { None })
        .collect()
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
    #[error("batch verification failed")]
    BatchVerificationFailed,
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

    #[test]
    fn test_batch_verify_all_valid() {
        let message = b"batch verify test";

        // Generate multiple key pairs
        let keys: Vec<BlsSecretKey> = (0..5)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();
        let signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

        // Create references
        let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
        let pk_refs: Vec<&BlsPublicKey> = public_keys.iter().collect();

        // Batch verify - should succeed
        assert!(verify_batch(message, &sig_refs, &pk_refs), "All valid signatures should pass batch verify");
    }

    #[test]
    fn test_batch_verify_one_invalid() {
        let message = b"batch verify test";

        // Generate key pairs
        let keys: Vec<BlsSecretKey> = (0..5)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();
        let mut signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

        // Replace one signature with invalid one (sign different message)
        signatures[2] = keys[2].sign(b"different message");

        // Create references
        let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
        let pk_refs: Vec<&BlsPublicKey> = public_keys.iter().collect();

        // Batch verify - should fail
        assert!(!verify_batch(message, &sig_refs, &pk_refs), "Should fail with one invalid signature");

        // Individual verification should identify the bad one
        let valid_indices = verify_individually(message, &sig_refs, &pk_refs);
        assert_eq!(valid_indices.len(), 4, "Should have 4 valid signatures");
        assert!(!valid_indices.contains(&2), "Index 2 should not be valid");
    }

    #[test]
    fn test_verify_individually() {
        let message = b"individual test";

        let keys: Vec<BlsSecretKey> = (0..3)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();

        // Create valid signatures, but one for wrong message
        let mut signatures: Vec<BlsSignature> = vec![];
        signatures.push(keys[0].sign(message));      // Valid
        signatures.push(keys[1].sign(b"wrong msg")); // Invalid
        signatures.push(keys[2].sign(message));      // Valid

        let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
        let pk_refs: Vec<&BlsPublicKey> = public_keys.iter().collect();

        let valid = verify_individually(message, &sig_refs, &pk_refs);
        assert_eq!(valid, vec![0, 2], "Only indices 0 and 2 should be valid");
    }

    #[test]
    fn test_batch_verify_empty() {
        assert!(!verify_batch(b"test", &[], &[]), "Empty should fail");
    }

    #[test]
    fn test_batch_verify_mismatched_lengths() {
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();
        let sig = sk.sign(b"test");

        // More signatures than keys
        assert!(!verify_batch(b"test", &[&sig, &sig], &[&pk]), "Mismatched lengths should fail");
    }
}
