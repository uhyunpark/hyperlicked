//! ECDSA Signing Utilities
//!
//! Provides key generation, signing, and address recovery.

use alloy_primitives::{keccak256, Address, B256};
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::rand_core::OsRng;

/// ECDSA signer with private key
pub struct Signer {
    signing_key: SigningKey,
}

impl Signer {
    /// Generate a new random key
    pub fn generate() -> Self {
        let signing_key = SigningKey::random(&mut OsRng);
        Self { signing_key }
    }

    /// Create signer from private key bytes
    pub fn from_bytes(bytes: &[u8; 32]) -> Result<Self, SignerError> {
        let signing_key = SigningKey::from_bytes(bytes.into())
            .map_err(|_| SignerError::InvalidKey)?;
        Ok(Self { signing_key })
    }

    /// Create signer from hex string (with or without 0x prefix)
    pub fn from_hex(hex_str: &str) -> Result<Self, SignerError> {
        let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
        let bytes = hex::decode(hex_str).map_err(|_| SignerError::InvalidHex)?;
        if bytes.len() != 32 {
            return Err(SignerError::InvalidKey);
        }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&bytes);
        Self::from_bytes(&key_bytes)
    }

    /// Get the Ethereum address for this signer
    pub fn address(&self) -> Address {
        let verifying_key = self.signing_key.verifying_key();
        public_key_to_address(verifying_key)
    }

    /// Get the private key as bytes
    pub fn private_key(&self) -> [u8; 32] {
        self.signing_key.to_bytes().into()
    }

    /// Get the private key as hex string
    pub fn private_key_hex(&self) -> String {
        format!("0x{}", hex::encode(self.private_key()))
    }

    /// Sign a 32-byte message hash
    /// Returns 65-byte signature (r || s || v)
    pub fn sign(&self, message_hash: &[u8; 32]) -> [u8; 65] {
        let (signature, recovery_id) = self.signing_key
            .sign_prehash_recoverable(message_hash)
            .expect("signing should not fail");

        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&signature.to_bytes());
        sig_bytes[64] = recovery_id.to_byte() + 27; // Ethereum uses 27/28

        sig_bytes
    }

    /// Sign a message with Ethereum prefix
    /// Adds "\x19Ethereum Signed Message:\n" prefix
    pub fn sign_message(&self, message: &[u8]) -> [u8; 65] {
        let hash = hash_message(message);
        self.sign(&hash)
    }
}

/// Recover address from signature and message hash
pub fn recover_address(message_hash: &[u8; 32], signature: &[u8]) -> Result<Address, SignerError> {
    if signature.len() != 65 {
        return Err(SignerError::InvalidSignature);
    }

    let sig = Signature::try_from(&signature[..64])
        .map_err(|_| SignerError::InvalidSignature)?;

    let v = signature[64];
    let recovery_id = if v >= 27 {
        RecoveryId::try_from(v - 27).map_err(|_| SignerError::InvalidSignature)?
    } else {
        RecoveryId::try_from(v).map_err(|_| SignerError::InvalidSignature)?
    };

    let verifying_key = VerifyingKey::recover_from_prehash(message_hash, &sig, recovery_id)
        .map_err(|_| SignerError::RecoveryFailed)?;

    Ok(public_key_to_address(&verifying_key))
}

/// Hash a message with Ethereum prefix
pub fn hash_message(message: &[u8]) -> [u8; 32] {
    let prefix = format!("\x19Ethereum Signed Message:\n{}", message.len());
    let mut data = prefix.into_bytes();
    data.extend_from_slice(message);
    keccak256(&data).into()
}

/// Convert public key to Ethereum address
fn public_key_to_address(verifying_key: &VerifyingKey) -> Address {
    let public_key = verifying_key.to_encoded_point(false);
    let public_key_bytes = &public_key.as_bytes()[1..]; // Skip 0x04 prefix
    let hash = keccak256(public_key_bytes);
    Address::from_slice(&hash[12..])
}

/// Signer errors
#[derive(Debug, Clone, thiserror::Error)]
pub enum SignerError {
    #[error("invalid private key")]
    InvalidKey,
    #[error("invalid hex string")]
    InvalidHex,
    #[error("invalid signature")]
    InvalidSignature,
    #[error("recovery failed")]
    RecoveryFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_and_sign() {
        let signer = Signer::generate();
        let message = b"hello world";
        let hash = keccak256(message);
        let signature = signer.sign(&hash.into());

        assert_eq!(signature.len(), 65);

        // Recover address
        let recovered = recover_address(&hash.into(), &signature).unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn test_from_hex() {
        let key_hex = "0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let signer = Signer::from_hex(key_hex).unwrap();
        assert!(!signer.address().is_zero());
    }

    #[test]
    fn test_address_derivation() {
        // Known test vector
        let key_hex = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let signer = Signer::from_hex(key_hex).unwrap();
        // This is the first Anvil/Hardhat account
        assert_eq!(
            signer.address().to_string().to_lowercase(),
            "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"
        );
    }
}
