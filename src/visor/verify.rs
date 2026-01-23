//! Binary signature verification
//!
//! Uses Ed25519 signatures for simplicity and no external dependencies.
//! For production, this can be enhanced to support GPG via external process.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::io::Read;
use std::path::Path;
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("failed to read file: {0}")]
    IoError(#[from] std::io::Error),
    #[error("failed to parse public key: {0}")]
    KeyParseError(String),
    #[error("failed to parse signature: {0}")]
    SignatureParseError(String),
    #[error("signature verification failed")]
    VerificationFailed,
    #[error("invalid signature length")]
    InvalidSignatureLength,
    #[error("invalid public key length")]
    InvalidKeyLength,
    #[error("public key not found")]
    KeyNotFound,
}

/// Result of signature verification
#[derive(Debug)]
pub struct VerifyResult {
    /// Whether verification succeeded
    pub valid: bool,
    /// Key fingerprint (hex of first 8 bytes)
    pub key_fingerprint: Option<String>,
}

/// Verify a binary against a detached Ed25519 signature
///
/// # Arguments
///
/// * `binary_path` - Path to the binary file
/// * `signature_path` - Path to the detached signature (.sig file, 64 bytes raw)
/// * `pubkey_path` - Path to the public key file (32 bytes raw or 64 bytes hex)
pub fn verify_binary(
    binary_path: &Path,
    signature_path: &Path,
    pubkey_path: &Path,
) -> Result<VerifyResult, VerifyError> {
    info!(
        "Verifying {} with signature {}",
        binary_path.display(),
        signature_path.display()
    );

    // Load the public key
    let pubkey_data = std::fs::read(pubkey_path)?;
    let pubkey = parse_public_key(&pubkey_data)?;

    debug!("Loaded public key: {}", hex::encode(pubkey.as_bytes()));

    // Load the signature
    let sig_data = std::fs::read(signature_path)?;
    let signature = parse_signature(&sig_data)?;

    // Load and hash the binary
    let binary_data = std::fs::read(binary_path)?;

    // Verify
    match pubkey.verify(&binary_data, &signature) {
        Ok(()) => {
            info!("Signature verification successful");
            Ok(VerifyResult {
                valid: true,
                key_fingerprint: Some(hex::encode(&pubkey.as_bytes()[..8])),
            })
        }
        Err(_) => {
            warn!("Signature verification failed");
            Err(VerifyError::VerificationFailed)
        }
    }
}

/// Verify a binary using raw bytes for the public key
pub fn verify_binary_with_key(
    binary_path: &Path,
    signature_path: &Path,
    pubkey_bytes: &[u8],
) -> Result<VerifyResult, VerifyError> {
    let pubkey = parse_public_key(pubkey_bytes)?;
    let sig_data = std::fs::read(signature_path)?;
    let signature = parse_signature(&sig_data)?;
    let binary_data = std::fs::read(binary_path)?;

    match pubkey.verify(&binary_data, &signature) {
        Ok(()) => Ok(VerifyResult {
            valid: true,
            key_fingerprint: Some(hex::encode(&pubkey.as_bytes()[..8])),
        }),
        Err(_) => Err(VerifyError::VerificationFailed),
    }
}

/// Parse public key from bytes (raw 32 bytes or hex-encoded)
fn parse_public_key(data: &[u8]) -> Result<VerifyingKey, VerifyError> {
    // Try as raw 32 bytes first
    if data.len() == 32 {
        let key_bytes: [u8; 32] = data
            .try_into()
            .map_err(|_| VerifyError::InvalidKeyLength)?;
        return VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| VerifyError::KeyParseError(e.to_string()));
    }

    // Try as hex-encoded (64 chars)
    let hex_str = String::from_utf8_lossy(data);
    let hex_str = hex_str.trim();

    if hex_str.len() == 64 {
        let key_bytes = hex::decode(hex_str)
            .map_err(|e| VerifyError::KeyParseError(e.to_string()))?;
        let key_array: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| VerifyError::InvalidKeyLength)?;
        return VerifyingKey::from_bytes(&key_array)
            .map_err(|e| VerifyError::KeyParseError(e.to_string()));
    }

    Err(VerifyError::InvalidKeyLength)
}

