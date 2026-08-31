//! BLS12-381 Signature Aggregation
//!
//! Provides BLS signatures for efficient vote aggregation in consensus.
//! A single aggregated signature proves 2f+1 validators voted for a block.
//!
//! ## Usage
//!
//! ```
//! use hyperlicked::crypto::bls::{
//!     aggregate_signatures, verify_aggregate, BlsSecretKey,
//! };
//!
//! # fn main() -> Result<(), hyperlicked::crypto::bls::BlsError> {
//! let sk1 = BlsSecretKey::from_seed(&[1u8; 32]);
//! let sk2 = BlsSecretKey::from_seed(&[2u8; 32]);
//! let pk1 = sk1.public_key();
//! let pk2 = sk2.public_key();
//! let message = b"vote for block";
//!
//! let sig1 = sk1.sign(message);
//! let sig2 = sk2.sign(message);
//! assert!(pk1.verify(message, &sig1));
//! assert!(pk2.verify(message, &sig2));
//!
//! let aggregate = aggregate_signatures(&[sig1, sig2])?;
//! assert!(verify_aggregate(message, &aggregate, &[pk1, pk2]));
//! # Ok(())
//! # }
//! ```

use blst::min_pk::{AggregatePublicKey, AggregateSignature, PublicKey, SecretKey, Signature};
use blst::BLST_ERROR;

/// Domain separation tag for BLS signatures
const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_HYPERLICKED_";

/// Domain separation tag for proof-of-possession signatures.
///
/// This is intentionally different from [`DST`].  A proof of possession is
/// a registration statement, not a consensus vote, and must never be
/// interchangeable with an ordinary BLS signature.
const POP_DST: &[u8] = b"HYPERLICKED_BLS_POP_V1";

/// Fixed, versioned prefix for the proof-of-possession statement.
///
/// The remaining fields are fixed-width as well, so the signed message has no
/// length ambiguity: prefix || chain domain || node id || compressed pubkey.
const POP_MAGIC_VERSION: &[u8; 22] = b"HYPERLICKED_BLS_POP_V1";
const POP_MESSAGE_LEN: usize = 22 + 32 + 32 + 48;

