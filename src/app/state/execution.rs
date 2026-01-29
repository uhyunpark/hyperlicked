//! Transaction Execution
//!
//! Handles execution of individual transactions within AppState.

use crate::app::{
    oracle::PriceSource,
    orderbook::{Fill, Order, Side},
    staking, MarketConfig, OrderType, Symbol, Transaction,
};

use super::{AppError, AppState, DepositInfo, OrderUpdateInfo, MAX_TRADES_PER_SYMBOL};

impl AppState {
    /// Execute a single transaction
    pub fn execute_tx(&mut self, tx: Transaction) -> Result<Vec<Fill>, AppError> {
        match tx {
            Transaction::Deposit { trader, amount } => self.execute_deposit(trader, amount),
            Transaction::Withdraw { trader, amount } => self.execute_withdraw(trader, amount),
            Transaction::CancelOrder { trader: _, order_id } => self.execute_cancel(order_id),
            Transaction::PlaceOrder {
                trader,
                symbol,
                side,
                price,
                size,
                order_type,
                reduce_only,
            } => self.execute_place_order(trader, symbol, side, price, size, order_type, reduce_only),

            // Staking transactions
            Transaction::RegisterValidator {
                operator,
                node_id,
                bls_pubkey,
                self_stake,
                commission_bps,
            } => self.execute_register_validator(operator, node_id, bls_pubkey, self_stake, commission_bps),
            Transaction::Delegate {
                delegator,
                validator,
                amount,
            } => self.execute_delegate(delegator, validator, amount),
            Transaction::Undelegate {
                delegator,
                validator,
                amount,
            } => self.execute_undelegate(delegator, validator, amount),
            Transaction::ClaimUnstaked { delegator } => self.execute_claim_unstaked(delegator),
            Transaction::ClaimRewards { claimant, validator } => {
                self.execute_claim_rewards(claimant, validator)
            }
            Transaction::Unjail { operator } => self.execute_unjail(operator),
            Transaction::SubmitEvidence {
                submitter: _,
                evidence,
            } => self.execute_submit_evidence(evidence),

            // Trigger order transactions
            Transaction::PlaceTriggerOrder {
                trader,
                symbol,
                trigger_type,
                trigger_price,
                size,
                limit_price,
                cloid,
            } => {
                self.execute_place_trigger_order(
                    trader,
                    symbol,
                    trigger_type,
                    trigger_price,
                    size,
                    limit_price,
                    cloid,
                )
                .map_err(AppError::Trigger)?;
                Ok(vec![])
            }
            Transaction::CancelTriggerOrder {
                trader,
                trigger_order_id,
            } => {
                self.execute_cancel_trigger_order(trader, trigger_order_id)
                    .map_err(AppError::Trigger)?;
                Ok(vec![])
            }
            Transaction::CancelTriggerOrderByCloid {
                trader,
                symbol,
                cloid,
            } => {
                self.execute_cancel_trigger_order_by_cloid(trader, symbol, cloid)
                    .map_err(AppError::Trigger)?;
                Ok(vec![])
            }

            // Oracle price update
            Transaction::OraclePriceUpdate {
                operator,
                symbol,
                sources,
                signature,
            } => self.execute_oracle_update(operator, symbol, sources, signature),
        }
    }

    fn execute_deposit(&mut self, trader: String, amount: i64) -> Result<Vec<Fill>, AppError> {
        self.accounts.deposit(&trader, amount)?;
        self.mark_dirty_account(&trader);
        self.pending_deposits.push(DepositInfo {
            trader: trader.clone(),
            amount,
        });
        Ok(vec![])
    }

    fn execute_withdraw(&mut self, trader: String, amount: i64) -> Result<Vec<Fill>, AppError> {
        self.accounts.withdraw(&trader, amount)?;
        self.mark_dirty_account(&trader);
        Ok(vec![])
    }

