//! Cryptographic Primitives
//!
//! EIP-712 typed data signing and agent key delegation.
//!
//! ## Components
//!
//! - `eip712`: EIP-712 typed data hashing and signing
//! - `agent`: Agent key delegation for gasless trading
//! - `signer`: ECDSA signing utilities

mod eip712;
mod agent;
mod signer;

pub use eip712::{EIP712Domain, EIP712Signer, OrderEIP712, CancelEIP712};
pub use agent::{AgentDelegation, AgentSigner, verify_agent_order};
pub use signer::{Signer, recover_address};
