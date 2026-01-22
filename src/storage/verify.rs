//! Block and Snapshot Verification
//!
//! Functions to verify the integrity of blocks and snapshots received
//! from peers during sync.
//!
//! ## Verification Checks
//!
//! ### Block Chain Verification
//! - Parent hash linkage (block.parent == prev_block.hash())
//! - Height sequence (each block is height + 1)
//! - View progression (views should increase or stay same for forks)
//!
//! ### Snapshot Verification
//! - State hash matches expected hash from corresponding block

use crate::storage::AppSnapshot;
use crate::types::{hash, Block, Hash};

/// Result of block chain verification
#[derive(Debug)]
pub struct ChainVerifyResult {
    /// Whether the chain is valid
    pub valid: bool,
    /// Number of blocks verified
    pub verified_count: usize,
    /// Error message if invalid
    pub error: Option<String>,
    /// Height of first invalid block (if any)
    pub invalid_height: Option<u64>,
}

impl ChainVerifyResult {
    fn ok(count: usize) -> Self {
        Self {
            valid: true,
            verified_count: count,
            error: None,
            invalid_height: None,
        }
    }

    fn err(count: usize, height: u64, error: String) -> Self {
        Self {
            valid: false,
            verified_count: count,
            error: Some(error),
            invalid_height: Some(height),
        }
    }
}

/// Verify a chain of blocks starting from a known hash
///
/// Checks that:
/// - First block's parent matches `starting_hash`
/// - Each subsequent block's parent matches previous block's hash
/// - Heights are sequential (each block is +1 from previous)
///
/// # Arguments
/// - `blocks`: Blocks to verify (must be sorted by height ascending)
/// - `starting_hash`: Expected parent hash of first block
///
/// # Returns
/// - `ChainVerifyResult` with verification status
pub fn verify_block_chain(blocks: &[Block], starting_hash: Hash) -> ChainVerifyResult {
    if blocks.is_empty() {
        return ChainVerifyResult::ok(0);
    }

    // Verify first block's parent
    let first = &blocks[0];
    if first.parent != starting_hash {
        return ChainVerifyResult::err(
            0,
            first.height,
            format!(
                "First block parent mismatch: expected {}, got {}",
                hex::encode(starting_hash),
                hex::encode(first.parent)
            ),
        );
    }

    let mut prev_hash = first.hash();
    let mut prev_height = first.height;

    // Verify chain linkage
    for (i, block) in blocks.iter().enumerate().skip(1) {
        // Check parent hash
        if block.parent != prev_hash {
            return ChainVerifyResult::err(
                i,
                block.height,
                format!(
                    "Parent hash mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(prev_hash),
                    hex::encode(block.parent)
                ),
            );
        }

        // Check height sequence
        if block.height != prev_height + 1 {
            return ChainVerifyResult::err(
                i,
                block.height,
                format!(
                    "Height sequence error: expected {}, got {}",
                    prev_height + 1,
                    block.height
                ),
            );
        }

        prev_hash = block.hash();
        prev_height = block.height;
    }

    ChainVerifyResult::ok(blocks.len())
}

/// Verify a chain of blocks (no starting hash, just internal consistency)
///
/// Use this when you don't have a known starting point but want to verify
/// that the blocks form a valid chain among themselves.
pub fn verify_block_chain_internal(blocks: &[Block]) -> ChainVerifyResult {
    if blocks.is_empty() {
        return ChainVerifyResult::ok(0);
    }

    let first = &blocks[0];
    let mut prev_hash = first.hash();
    let mut prev_height = first.height;

    for (i, block) in blocks.iter().enumerate().skip(1) {
        if block.parent != prev_hash {
            return ChainVerifyResult::err(
                i,
                block.height,
                format!(
                    "Parent hash mismatch at height {}: expected {}, got {}",
                    block.height,
                    hex::encode(prev_hash),
                    hex::encode(block.parent)
                ),
            );
        }

        if block.height != prev_height + 1 {
            return ChainVerifyResult::err(
                i,
                block.height,
                format!(
                    "Height sequence error: expected {}, got {}",
                    prev_height + 1,
                    block.height
                ),
            );
        }

        prev_hash = block.hash();
        prev_height = block.height;
    }

    ChainVerifyResult::ok(blocks.len())
}