fn proof_of_possession_message(
    chain_domain: &[u8; 32],
    node_id: &[u8; 32],
    public_key: &[u8; 48],
) -> [u8; POP_MESSAGE_LEN] {
    let mut message = [0u8; POP_MESSAGE_LEN];
    let mut offset = 0;

    message[offset..offset + POP_MAGIC_VERSION.len()].copy_from_slice(POP_MAGIC_VERSION);
    offset += POP_MAGIC_VERSION.len();
    message[offset..offset + chain_domain.len()].copy_from_slice(chain_domain);
    offset += chain_domain.len();
    message[offset..offset + node_id.len()].copy_from_slice(node_id);
    offset += node_id.len();
    message[offset..offset + public_key.len()].copy_from_slice(public_key);

    message
}

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

    /// Create a proof that this key controls the supplied node identity on a
    /// specific chain domain.
    pub fn create_proof_of_possession(
        &self,
        chain_domain: &[u8; 32],
        node_id: &[u8; 32],
    ) -> BlsProofOfPossession {
        let public_key = self.public_key().to_bytes();
        let message = proof_of_possession_message(chain_domain, node_id, &public_key);
        let signature = self.inner.sign(&message, POP_DST, &[]);

        BlsProofOfPossession { inner: signature }
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

    /// Verify a proof of possession for this key, node identity, and chain.
    pub fn verify_proof_of_possession(
        &self,
        chain_domain: &[u8; 32],
        node_id: &[u8; 32],
        proof: &BlsProofOfPossession,
    ) -> bool {
        let public_key = self.to_bytes();
        let message = proof_of_possession_message(chain_domain, node_id, &public_key);

        proof
            .inner
            .verify(true, &message, POP_DST, &[], &self.inner, true)
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

/// BLS proof-of-possession signature (96 bytes compressed G2 point).
#[derive(Clone, Debug)]
pub struct BlsProofOfPossession {
    inner: Signature,
}

impl BlsProofOfPossession {
    /// Serialize to 96 bytes.
    pub fn to_bytes(&self) -> [u8; 96] {
        self.inner.to_bytes()
    }

    /// Deserialize from exactly 96 bytes, preserving blst encoding and
    /// subgroup validation.
    pub fn from_bytes(bytes: &[u8; 96]) -> Result<Self, BlsError> {
        let signature =
            Signature::from_bytes(bytes).map_err(|_| BlsError::InvalidProofOfPossession)?;
        Ok(Self { inner: signature })
    }

    /// Deserialize from a slice.  Any length other than 96 bytes is invalid.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, BlsError> {
        if bytes.len() != 96 {
            return Err(BlsError::InvalidProofOfPossession);
        }

        let mut serialized = [0u8; 96];
        serialized.copy_from_slice(bytes);
        Self::from_bytes(&serialized)
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
    let agg =
        AggregateSignature::aggregate(&sigs, true).map_err(|_| BlsError::AggregationFailed)?;

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
        .filter_map(|(i, (sig, pk))| {
            if pk.verify(message, *sig) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

/// Verify an aggregated BLS signature where ALL signers signed the SAME message
///
/// This is the efficient BLS verification path: aggregate all public keys,
/// then verify the aggregate signature against the aggregate public key.
///
/// **IMPORTANT**: This only works when all signers signed EXACTLY the same message.
/// If messages differ (e.g., each includes voter ID), use `verify_multi_message` instead.
///
/// Returns true if the aggregate signature is valid.
pub fn verify_aggregate_same_message(
    message: &[u8],
    agg_sig: &BlsSignature,
    public_keys: &[BlsPublicKey],
) -> bool {
    if public_keys.is_empty() {
        return false;
    }

    // Aggregate all public keys
    let agg_pk = match aggregate_public_keys(public_keys) {
        Ok(pk) => pk,
        Err(_) => return false,
    };

    // Verify aggregate signature against aggregate public key
    agg_pk.verify(message, agg_sig)
}

/// Verify multiple BLS signatures where each signer signed a DIFFERENT message
///
/// **WARNING**: This function requires individual signatures, not an aggregate.
/// BLS aggregate signatures CANNOT be verified against different messages.
///
/// If you have an aggregate signature and different messages, you must either:
/// 1. Have verified individual signatures BEFORE aggregation, OR
/// 2. Store individual signatures alongside the aggregate
///
/// This function verifies individual (signature, pubkey, message) tuples.
/// Returns a vector of indices that passed verification.
pub fn verify_multi_message(
    signatures: &[BlsSignature],
    public_keys: &[BlsPublicKey],
    messages: &[Vec<u8>],
) -> Vec<usize> {
    if signatures.len() != public_keys.len() || signatures.len() != messages.len() {
        return vec![];
    }

    signatures
        .iter()
        .zip(public_keys.iter())
        .zip(messages.iter())
        .enumerate()
        .filter_map(
            |(i, ((sig, pk), msg))| {
                if pk.verify(msg, sig) {
                    Some(i)
                } else {
                    None
                }
            },
        )
        .collect()
}

/// Batch verify multiple BLS signatures where each signer signs a DIFFERENT message
///
/// **DEPRECATED**: This function cannot properly verify an aggregate signature
/// against different messages. BLS aggregation fundamentally requires the same message.
///
/// For certificate verification:
/// - If all voters signed the same message (view, block_hash, app_hash),
///   use `verify_aggregate_same_message`
/// - If signatures were verified individually before aggregation,
///   the aggregate is already trustworthy
///
/// This function now returns false for safety. Use the appropriate
/// verification function based on your use case.
#[deprecated(
    since = "0.2.0",
    note = "BLS aggregate signatures cannot be verified against different messages. \
            Use verify_aggregate_same_message for same-message verification, or \
            verify individual signatures before aggregation."
)]
pub fn batch_verify(
    _public_keys: &[BlsPublicKey],
    _messages: &[Vec<u8>],
    _agg_sig: &BlsSignature,
) -> bool {
    // BLS aggregate signatures CANNOT be verified against different messages.
    // This function previously returned `true` unconditionally, which was a security bug.
    //
    // If you're hitting this, you need to either:
    // 1. Change your signing protocol so all signers sign the same message
    // 2. Verify individual signatures BEFORE aggregation (see aggregator.rs)
    // 3. Use verify_aggregate_same_message with a common message
    false
}

/// Errors that can occur during BLS operations
#[derive(Debug, Clone, thiserror::Error)]
pub enum BlsError {
    #[error("invalid public key")]
    InvalidPublicKey,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("invalid proof of possession")]
    InvalidProofOfPossession,
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
    fn test_proof_of_possession() {
        let sk = BlsSecretKey::from_seed(&[7u8; 32]);
        let other_sk = BlsSecretKey::from_seed(&[8u8; 32]);
        let pk = sk.public_key();
        let other_pk = other_sk.public_key();
        let chain_domain = [0x11u8; 32];
        let other_chain_domain = [0x22u8; 32];
        let node_id = [0x33u8; 32];
        let other_node_id = [0x44u8; 32];

        let proof = sk.create_proof_of_possession(&chain_domain, &node_id);
        assert!(pk.verify_proof_of_possession(&chain_domain, &node_id, &proof));

        let serialized = proof.to_bytes();
        assert_eq!(serialized.len(), 96);
        let parsed = BlsProofOfPossession::from_bytes(&serialized).unwrap();
        assert!(pk.verify_proof_of_possession(&chain_domain, &node_id, &parsed));

        assert!(!pk.verify_proof_of_possession(&other_chain_domain, &node_id, &parsed));
        assert!(!pk.verify_proof_of_possession(&chain_domain, &other_node_id, &parsed));
        assert!(!other_pk.verify_proof_of_possession(&chain_domain, &node_id, &parsed));

        let mut corrupted = serialized;
        corrupted[0] ^= 0x01;
        let corrupted_result = BlsProofOfPossession::from_bytes(&corrupted);
        assert!(!corrupted_result
            .is_ok_and(|proof| { pk.verify_proof_of_possession(&chain_domain, &node_id, &proof) }));

        assert!(BlsProofOfPossession::from_slice(&serialized[..95]).is_err());
        assert!(BlsProofOfPossession::from_slice(&[0u8; 97]).is_err());
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
    fn test_verify_aggregate_same_message() {
        let message = b"consensus vote for block";

        // Generate multiple key pairs (simulating validators)
        let keys: Vec<BlsSecretKey> = (0..5)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();
        let signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

        // Aggregate signatures
        let agg_sig = aggregate_signatures(&signatures).unwrap();

        // Verify aggregate - should succeed
        assert!(
            verify_aggregate_same_message(message, &agg_sig, &public_keys),
            "Valid aggregate signature should verify"
        );

        // Wrong message should fail
        assert!(
            !verify_aggregate_same_message(b"wrong message", &agg_sig, &public_keys),
            "Wrong message should fail verification"
        );
    }

    #[test]
    fn test_verify_aggregate_same_message_one_bad_sig() {
        let message = b"consensus vote";

        let keys: Vec<BlsSecretKey> = (0..5)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();
        let mut signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

        // Replace one signature with one for different message
        signatures[2] = keys[2].sign(b"different message");

        // Aggregate (including the bad signature)
        let agg_sig = aggregate_signatures(&signatures).unwrap();

        // Verification should FAIL because not all signers signed the same message
        assert!(
            !verify_aggregate_same_message(message, &agg_sig, &public_keys),
            "Aggregate with one bad signature should fail"
        );
    }

    #[test]
    fn test_verify_multi_message() {
        // Each signer signs a DIFFERENT message (like old vote format with voter ID)
        let keys: Vec<BlsSecretKey> = (0..3)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();

        // Each signer signs their own unique message
        let messages: Vec<Vec<u8>> = (0..3)
            .map(|i| format!("message for voter {}", i).into_bytes())
            .collect();

        let signatures: Vec<BlsSignature> = keys
            .iter()
            .zip(messages.iter())
            .map(|(k, m)| k.sign(m))
            .collect();

        // Verify all signatures individually
        let valid = verify_multi_message(&signatures, &public_keys, &messages);
        assert_eq!(valid, vec![0, 1, 2], "All signatures should be valid");
    }

    #[test]
    fn test_verify_multi_message_one_invalid() {
        let keys: Vec<BlsSecretKey> = (0..3)
            .map(|i| {
                let mut seed = [0u8; 32];
                seed[0] = i as u8;
                BlsSecretKey::from_seed(&seed)
            })
            .collect();

        let public_keys: Vec<BlsPublicKey> = keys.iter().map(|k| k.public_key()).collect();

        let messages: Vec<Vec<u8>> = (0..3)
            .map(|i| format!("message for voter {}", i).into_bytes())
            .collect();

        // Sign correctly except one signs the wrong message
        let mut signatures: Vec<BlsSignature> = vec![];
        signatures.push(keys[0].sign(&messages[0])); // Valid
        signatures.push(keys[1].sign(b"WRONG MESSAGE")); // Invalid
        signatures.push(keys[2].sign(&messages[2])); // Valid

        let valid = verify_multi_message(&signatures, &public_keys, &messages);
        assert_eq!(valid, vec![0, 2], "Only indices 0 and 2 should be valid");
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
        signatures.push(keys[0].sign(message)); // Valid
        signatures.push(keys[1].sign(b"wrong msg")); // Invalid
        signatures.push(keys[2].sign(message)); // Valid

        let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
        let pk_refs: Vec<&BlsPublicKey> = public_keys.iter().collect();

        let valid = verify_individually(message, &sig_refs, &pk_refs);
        assert_eq!(valid, vec![0, 2], "Only indices 0 and 2 should be valid");
    }

    #[test]
    #[allow(deprecated)]
    fn test_deprecated_batch_verify_returns_false() {
        // The old batch_verify with different messages is deprecated and now returns false
        let sk = BlsSecretKey::generate();
        let pk = sk.public_key();
        let sig = sk.sign(b"test");

        // This should now return false (was a security bug that returned true)
        assert!(
            !batch_verify(&[pk], &[b"test".to_vec()], &sig),
            "Deprecated batch_verify should return false"
        );
    }

    #[test]
    fn test_verify_aggregate_empty_pubkeys() {
        // Create a valid signature first
        let sk = BlsSecretKey::generate();
        let sig = sk.sign(b"test");

        // Empty pubkeys should fail verification
        assert!(
            !verify_aggregate_same_message(b"test", &sig, &[]),
            "Empty pubkeys should fail"
        );
    }

    #[test]
    fn test_verify_aggregate_wrong_pubkey() {
        // Sign with one key, try to verify with different key
        let sk1 = BlsSecretKey::generate();
        let sk2 = BlsSecretKey::generate();

        let sig = sk1.sign(b"test");
        let wrong_pk = sk2.public_key();

        assert!(
            !verify_aggregate_same_message(b"test", &sig, &[wrong_pk]),
            "Wrong pubkey should fail"
        );
    }
}