/// Parse signature from bytes (raw 64 bytes or hex-encoded)
fn parse_signature(data: &[u8]) -> Result<Signature, VerifyError> {
    // Try as raw 64 bytes first
    if data.len() == 64 {
        let sig_bytes: [u8; 64] = data
            .try_into()
            .map_err(|_| VerifyError::InvalidSignatureLength)?;
        return Ok(Signature::from_bytes(&sig_bytes));
    }

    // Try as hex-encoded (128 chars)
    let hex_str = String::from_utf8_lossy(data);
    let hex_str = hex_str.trim();

    if hex_str.len() == 128 {
        let sig_bytes = hex::decode(hex_str)
            .map_err(|e| VerifyError::SignatureParseError(e.to_string()))?;
        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| VerifyError::InvalidSignatureLength)?;
        return Ok(Signature::from_bytes(&sig_array));
    }

    Err(VerifyError::InvalidSignatureLength)
}

/// Compute SHA256 checksum of a file
pub fn sha256_checksum(path: &Path) -> Result<String, std::io::Error> {
    use sha2::{Digest, Sha256};

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Verify SHA256 checksum
pub fn verify_checksum(path: &Path, expected: &str) -> Result<bool, std::io::Error> {
    let actual = sha256_checksum(path)?;

    // Handle "sha256:" prefix
    let expected_hash = expected
        .strip_prefix("sha256:")
        .unwrap_or(expected)
        .to_lowercase();

    Ok(actual.to_lowercase() == expected_hash)
}

/// Sign a file using Ed25519 (for testing/key generation)
#[cfg(test)]
pub fn sign_file(
    file_path: &Path,
    signing_key: &ed25519_dalek::SigningKey,
) -> Result<Vec<u8>, std::io::Error> {
    use ed25519_dalek::Signer;
    let data = std::fs::read(file_path)?;
    let signature = signing_key.sign(&data);
    Ok(signature.to_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    use tempfile::TempDir;

    #[test]
    fn test_sha256_checksum() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "hello world\n").unwrap();

        let checksum = sha256_checksum(&test_file).unwrap();
        assert_eq!(checksum.len(), 64);
    }

    #[test]
    fn test_verify_checksum() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "test").unwrap();

        let checksum = sha256_checksum(&test_file).unwrap();

        assert!(verify_checksum(&test_file, &checksum).unwrap());
        assert!(verify_checksum(&test_file, &format!("sha256:{}", checksum)).unwrap());
        assert!(!verify_checksum(&test_file, "invalid").unwrap());
    }

    #[test]
    fn test_ed25519_sign_verify() {
        let temp_dir = TempDir::new().unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test_binary");
        std::fs::write(&test_file, b"test binary content").unwrap();

        // Generate key pair
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        // Sign the file
        let signature = sign_file(&test_file, &signing_key).unwrap();

        // Write signature and public key
        let sig_file = temp_dir.path().join("test_binary.sig");
        let key_file = temp_dir.path().join("pubkey");
        std::fs::write(&sig_file, &signature).unwrap();
        std::fs::write(&key_file, verifying_key.as_bytes()).unwrap();

        // Verify
        let result = verify_binary(&test_file, &sig_file, &key_file).unwrap();
        assert!(result.valid);
    }

    #[test]
    fn test_hex_encoded_key_and_sig() {
        let temp_dir = TempDir::new().unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test_binary");
        std::fs::write(&test_file, b"test binary content").unwrap();

        // Generate key pair
        let signing_key = SigningKey::generate(&mut OsRng);
        let verifying_key = signing_key.verifying_key();

        // Sign the file
        let signature = sign_file(&test_file, &signing_key).unwrap();

        // Write hex-encoded signature and public key
        let sig_file = temp_dir.path().join("test_binary.sig");
        let key_file = temp_dir.path().join("pubkey");
        std::fs::write(&sig_file, hex::encode(&signature)).unwrap();
        std::fs::write(&key_file, hex::encode(verifying_key.as_bytes())).unwrap();

        // Verify
        let result = verify_binary(&test_file, &sig_file, &key_file).unwrap();
        assert!(result.valid);
    }
}