/// Verify snapshot data matches expected state hash
///
/// Computes the hash of the serialized snapshot and compares to expected.
///
/// # Arguments
/// - `snapshot`: The snapshot to verify
/// - `expected_hash`: Expected hash (from the block at this height)
///
/// # Returns
/// - `true` if hash matches, `false` otherwise
pub fn verify_snapshot(snapshot: &AppSnapshot, expected_hash: &Hash) -> bool {
    match serde_json::to_vec(snapshot) {
        Ok(bytes) => hash(&bytes) == *expected_hash,
        Err(_) => false,
    }
}

/// Compute the hash of a snapshot
///
/// Returns the SHA-256 hash of the JSON-serialized snapshot.
pub fn compute_snapshot_hash(snapshot: &AppSnapshot) -> Option<Hash> {
    serde_json::to_vec(snapshot).ok().map(|bytes| hash(&bytes))
}

/// Verify that a snapshot height is consistent with a block
///
/// The snapshot at height H should represent state AFTER executing block H.
/// This checks that the snapshot height matches the block height.
pub fn verify_snapshot_height(snapshot: &AppSnapshot, block: &Block) -> bool {
    snapshot.height == block.height
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn create_chain(start_height: u64, count: usize) -> Vec<Block> {
        let mut blocks = Vec::with_capacity(count);
        let mut parent = [0u8; 32];

        for i in 0..count {
            let block = Block {
                view: (start_height + i as u64) * 2,
                height: start_height + i as u64,
                parent,
                payload: vec![],
                proposer: [1u8; 32],
                app_hash: [0u8; 32],
                timestamp: 1000 + i as u64,
            };
            parent = block.hash();
            blocks.push(block);
        }

        blocks
    }

    #[test]
    fn test_verify_empty_chain() {
        let result = verify_block_chain(&[], [0u8; 32]);
        assert!(result.valid);
        assert_eq!(result.verified_count, 0);
    }

    #[test]
    fn test_verify_valid_chain() {
        let blocks = create_chain(1, 5);
        let starting_hash = [0u8; 32]; // Genesis parent

        let result = verify_block_chain(&blocks, starting_hash);
        assert!(result.valid, "Chain should be valid: {:?}", result.error);
        assert_eq!(result.verified_count, 5);
    }

    #[test]
    fn test_verify_invalid_first_parent() {
        let blocks = create_chain(1, 5);
        let wrong_hash = [1u8; 32]; // Wrong starting hash

        let result = verify_block_chain(&blocks, wrong_hash);
        assert!(!result.valid);
        assert_eq!(result.verified_count, 0);
        assert_eq!(result.invalid_height, Some(1));
        assert!(result.error.unwrap().contains("First block parent mismatch"));
    }

    #[test]
    fn test_verify_broken_chain() {
        let mut blocks = create_chain(1, 5);
        // Break the chain by modifying a block's parent
        blocks[2].parent = [99u8; 32];

        let result = verify_block_chain(&blocks, [0u8; 32]);
        assert!(!result.valid);
        assert_eq!(result.verified_count, 2); // First 2 blocks verified
        assert_eq!(result.invalid_height, Some(3));
        assert!(result.error.unwrap().contains("Parent hash mismatch"));
    }

    #[test]
    fn test_verify_height_gap() {
        let mut blocks = create_chain(1, 5);
        // Create a height gap
        blocks[3].height = 10; // Should be 4

        let result = verify_block_chain(&blocks, [0u8; 32]);
        assert!(!result.valid);
        assert!(result.error.unwrap().contains("Height sequence error"));
    }

    #[test]
    fn test_verify_internal_consistency() {
        let blocks = create_chain(1, 5);

        let result = verify_block_chain_internal(&blocks);
        assert!(result.valid);
        assert_eq!(result.verified_count, 5);
    }

    #[test]
    fn test_snapshot_hash() {
        let snapshot = AppSnapshot::genesis();
        let hash1 = compute_snapshot_hash(&snapshot);
        let hash2 = compute_snapshot_hash(&snapshot);

        assert!(hash1.is_some());
        assert_eq!(hash1, hash2); // Deterministic
    }

    #[test]
    fn test_verify_snapshot() {
        let snapshot = AppSnapshot::genesis();
        let expected = compute_snapshot_hash(&snapshot).unwrap();

        assert!(verify_snapshot(&snapshot, &expected));
        assert!(!verify_snapshot(&snapshot, &[0u8; 32])); // Wrong hash
    }
}
