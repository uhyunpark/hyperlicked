//! Tests for BLS batch verification
//!
//! Verifies that:
//! - All valid signatures pass batch verification
//! - One invalid signature causes batch to fail with fallback
//! - Byzantine detection works correctly

use hyperlicked::crypto::bls::{
    verify_batch, verify_individually, BlsSecretKey, BlsSignature,
};

#[test]
fn test_batch_verify_basic() {
    let message = b"consensus vote";

    // Generate 5 key pairs
    let keys: Vec<BlsSecretKey> = (0..5)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = i as u8;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let public_keys: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

    // Create references
    let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
    let pk_refs: Vec<_> = public_keys.iter().collect();

    // Batch verify should succeed
    assert!(
        verify_batch(message, &sig_refs, &pk_refs),
        "All valid signatures should pass batch verification"
    );
}

#[test]
fn test_batch_verify_detects_invalid() {
    let message = b"consensus vote";

    let keys: Vec<BlsSecretKey> = (0..5)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = i as u8;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let public_keys: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let mut signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

    // Corrupt one signature by signing different message
    signatures[2] = keys[2].sign(b"different message");

    let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
    let pk_refs: Vec<_> = public_keys.iter().collect();

    // Batch verify should fail
    assert!(
        !verify_batch(message, &sig_refs, &pk_refs),
        "Should fail with one invalid signature"
    );

    // Individual verification should identify the bad one
    let valid_indices = verify_individually(message, &sig_refs, &pk_refs);
    assert_eq!(valid_indices.len(), 4, "Should have 4 valid signatures");
    assert!(valid_indices.contains(&0));
    assert!(valid_indices.contains(&1));
    assert!(!valid_indices.contains(&2), "Index 2 should not be valid");
    assert!(valid_indices.contains(&3));
    assert!(valid_indices.contains(&4));
}

#[test]
fn test_verify_individually_multiple_invalid() {
    let message = b"test message";

    let keys: Vec<BlsSecretKey> = (0..5)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = i as u8;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let public_keys: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let mut signatures: Vec<BlsSignature> = vec![];

    // Sign different messages for some
    signatures.push(keys[0].sign(message));      // Valid
    signatures.push(keys[1].sign(b"wrong1"));    // Invalid
    signatures.push(keys[2].sign(message));      // Valid
    signatures.push(keys[3].sign(b"wrong2"));    // Invalid
    signatures.push(keys[4].sign(message));      // Valid

    let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
    let pk_refs: Vec<_> = public_keys.iter().collect();

    let valid = verify_individually(message, &sig_refs, &pk_refs);
    assert_eq!(valid, vec![0, 2, 4], "Only indices 0, 2, 4 should be valid");
}

#[test]
fn test_empty_inputs() {
    // Empty arrays should fail gracefully
    assert!(!verify_batch(b"test", &[], &[]), "Empty should fail");
    assert!(verify_individually(b"test", &[], &[]).is_empty(), "Empty should return empty");
}

#[test]
fn test_mismatched_lengths() {
    let sk = BlsSecretKey::generate();
    let pk = sk.public_key();
    let sig = sk.sign(b"test");

    // More signatures than keys
    assert!(!verify_batch(b"test", &[&sig, &sig], &[&pk]), "Mismatched should fail");

    // Should also fail individual
    let result = verify_individually(b"test", &[&sig, &sig], &[&pk]);
    assert!(result.is_empty(), "Mismatched lengths should return empty");
}

#[test]
fn test_large_batch() {
    let message = b"large batch test";

    // Create 100 signers
    let keys: Vec<BlsSecretKey> = (0..100)
        .map(|i| {
            let mut seed = [0u8; 32];
            seed[0] = (i % 256) as u8;
            seed[1] = (i / 256) as u8;
            BlsSecretKey::from_seed(&seed)
        })
        .collect();

    let public_keys: Vec<_> = keys.iter().map(|k| k.public_key()).collect();
    let signatures: Vec<BlsSignature> = keys.iter().map(|k| k.sign(message)).collect();

    let sig_refs: Vec<&BlsSignature> = signatures.iter().collect();
    let pk_refs: Vec<_> = public_keys.iter().collect();

    // Should still work with many signatures
    assert!(
        verify_batch(message, &sig_refs, &pk_refs),
        "Large batch should succeed"
    );
}
