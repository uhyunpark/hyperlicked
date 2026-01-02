//! EIP-712 Typed Data Signing
//!
//! Implements EIP-712 for order and cancel signing.
//! Compatible with MetaMask's eth_signTypedData_v4.

use alloy_primitives::{keccak256, Address, B256, U256};
use serde::{Deserialize, Serialize};

use super::signer::{recover_address, Signer, SignerError};

/// EIP-712 Domain separator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EIP712Domain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: Address,
}

impl Default for EIP712Domain {
    fn default() -> Self {
        Self {
            name: "HyperLicked".to_string(),
            version: "1".to_string(),
            chain_id: 1337, // Local dev chain
            verifying_contract: Address::ZERO,
        }
    }
}

impl EIP712Domain {
    /// Compute the domain separator hash
    pub fn separator(&self) -> B256 {
        let type_hash = keccak256(
            b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
        );

        let name_hash = keccak256(self.name.as_bytes());
        let version_hash = keccak256(self.version.as_bytes());

        let mut encoded = Vec::new();
        encoded.extend_from_slice(type_hash.as_slice());
        encoded.extend_from_slice(name_hash.as_slice());
        encoded.extend_from_slice(version_hash.as_slice());
        encoded.extend_from_slice(&U256::from(self.chain_id).to_be_bytes::<32>());
        encoded.extend_from_slice(self.verifying_contract.as_slice());

        keccak256(&encoded)
    }
}

/// Order for EIP-712 signing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderEIP712 {
    pub symbol: String,
    pub side: u8,      // 1 = Buy, 2 = Sell
    pub order_type: u8, // 1 = GTC, 2 = IOC, 3 = ALO
    pub price: U256,
    pub qty: U256,
    pub nonce: U256,
    pub deadline: U256,
    pub leverage: u8,
    pub owner: Address,
}

impl OrderEIP712 {
    /// Compute the struct hash for this order
    pub fn struct_hash(&self) -> B256 {
        let type_hash = keccak256(
            b"Order(string symbol,uint8 side,uint8 type,uint256 price,uint256 qty,uint256 nonce,uint256 deadline,uint8 leverage,address owner)"
        );

        let symbol_hash = keccak256(self.symbol.as_bytes());

        let mut encoded = Vec::new();
        encoded.extend_from_slice(type_hash.as_slice());
        encoded.extend_from_slice(symbol_hash.as_slice());
        encoded.extend_from_slice(&[0u8; 31]); // padding for uint8
        encoded.push(self.side);
        encoded.extend_from_slice(&[0u8; 31]);
        encoded.push(self.order_type);
        encoded.extend_from_slice(&self.price.to_be_bytes::<32>());
        encoded.extend_from_slice(&self.qty.to_be_bytes::<32>());
        encoded.extend_from_slice(&self.nonce.to_be_bytes::<32>());
        encoded.extend_from_slice(&self.deadline.to_be_bytes::<32>());
        encoded.extend_from_slice(&[0u8; 31]);
        encoded.push(self.leverage);
        encoded.extend_from_slice(&[0u8; 12]); // address padding
        encoded.extend_from_slice(self.owner.as_slice());

        keccak256(&encoded)
    }
}

/// Cancel order for EIP-712 signing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelEIP712 {
    pub order_id: String,
    pub symbol: String,
    pub nonce: U256,
    pub owner: Address,
}

impl CancelEIP712 {
    /// Compute the struct hash for this cancel
    pub fn struct_hash(&self) -> B256 {
        let type_hash = keccak256(
            b"CancelOrder(string orderId,string symbol,uint256 nonce,address owner)"
        );

        let order_id_hash = keccak256(self.order_id.as_bytes());
        let symbol_hash = keccak256(self.symbol.as_bytes());

        let mut encoded = Vec::new();
        encoded.extend_from_slice(type_hash.as_slice());
        encoded.extend_from_slice(order_id_hash.as_slice());
        encoded.extend_from_slice(symbol_hash.as_slice());
        encoded.extend_from_slice(&self.nonce.to_be_bytes::<32>());
        encoded.extend_from_slice(&[0u8; 12]);
        encoded.extend_from_slice(self.owner.as_slice());

        keccak256(&encoded)
    }
}

/// EIP-712 signer for orders and cancels
pub struct EIP712Signer {
    domain: EIP712Domain,
}

impl EIP712Signer {
    /// Create a new signer with the given domain
    pub fn new(domain: EIP712Domain) -> Self {
        Self { domain }
    }

    /// Create a signer with the default domain
    pub fn default_domain() -> Self {
        Self::new(EIP712Domain::default())
    }

    /// Hash an order according to EIP-712
    pub fn hash_order(&self, order: &OrderEIP712) -> B256 {
        let domain_separator = self.domain.separator();
        let struct_hash = order.struct_hash();

        let mut message = vec![0x19, 0x01];
        message.extend_from_slice(domain_separator.as_slice());
        message.extend_from_slice(struct_hash.as_slice());

        keccak256(&message)
    }

    /// Sign an order
    pub fn sign_order(&self, signer: &Signer, order: &OrderEIP712) -> [u8; 65] {
        let hash = self.hash_order(order);
        signer.sign(&hash.into())
    }