    fn execute_cancel(&mut self, order_id: String) -> Result<Vec<Fill>, AppError> {
        for book in self.orderbooks.values_mut() {
            if let Some(cancelled) = book.cancel(&order_id) {
                // Emit order update with cancelled status
                let side_str = match cancelled.side {
                    Side::Bid => "buy",
                    Side::Ask => "sell",
                };
                let filled = cancelled.original_size - cancelled.size;
                self.pending_order_updates.push(OrderUpdateInfo {
                    trader: cancelled.trader,
                    order_id: cancelled.id,
                    symbol: cancelled.symbol,
                    side: side_str.to_string(),
                    price: cancelled.price,
                    original_size: cancelled.original_size,
                    status: "cancelled".to_string(),
                    filled,
                    remaining: 0, // Cancelled orders have no remaining
                });
                return Ok(vec![]);
            }
        }
        Err(AppError::OrderNotFound)
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_place_order(
        &mut self,
        trader: String,
        symbol: Symbol,
        side: Side,
        price: i64,
        mut size: i64,
        order_type: OrderType,
        reduce_only: bool,
    ) -> Result<Vec<Fill>, AppError> {
        let config = self.configs.get(&symbol).ok_or(AppError::MarketNotFound)?.clone();

        // Handle reduce_only validation
        if reduce_only {
            let account = self.accounts.get_or_create(&trader);
            let pos = account.position(&symbol);

            // Long position (size > 0) can only reduce with sells (Ask)
            // Short position (size < 0) can only reduce with buys (Bid)
            let is_reducing =
                (pos.size > 0 && side == Side::Ask) || (pos.size < 0 && side == Side::Bid);

            if !is_reducing || pos.size == 0 {
                return Err(AppError::ReduceOnlyViolation);
            }

            // Clamp size to position size
            size = size.min(pos.size.abs());
        }

        // Check margin (simplified: require full notional)
        // SECURITY: Use i128 to prevent overflow with large orders.
        // Example: 1000 BTC ($200M) × $200,000 = 2×10^16 (overflows i64)
        let notional = ((size as i128 * price as i128) / 100_000_000)
            .clamp(0, i64::MAX as i128) as i64;
        {
            let account = self.accounts.get_or_create(&trader);
            // SECURITY: Use equity (balance + locked + unrealized PnL) for margin check.
            // Using balance alone allows opening positions with underwater equity.
            let equity = account.equity(&self.mark_prices);
            if equity < notional / 10 {
                return Err(AppError::InsufficientMargin);
            }
        }

        // Place order in isolated scope so book borrow ends
        let (fills, order_id) = {
            let book = self
                .orderbooks
                .get_mut(&symbol)
                .ok_or(AppError::MarketNotFound)?;

            let order_id = book.next_order_id();
            let order = Order {
                id: order_id.clone(),
                trader: trader.clone(),
                symbol: symbol.clone(),
                side,
                price,
                size,
                original_size: size,
                order_type,
                reduce_only,
                timestamp: self.timestamp,
            };

            let fills = book.place(order, &config)?;
            (fills, order_id)
        };

        // Calculate filled amount for the taker's order
        let filled: i64 = fills.iter().map(|f| f.size).sum();
        let remaining = size - filled;

        // Determine order status
        let status = if remaining == 0 {
            "filled"
        } else if filled > 0 {
            "partial"
        } else {
            "open"
        };

        // Emit order update for the taker (order placer)
        let side_str = match side {
            Side::Bid => "buy",
            Side::Ask => "sell",
        };
        self.pending_order_updates.push(OrderUpdateInfo {
            trader: trader.clone(),
            order_id: order_id.clone(),
            symbol: symbol.clone(),
            side: side_str.to_string(),
            price,
            original_size: size,
            status: status.to_string(),
            filled,
            remaining,
        });

        // Process fills (book borrow is now released)
        self.process_fills(&fills, &symbol, &config);

        Ok(fills)
    }

    pub(super) fn process_fills(&mut self, fills: &[Fill], symbol: &Symbol, config: &MarketConfig) {
        for fill in fills {
            let is_buy = fill.side == Side::Bid;
            self.accounts.apply_fill(
                &fill.maker,
                &fill.taker,
                &fill.symbol,
                is_buy,
                fill.size,
                fill.price,
                config.maker_fee,
                config.taker_fee,
            );

            // Mark both accounts as dirty for incremental hashing
            self.mark_dirty_account(&fill.maker);
            self.mark_dirty_account(&fill.taker);

            // Update mark price to last trade
            self.mark_prices.insert(symbol.clone(), fill.price);
            self.mark_globals_dirty();

            // Store fill in trade history
            let history = self.trade_history.entry(fill.symbol.clone()).or_default();
            history.push_back(fill.clone());
            while history.len() > MAX_TRADES_PER_SYMBOL {
                history.pop_front();
            }

            // Update candle aggregation
            self.candle_manager
                .add_trade(&fill.symbol, fill.price, fill.size, fill.timestamp);
        }
    }

    // === Staking Transaction Handlers ===

    fn execute_register_validator(
        &mut self,
        operator: String,
        node_id: crate::types::NodeId,
        bls_pubkey: Vec<u8>,
        self_stake: i64,
        commission_bps: i64,
    ) -> Result<Vec<Fill>, AppError> {
        // Deduct self-stake from account
        self.accounts.withdraw(&operator, self_stake)?;
        self.mark_dirty_account(&operator);
        self.staking.register_validator(
            operator.clone(),
            node_id,
            bls_pubkey,
            self_stake,
            commission_bps,
        )?;
        self.mark_globals_dirty(); // Staking state changed
        self.pending_staking_events.push(
            staking::StakingTxResult::ValidatorRegistered { operator, node_id },
        );
        Ok(vec![])
    }

    fn execute_delegate(
        &mut self,
        delegator: String,
        validator: String,
        amount: i64,
    ) -> Result<Vec<Fill>, AppError> {
        // Check validator exists BEFORE withdrawing funds
        if self.staking.get_validator(&validator).is_none() {
            return Err(AppError::from(staking::StakingError::ValidatorNotFound));
        }
        // Deduct delegation amount from account
        self.accounts.withdraw(&delegator, amount)?;
        self.mark_dirty_account(&delegator);
        self.staking
            .delegate(delegator.clone(), validator.clone(), amount)?;
        self.mark_globals_dirty(); // Staking state changed
        self.pending_staking_events
            .push(staking::StakingTxResult::Delegated {
                delegator,
                validator,
                amount,
            });
        Ok(vec![])
    }

    fn execute_undelegate(
        &mut self,
        delegator: String,
        validator: String,
        amount: i64,
    ) -> Result<Vec<Fill>, AppError> {
        self.staking.undelegate(
            delegator.clone(),
            validator.clone(),
            amount,
            self.timestamp,
        )?;
        self.mark_globals_dirty(); // Staking state changed
        let completion_time = self.timestamp + staking::UNSTAKE_DELAY_MS;
        self.pending_staking_events
            .push(staking::StakingTxResult::Undelegated {
                delegator,
                validator,
                amount,
                completion_time,
            });
        Ok(vec![])
    }

    fn execute_claim_unstaked(&mut self, delegator: String) -> Result<Vec<Fill>, AppError> {
        let completions = self.staking.process_unstake_queue(self.timestamp);
        let mut total_amount = 0i64;
        for (d, amount) in completions {
            if d == delegator {
                // Return funds to account
                self.accounts.deposit(&d, amount)?;
                self.mark_dirty_account(&d);
                total_amount += amount;
            }
        }
        if total_amount > 0 {
            self.mark_globals_dirty(); // Staking state changed
        }
        self.pending_staking_events
            .push(staking::StakingTxResult::UnstakeClaimed {
                delegator,
                amount: total_amount,
            });
        Ok(vec![])
    }

    fn execute_claim_rewards(
        &mut self,
        claimant: String,
        validator: Option<String>,
    ) -> Result<Vec<Fill>, AppError> {
        let amount = match &validator {
            Some(val) => {
                let result = self.staking.claim_delegation_rewards(&claimant, val)?;
                result.amount
            }
            None => {
                let result = self.staking.claim_validator_rewards(&claimant)?;
                result.amount
            }
        };
        // Add rewards to account balance
        if amount > 0 {
            self.accounts.deposit(&claimant, amount)?;
            self.mark_dirty_account(&claimant);
            self.mark_globals_dirty(); // Staking state changed
        }
        self.pending_staking_events
            .push(staking::StakingTxResult::RewardsClaimed { claimant, amount });
        Ok(vec![])
    }

    fn execute_unjail(&mut self, operator: String) -> Result<Vec<Fill>, AppError> {
        self.staking.unjail(&operator, self.timestamp)?;
        self.mark_globals_dirty(); // Staking state changed
        self.pending_staking_events
            .push(staking::StakingTxResult::Unjailed { operator });
        Ok(vec![])
    }

    fn execute_submit_evidence(
        &mut self,
        evidence: staking::Evidence,
    ) -> Result<Vec<Fill>, AppError> {
        let offender = evidence.offender;
        self.staking.submit_evidence(evidence)?;
        let results = self.staking.process_pending_evidence();
        let slashed_amount = results
            .iter()
            .filter(|r| r.offender == offender)
            .map(|r| r.total_slashed)
            .sum();
        self.mark_globals_dirty(); // Staking state changed
        self.pending_staking_events
            .push(staking::StakingTxResult::EvidenceProcessed {
                offender,
                slashed_amount,
            });
        Ok(vec![])
    }

    // === Oracle Transaction Handlers ===

    fn execute_oracle_update(
        &mut self,
        operator: String,
        symbol: Symbol,
        sources: Vec<PriceSource>,
        signature: Vec<u8>,
    ) -> Result<Vec<Fill>, AppError> {
        // Check operator authorization (must be registered validator)
        // Skip check if signature verification is disabled (dev mode)
        let skip_verify = std::env::var("SKIP_SIG_VERIFY")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);

        // Get validator (required even if skipping sig verify, for authorization)
        let validator = self.staking.get_validator(&operator);

        if !skip_verify {
            let validator = validator.ok_or_else(|| {
                AppError::Oracle(crate::app::oracle::OracleError::UnauthorizedOperator(
                    operator.clone(),
                ))
            })?;

            // Verify BLS signature over price data
            self.verify_oracle_signature(&symbol, &sources, &signature, &validator.bls_pubkey)?;
        }

        // Get mark price for circuit breaker check
        let mark_price = self.mark_prices.get(&symbol).copied();

        // Process the oracle update
        self.oracle.process_update(&symbol, sources, self.timestamp, mark_price)?;

        tracing::debug!(
            operator = %operator,
            symbol = %symbol,
            price = ?self.oracle.prices.get(&symbol).map(|p| p.price),
            "Oracle price updated"
        );

        Ok(vec![])
    }

