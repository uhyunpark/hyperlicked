//! Authenticated Handshake Protocol
//!
//! Provides cryptographic authentication for TCP connections using BLS signatures.
//! This prevents impersonation attacks where a malicious node claims to be another validator.
//!
//! ## Protocol
//!
//! 1. Both sides exchange node IDs
//! 2. Both sides exchange random 32-byte challenges
//! 3. Both sides sign the combined challenge (their_nonce || our_nonce) with BLS
//! 4. Both sides verify the signature against the expected public key
//!
//! ## Security Properties
//!
//! - Prevents impersonation: Attacker cannot sign without the BLS private key
//! - Prevents replay: Random nonces ensure unique handshake per connection
//! - Mutual authentication: Both sides prove identity

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;
use tracing::{debug, warn};

/// Handshake timeout (10 seconds)
/// Shorter than normal read timeout since handshakes should be fast
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

use crate::crypto::bls::{BlsPublicKey, BlsSecretKey, BlsSignature};
use crate::types::{hash_short, NodeId};

/// Handshake configuration
#[derive(Clone)]
pub struct HandshakeConfig {
    /// Our node ID
    pub node_id: NodeId,
    /// Our BLS secret key for signing challenges
    pub bls_sk: Option<BlsSecretKey>,
    /// Known validator public keys (for verification)
    pub validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    /// Whether to require authentication (can be disabled for dev mode)
    pub require_auth: bool,
}

impl HandshakeConfig {
    /// Create config without authentication (dev mode)
    pub fn unauthenticated(node_id: NodeId) -> Self {
        Self {
            node_id,
            bls_sk: None,
            validator_pubkeys: HashMap::new(),
            require_auth: false,
        }
    }

    /// Create config with authentication
    pub fn authenticated(
        node_id: NodeId,
        bls_sk: BlsSecretKey,
        validator_pubkeys: HashMap<NodeId, BlsPublicKey>,
    ) -> Self {
        Self {
            node_id,
            bls_sk: Some(bls_sk),
            validator_pubkeys,
            require_auth: true,
        }
    }
}

/// Result of a successful handshake
pub struct HandshakeResult {
    /// The authenticated peer's node ID
    pub peer_id: NodeId,
    /// Whether authentication was performed
    pub authenticated: bool,
}

/// Handshake message format:
/// - 32 bytes: node ID
/// - 32 bytes: random challenge nonce
/// - 96 bytes: BLS signature (or zeros if unauthenticated)
const HANDSHAKE_SIZE: usize = 32 + 32 + 96;

/// Perform authenticated handshake as the connection initiator (client side)
pub async fn handshake_outbound(
    read_half: &mut OwnedReadHalf,
    write_half: &mut OwnedWriteHalf,
    config: &HandshakeConfig,
    expected_peer_id: &NodeId,
) -> Result<HandshakeResult> {
    // Generate our challenge nonce
    let our_nonce = generate_nonce();

    // Send our handshake message
    let our_msg = create_handshake_message(&config.node_id, &our_nonce, config, &[0u8; 32])?;
    write_half.write_all(&our_msg).await?;

    // Receive their handshake message with timeout
    let mut their_msg = [0u8; HANDSHAKE_SIZE];
    timeout(HANDSHAKE_TIMEOUT, read_half.read_exact(&mut their_msg))
        .await
        .map_err(|_| anyhow!("Handshake timeout waiting for peer response"))?
        .map_err(|e| anyhow!("Handshake read error: {}", e))?;

    let (peer_id, their_nonce, their_sig) = parse_handshake_message(&their_msg)?;

    // Verify peer ID matches expected
    if peer_id != *expected_peer_id {
        return Err(anyhow!(
            "Peer ID mismatch: expected {}, got {}",
            hash_short(expected_peer_id),
            hash_short(&peer_id)
        ));
    }

    // Now send our signature over their nonce (prove our identity)
    let challenge = create_challenge(&their_nonce, &our_nonce);
    let our_sig = sign_challenge(&challenge, config)?;
    write_half.write_all(&our_sig).await?;

    // Verify their signature over our nonce (verify their identity)
    let their_challenge = create_challenge(&our_nonce, &their_nonce);
    let authenticated = verify_signature(config, &peer_id, &their_challenge, &their_sig)?;

    debug!(
        peer = %hash_short(&peer_id),
        authenticated,
        "Outbound handshake complete"
    );

    Ok(HandshakeResult {
        peer_id,
        authenticated,
    })
}