    /// Verify an order signature
    pub fn verify_order_signature(
        &self,
        order: &OrderEIP712,
        signature: &[u8],
    ) -> Result<bool, SignerError> {
        let hash = self.hash_order(order);
        let recovered = recover_address(&hash.into(), signature)?;
        Ok(recovered == order.owner)
    }

    /// Recover signer address from order signature
    pub fn recover_order_signer(
        &self,
        order: &OrderEIP712,
        signature: &[u8],
    ) -> Result<Address, SignerError> {
        let hash = self.hash_order(order);
        recover_address(&hash.into(), signature)
    }

    /// Hash a cancel according to EIP-712
    pub fn hash_cancel(&self, cancel: &CancelEIP712) -> B256 {
        let domain_separator = self.domain.separator();
        let struct_hash = cancel.struct_hash();

        let mut message = vec![0x19, 0x01];
        message.extend_from_slice(domain_separator.as_slice());
        message.extend_from_slice(struct_hash.as_slice());

        keccak256(&message)
    }

    /// Sign a cancel
    pub fn sign_cancel(&self, signer: &Signer, cancel: &CancelEIP712) -> [u8; 65] {
        let hash = self.hash_cancel(cancel);
        signer.sign(&hash.into())
    }

    /// Verify a cancel signature
    pub fn verify_cancel_signature(
        &self,
        cancel: &CancelEIP712,
        signature: &[u8],
    ) -> Result<bool, SignerError> {
        let hash = self.hash_cancel(cancel);
        let recovered = recover_address(&hash.into(), signature)?;
        Ok(recovered == cancel.owner)
    }

    /// Get the domain for JSON serialization (for frontend)
    pub fn domain(&self) -> &EIP712Domain {
        &self.domain
    }

    /// Generate typed data JSON for MetaMask
    pub fn order_to_json(&self, order: &OrderEIP712) -> serde_json::Value {
        serde_json::json!({
            "types": {
                "EIP712Domain": [
                    {"name": "name", "type": "string"},
                    {"name": "version", "type": "string"},
                    {"name": "chainId", "type": "uint256"},
                    {"name": "verifyingContract", "type": "address"}
                ],
                "Order": [
                    {"name": "symbol", "type": "string"},
                    {"name": "side", "type": "uint8"},
                    {"name": "type", "type": "uint8"},
                    {"name": "price", "type": "uint256"},
                    {"name": "qty", "type": "uint256"},
                    {"name": "nonce", "type": "uint256"},
                    {"name": "deadline", "type": "uint256"},
                    {"name": "leverage", "type": "uint8"},
                    {"name": "owner", "type": "address"}
                ]
            },
            "primaryType": "Order",
            "domain": {
                "name": self.domain.name,
                "version": self.domain.version,
                "chainId": self.domain.chain_id.to_string(),
                "verifyingContract": self.domain.verifying_contract.to_string()
            },
            "message": {
                "symbol": order.symbol,
                "side": order.side,
                "type": order.order_type,
                "price": order.price.to_string(),
                "qty": order.qty.to_string(),
                "nonce": order.nonce.to_string(),
                "deadline": order.deadline.to_string(),
                "leverage": order.leverage,
                "owner": order.owner.to_string()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_separator() {
        let domain = EIP712Domain::default();
        let separator = domain.separator();
        assert!(!separator.is_zero());
    }

    #[test]
    fn test_sign_and_verify_order() {
        let signer = Signer::generate();
        let eip712 = EIP712Signer::default_domain();

        let order = OrderEIP712 {
            symbol: "BTC-USDT".to_string(),
            side: 1,
            order_type: 1,
            price: U256::from(50000),
            qty: U256::from(100),
            nonce: U256::from(1),
            deadline: U256::ZERO,
            leverage: 10,
            owner: signer.address(),
        };

        let signature = eip712.sign_order(&signer, &order);
        assert_eq!(signature.len(), 65);

        let valid = eip712.verify_order_signature(&order, &signature).unwrap();
        assert!(valid);
    }

    #[test]
    fn test_wrong_signer_fails() {
        let signer1 = Signer::generate();
        let signer2 = Signer::generate();
        let eip712 = EIP712Signer::default_domain();

        let order = OrderEIP712 {
            symbol: "BTC-USDT".to_string(),
            side: 1,
            order_type: 1,
            price: U256::from(50000),
            qty: U256::from(100),
            nonce: U256::from(1),
            deadline: U256::ZERO,
            leverage: 10,
            owner: signer1.address(), // Owner is signer1
        };

        // Sign with signer2 (wrong signer)
        let signature = eip712.sign_order(&signer2, &order);

        // Should fail verification
        let valid = eip712.verify_order_signature(&order, &signature).unwrap();
        assert!(!valid);
    }

    #[test]
    fn test_order_to_json() {
        let signer = Signer::generate();
        let eip712 = EIP712Signer::default_domain();

        let order = OrderEIP712 {
            symbol: "BTC-USDT".to_string(),
            side: 1,
            order_type: 1,
            price: U256::from(50000),
            qty: U256::from(100),
            nonce: U256::from(1),
            deadline: U256::ZERO,
            leverage: 10,
            owner: signer.address(),
        };

        let json = eip712.order_to_json(&order);
        assert_eq!(json["primaryType"], "Order");
        assert!(json["types"]["Order"].is_array());
    }
}
