//! Cryptographic Primitives
//!
//! EIP-712 typed data signing and agent key delegation.
//!
//! ## Components
//!
//! - `eip712`: EIP-712 typed data hashing and signing
//! - `agent`: Agent key delegation for gasless trading
//! - `signer`: ECDSA signing utilities
//! - `bls`: BLS12-381 signature aggregation for consensus

mod agent;
pub mod bls;
mod eip712;
mod signer;

pub use agent::{verify_agent_order, AgentDelegation, AgentSigner};
pub use bls::{
    aggregate_public_keys, aggregate_signatures, verify_aggregate, BlsError, BlsPublicKey,
    BlsSecretKey, BlsSignature,
};
pub use eip712::{CancelEIP712, EIP712Domain, EIP712Signer, OrderEIP712};
pub use signer::{recover_address, Signer};