/// Perform authenticated handshake as the connection acceptor (server side)
pub async fn handshake_inbound(
    read_half: &mut OwnedReadHalf,
    write_half: &mut OwnedWriteHalf,
    config: &HandshakeConfig,
) -> Result<HandshakeResult> {
    // Receive their handshake message with timeout
    let mut their_msg = [0u8; HANDSHAKE_SIZE];
    timeout(HANDSHAKE_TIMEOUT, read_half.read_exact(&mut their_msg))
        .await
        .map_err(|_| anyhow!("Handshake timeout waiting for initial message"))?
        .map_err(|e| anyhow!("Handshake read error: {}", e))?;

    let (peer_id, their_nonce, _their_initial_sig) = parse_handshake_message(&their_msg)?;

    // Generate our challenge nonce
    let our_nonce = generate_nonce();

    // Sign challenge for their verification
    let challenge = create_challenge(&their_nonce, &our_nonce);
    let our_sig = sign_challenge(&challenge, config)?;

    // Create and send our handshake message with signature
    let mut our_msg = [0u8; HANDSHAKE_SIZE];
    our_msg[..32].copy_from_slice(&config.node_id);
    our_msg[32..64].copy_from_slice(&our_nonce);
    our_msg[64..].copy_from_slice(&our_sig);
    write_half.write_all(&our_msg).await?;

    // Receive their signature (proving their identity) with timeout
    let mut their_sig = [0u8; 96];
    timeout(HANDSHAKE_TIMEOUT, read_half.read_exact(&mut their_sig))
        .await
        .map_err(|_| anyhow!("Handshake timeout waiting for signature"))?
        .map_err(|e| anyhow!("Handshake read error: {}", e))?;

    // Verify their signature
    let their_challenge = create_challenge(&our_nonce, &their_nonce);
    let authenticated = verify_signature(config, &peer_id, &their_challenge, &their_sig)?;

    debug!(
        peer = %hash_short(&peer_id),
        authenticated,
        "Inbound handshake complete"
    );

    Ok(HandshakeResult {
        peer_id,
        authenticated,
    })
}

/// Generate a random 32-byte nonce
fn generate_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce).expect("Failed to generate random nonce");
    nonce
}

/// Create the challenge bytes from two nonces
fn create_challenge(nonce_a: &[u8; 32], nonce_b: &[u8; 32]) -> [u8; 64] {
    let mut challenge = [0u8; 64];
    challenge[..32].copy_from_slice(nonce_a);
    challenge[32..].copy_from_slice(nonce_b);
    challenge
}

/// Create handshake message
fn create_handshake_message(
    node_id: &NodeId,
    nonce: &[u8; 32],
    config: &HandshakeConfig,
    _their_nonce: &[u8; 32], // Not used for initial message
) -> Result<[u8; HANDSHAKE_SIZE]> {
    let mut msg = [0u8; HANDSHAKE_SIZE];
    msg[..32].copy_from_slice(node_id);
    msg[32..64].copy_from_slice(nonce);
    // Signature is zeros for initial message (we don't know their nonce yet)

    // If we're not doing auth, leave signature as zeros
    if !config.require_auth || config.bls_sk.is_none() {
        return Ok(msg);
    }

    Ok(msg)
}

/// Parse handshake message
fn parse_handshake_message(msg: &[u8; HANDSHAKE_SIZE]) -> Result<(NodeId, [u8; 32], [u8; 96])> {
    let mut node_id = [0u8; 32];
    let mut nonce = [0u8; 32];
    let mut sig = [0u8; 96];

    node_id.copy_from_slice(&msg[..32]);
    nonce.copy_from_slice(&msg[32..64]);
    sig.copy_from_slice(&msg[64..]);

    Ok((node_id, nonce, sig))
}

