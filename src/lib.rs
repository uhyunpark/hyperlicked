//! Hyperliquid Clone
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
//! ## Quick Start
//!
//! ```rust
//! use hyperlicked::consensus::{Engine, MemoryBlockStore, NoOpApp};
//! use hyperlicked::types::ConsensusConfig;
//!
//! // Create single-node engine
//! let config = ConsensusConfig::single_node();
//! let mut engine = Engine::new(config, NoOpApp, MemoryBlockStore::new());
//!
//! // Produce blocks
//! for _ in 0..10 {
//!     if let Some(block) = engine.tick() {
//!         println!("Committed block at height {}", block.height);
//!     }
//! }
//! ```

pub mod api;
pub mod app;
pub mod config;
pub mod consensus;
pub mod crypto;
pub mod network;
pub mod storage;
pub mod types;

// Re-exports for convenience
pub use api::{SharedState, create_router};
pub use app::AppState;
pub use consensus::{Engine, MemoryBlockStore, NoOpApp};
pub use network::{Network, NetworkConfig, TcpNetwork};
pub use types::*;
