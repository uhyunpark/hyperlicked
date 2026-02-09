//! Parallel Transaction Execution
//!
//! Provides optimized transaction execution with symbol-based grouping.
//!
//! When `parallel_matching` feature is enabled:
//! - Global transactions (deposits, staking) execute first sequentially
//! - Symbol-scoped transactions are grouped by symbol for cache locality
//!
//! ## Future Optimization
//!
//! True parallel orderbook matching requires:
//! - Thread-safe orderbook or per-symbol locking
//! - Deferred fill application (accounts are shared across symbols)
//!
//! Current implementation groups transactions for better locality but
//! processes sequentially to maintain correctness.

#[cfg(feature = "parallel_matching")]
use std::collections::BTreeMap;

use crate::app::{orderbook::Fill, Transaction};

#[cfg(feature = "parallel_matching")]
use crate::app::Symbol;

use super::AppState;

impl AppState {
    /// Execute transactions with symbol-based grouping for optimized execution.
    ///
    /// When parallel_matching feature is enabled:
    /// - Global transactions (deposits, staking) execute first sequentially
    /// - Symbol-scoped transactions are grouped by symbol for cache locality
    #[cfg(feature = "parallel_matching")]
    pub fn execute_transactions_parallel(&mut self, txs: Vec<Transaction>) -> Vec<Fill> {
        // Separate global vs symbol-scoped transactions
        let (global_txs, symbol_txs): (Vec<_>, Vec<_>) =
            txs.into_iter().partition(|tx| tx.symbol().is_none());

        // Phase 0: Execute global transactions sequentially (deposits, withdrawals, staking)
        let mut all_fills = Vec::new();
        for tx in global_txs {
            match self.execute_tx(tx) {
                Ok(fills) => all_fills.extend(fills),
                Err(e) => tracing::warn!(error = %e, "Global transaction failed"),
            }
        }

        // Phase 1: Execute symbol-scoped transactions
        // Group by symbol for better cache locality
        let mut by_symbol: BTreeMap<Symbol, Vec<Transaction>> = BTreeMap::new();
        for tx in symbol_txs {
            if let Some(symbol) = tx.symbol() {
                by_symbol.entry(symbol.clone()).or_default().push(tx);
            }
        }

        // Process each symbol's transactions
        // Note: We process symbols sequentially since we need &mut self
        // True parallelism would require per-symbol locks or extraction
        for (_symbol, txs) in by_symbol {
            for tx in txs {
                match self.execute_tx(tx) {
                    Ok(fills) => all_fills.extend(fills),
                    Err(e) => tracing::warn!(error = %e, "Transaction failed"),
                }
            }
        }

        all_fills
    }

    /// Fallback: Execute transactions sequentially (when parallel_matching disabled)
    #[cfg(not(feature = "parallel_matching"))]
    pub fn execute_transactions_parallel(&mut self, txs: Vec<Transaction>) -> Vec<Fill> {
        let mut all_fills = Vec::new();
        for tx in txs {
            match self.execute_tx(tx) {
                Ok(fills) => all_fills.extend(fills),
                Err(e) => tracing::warn!(error = %e, "Transaction failed"),
            }
        }
        all_fills
    }
}