/// Sign challenge with BLS key
fn sign_challenge(challenge: &[u8; 64], config: &HandshakeConfig) -> Result<[u8; 96]> {
    if let Some(ref bls_sk) = config.bls_sk {
        let sig = bls_sk.sign(challenge);
        Ok(sig.to_bytes())
    } else {
        // No BLS key - return zeros (unauthenticated mode)
        Ok([0u8; 96])
    }
}

/// Verify signature against known public key
fn verify_signature(
    config: &HandshakeConfig,
    peer_id: &NodeId,
    challenge: &[u8; 64],
    sig_bytes: &[u8; 96],
) -> Result<bool> {
    // If authentication is not required, accept
    if !config.require_auth {
        return Ok(false);
    }

    // If signature is all zeros, it's unauthenticated
    if sig_bytes.iter().all(|&b| b == 0) {
        if config.require_auth {
            warn!(
                peer = %hash_short(peer_id),
                "Peer did not provide authentication signature"
            );
            return Err(anyhow!("Authentication required but peer sent empty signature"));
        }
        return Ok(false);
    }

    // Look up the peer's public key
    let pubkey = config.validator_pubkeys.get(peer_id).ok_or_else(|| {
        warn!(
            peer = %hash_short(peer_id),
            "Unknown validator - no public key registered"
        );
        anyhow!("Unknown validator: {}", hash_short(peer_id))
    })?;

    // Parse and verify signature
    let sig = BlsSignature::from_slice(sig_bytes).map_err(|e| {
        warn!(peer = %hash_short(peer_id), "Invalid BLS signature format");
        anyhow!("Invalid signature format: {}", e)
    })?;

    if !pubkey.verify(challenge, &sig) {
        warn!(
            peer = %hash_short(peer_id),
            "BLS signature verification FAILED - POTENTIAL IMPERSONATION ATTACK"
        );
        return Err(anyhow!(
            "Signature verification failed - potential impersonation attack from {}",
            hash_short(peer_id)
        ));
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_challenge_creation() {
        let nonce_a = [1u8; 32];
        let nonce_b = [2u8; 32];

        let challenge = create_challenge(&nonce_a, &nonce_b);
        assert_eq!(&challenge[..32], &nonce_a);
        assert_eq!(&challenge[32..], &nonce_b);
    }

    #[test]
    fn test_nonce_generation_is_random() {
        let nonce1 = generate_nonce();
        let nonce2 = generate_nonce();
        assert_ne!(nonce1, nonce2);
    }

    #[test]
    fn test_unauthenticated_config() {
        let config = HandshakeConfig::unauthenticated(test_node_id(1));
        assert!(!config.require_auth);
        assert!(config.bls_sk.is_none());
    }

    #[test]
    fn test_authenticated_config() {
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);
        let bls_pk = bls_sk.public_key();
        let mut pubkeys = HashMap::new();
        pubkeys.insert(test_node_id(2), bls_pk);

        let config = HandshakeConfig::authenticated(test_node_id(1), bls_sk, pubkeys);
        assert!(config.require_auth);
        assert!(config.bls_sk.is_some());
    }

    #[test]
    fn test_sign_and_verify_challenge() {
        let bls_sk = BlsSecretKey::from_seed(&[42u8; 32]);
        let bls_pk = bls_sk.public_key();

        let mut pubkeys = HashMap::new();
        pubkeys.insert(test_node_id(1), bls_pk);

        let config = HandshakeConfig::authenticated(test_node_id(1), bls_sk, pubkeys.clone());

        let challenge = [0x42u8; 64];
        let sig = sign_challenge(&challenge, &config).unwrap();

        // Create a verifying config
        let verify_config = HandshakeConfig {
            node_id: test_node_id(2),
            bls_sk: None,
            validator_pubkeys: pubkeys,
            require_auth: true,
        };

        let result = verify_signature(&verify_config, &test_node_id(1), &challenge, &sig);
        assert!(result.is_ok());
        assert!(result.unwrap()); // Should be authenticated
    }
}
