//! Hyperlicked
//!
//! A mock implementation of Hyperlicked for testing and development.
//! A high-performance perpetual futures exchange built with HotStuff-2 consensus.
//!
//! ## Architecture
//!
//! See `docs/ARCHITECTURE.md` for full system design.
//!
//! ## Modules
//!
//! - `types`: Core data types (Block, Vote, Order, etc.)
//! - `consensus`: HotStuff-2 consensus engine
//!
//! ## Runtime entry point
//!
//! Production validators are started through the `hl-node` binary and use
//! [`consensus::ConsensusRunner`]. The former in-memory `Engine` is retained
//! only behind the opt-in `legacy-engine` compatibility feature.

pub mod api;
pub mod app;
pub mod config;
pub mod consensus;
pub mod crypto;
pub mod market_maker_service;
pub mod network;
pub mod node_config;
pub mod state_sync;
pub mod storage;
pub mod types;
pub mod visor;

// Re-exports for convenience
pub use api::{create_router, SharedState};
pub use app::AppState;
pub use consensus::{MemoryBlockStore, NoOpApp};
pub use network::{Network, NetworkConfig, TcpNetwork};
pub use state_sync::{import_verified_blocks, VerifiedBlockImporter};
pub use types::*;
