//! E2E Test Modules
//!
//! Organized by domain with shared test helpers.
//!
//! ## Structure
//!
//! - `helpers/` - Shared test utilities (fixtures, builders, assertions)
//! - `*_test.rs` - Domain-specific test files
//!
//! ## Running Tests
//!
//! ```bash
//! # All e2e tests
//! cargo test --test e2e
//!
//! # Specific domain
//! cargo test --test e2e accounts
//! cargo test --test e2e matching
//! ```

pub mod helpers;

// Phase 1: Core Trading
pub mod accounts_test;
pub mod orders_test;
pub mod matching_test;
pub mod positions_test;

// Phase 2: Risk Management
pub mod liquidation_test;
pub mod funding_test;
pub mod triggers_test;

// Phase 3: Staking & Integration
pub mod staking_test;
pub mod integration_test;
pub mod oracle_test;
