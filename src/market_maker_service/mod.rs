//! Dev-only external market-maker runtime.
//!
//! `hl-mm` talks to `hl-node` through its public API, owns deterministic
//! development keys, signs canonical envelopes, and waits for finalized
//! receipts before submitting the next transaction.

mod client;
mod config;
mod identity;
mod service;
mod wire;

pub use config::ServiceConfig;
pub use identity::{derive_dev_identities, DevIdentity};
pub use service::MarketMakerService;
