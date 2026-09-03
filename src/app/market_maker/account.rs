//! Market Maker Accounts
//!
//! Deterministic account generation for market maker strategies.

use super::strategy::Strategy;
use super::types::StrategyType;
use crate::app::Address;

/// A market maker account with its strategy
pub struct MakerAccount {
    /// Ethereum-style address (0x...)
    pub address: Address,
    /// Strategy instance for this account
    pub strategy: Box<dyn Strategy>,
    /// Strategy type for logging
    pub strategy_type: StrategyType,
    /// Active order IDs (for cancellation tracking)
    pub active_orders: Vec<String>,
    /// Transaction nonce
    pub nonce: u64,
}

impl MakerAccount {
    /// Create a new maker account
    pub fn new(address: Address, strategy: Box<dyn Strategy>, strategy_type: StrategyType) -> Self {
        Self {
            address,
            strategy,
            strategy_type,
            active_orders: Vec::new(),
            nonce: 0,
        }
    }

    /// Generate deterministic address from seed and index
    ///
    /// Uses SHA-256 hash of "mm:{seed}:{index}" to create 20-byte address
    pub fn generate_address(seed: u64, index: usize) -> Address {
        use sha2::{Digest, Sha256};

        let input = format!("mm:{}:{}", seed, index);
        let hash = Sha256::digest(input.as_bytes());

        // Take first 20 bytes for ETH-style address
        format!("0x{}", hex::encode(&hash[..20]))
    }

    /// Generate multiple addresses for a strategy type
    pub fn generate_addresses(
        seed: u64,
        strategy_type: StrategyType,
        count: usize,
    ) -> Vec<Address> {
        // Offset by strategy type to ensure unique addresses per strategy
        let offset = match strategy_type {
            StrategyType::TightQuoter => 0,
            StrategyType::WideQuoter => 1000,
            StrategyType::TrendFollower => 2000,
            StrategyType::MeanReverter => 3000,
            StrategyType::NoiseTrader => 4000,
            StrategyType::AggressiveTaker => 5000,
        };

        (0..count)
            .map(|i| Self::generate_address(seed, offset + i))
            .collect()
    }

    /// Track a placed order
    pub fn add_order(&mut self, order_id: String) {
        self.active_orders.push(order_id);
    }

    /// Remove a cancelled/filled order
    pub fn remove_order(&mut self, order_id: &str) {
        self.active_orders.retain(|id| id != order_id);
    }

    /// Clear all active orders
    pub fn clear_orders(&mut self) {
        self.active_orders.clear();
    }

    /// Get next nonce and increment
    pub fn next_nonce(&mut self) -> u64 {
        let n = self.nonce;
        self.nonce += 1;
        n
    }
}

impl std::fmt::Debug for MakerAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MakerAccount")
            .field("address", &self.address)
            .field("strategy_type", &self.strategy_type)
            .field("active_orders", &self.active_orders.len())
            .field("nonce", &self.nonce)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deterministic_address_generation() {
        let addr1 = MakerAccount::generate_address(12345, 0);
        let addr2 = MakerAccount::generate_address(12345, 0);
        let addr3 = MakerAccount::generate_address(12345, 1);

        // Same seed + index = same address
        assert_eq!(addr1, addr2);
        // Different index = different address
        assert_ne!(addr1, addr3);

        // Verify format
        assert!(addr1.starts_with("0x"));
        assert_eq!(addr1.len(), 42); // 0x + 40 hex chars
    }

    #[test]
    fn test_strategy_addresses_are_unique() {
        let seed = 12345;
        let tight = MakerAccount::generate_addresses(seed, StrategyType::TightQuoter, 2);
        let wide = MakerAccount::generate_addresses(seed, StrategyType::WideQuoter, 2);

        // Different strategies should have different addresses
        assert_ne!(tight[0], wide[0]);
        assert_ne!(tight[1], wide[1]);
    }
}