    /// Verify BLS signature over oracle price data
    ///
    /// The signing data is: symbol || timestamp || sources (serialized)
    fn verify_oracle_signature(
        &self,
        symbol: &Symbol,
        sources: &[PriceSource],
        signature: &[u8],
        bls_pubkey: &[u8],
    ) -> Result<(), AppError> {
        use crate::crypto::bls::{BlsPublicKey, BlsSignature};

        // Parse BLS public key
        if bls_pubkey.len() != 48 {
            return Err(AppError::Oracle(
                crate::app::oracle::OracleError::InvalidSignature("Invalid BLS pubkey length".into()),
            ));
        }
        let mut pk_arr = [0u8; 48];
        pk_arr.copy_from_slice(bls_pubkey);
        let pubkey = BlsPublicKey::from_bytes(&pk_arr).map_err(|_| {
            AppError::Oracle(crate::app::oracle::OracleError::InvalidSignature(
                "Failed to parse BLS pubkey".into(),
            ))
        })?;

        // Parse BLS signature
        let sig = BlsSignature::from_slice(signature).map_err(|_| {
            AppError::Oracle(crate::app::oracle::OracleError::InvalidSignature(
                "Failed to parse BLS signature".into(),
            ))
        })?;

        // Build signing data: symbol || timestamp || sources
        let signing_data = self.build_oracle_signing_data(symbol, sources);

        // Verify signature
        if !pubkey.verify(&signing_data, &sig) {
            return Err(AppError::Oracle(
                crate::app::oracle::OracleError::InvalidSignature(
                    "BLS signature verification failed".into(),
                ),
            ));
        }

        Ok(())
    }

    /// Build deterministic signing data for oracle updates
    fn build_oracle_signing_data(&self, symbol: &Symbol, sources: &[PriceSource]) -> Vec<u8> {
        let mut data = Vec::new();

        // Symbol
        data.extend_from_slice(symbol.as_bytes());

        // Timestamp (current block timestamp)
        data.extend_from_slice(&self.timestamp.to_le_bytes());

        // Sources (deterministic serialization)
        for source in sources {
            data.extend_from_slice(source.source_id.as_bytes());
            data.extend_from_slice(&source.price.to_le_bytes());
            data.extend_from_slice(&source.timestamp.to_le_bytes());
        }

        data
    }
}
