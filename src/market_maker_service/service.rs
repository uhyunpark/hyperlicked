use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};

use crate::app::market_maker::{MarketMakerConfig, MarketMakerState};
use crate::app::Transaction;

use super::client::NodeClient;
use super::config::ServiceConfig;
use super::identity::{derive_dev_identities, DevIdentity};
use super::wire::{signed_request, ReceiptResponse, SignedRequest};

const ORDER_VALIDITY_MS: u64 = 3_600_000;

/// Standalone market-maker service.
pub struct MarketMakerService {
    config: ServiceConfig,
    client: NodeClient,
    identities: Vec<DevIdentity>,
    identity_by_address: HashMap<String, usize>,
    state: MarketMakerState,
    chain_domain: Option<[u8; 32]>,
    submission_times: VecDeque<Instant>,
}

impl MarketMakerService {
    pub fn new(config: ServiceConfig) -> Result<Self> {
        config.validate()?;
        let client = NodeClient::new(&config)?;
        let identities = derive_dev_identities(config.seed, config.intensity)?;
        let addresses = identities
            .iter()
            .map(|identity| identity.address().to_string())
            .collect();
        let state_config = MarketMakerConfig {
            enabled: true,
            interval_ms: config.interval_ms,
            intensity: config.intensity,
            seed: config.seed,
            symbol: config.symbol.clone(),
            initial_deposit: config.target_balance,
        };
        let state = MarketMakerState::new_with_addresses(state_config, addresses)?;
        let identity_by_address = identities
            .iter()
            .enumerate()
            .map(|(index, identity)| (identity.address().to_ascii_lowercase(), index))
            .collect();
        Ok(Self {
            config,
            client,
            identities,
            identity_by_address,
            state,
            chain_domain: None,
            submission_times: VecDeque::new(),
        })
    }

    pub async fn run(mut self) -> Result<()> {
        let domain = self.client.discover_chain_domain().await?;
        self.chain_domain = Some(domain);
        tracing::info!(
            accounts = self.identities.len(),
            "checking simulated market-maker balances"
        );
        for index in 0..self.identities.len() {
            self.ensure_funded(index).await?;
        }
        tracing::info!(
            accounts = self.identities.len(),
            symbol = %self.config.symbol,
            "dev market-maker ready"
        );

        let mut completed_ticks = 0u64;
        let mut consecutive_failures = 0u32;
        loop {
            if self
                .config
                .ticks
                .is_some_and(|limit| completed_ticks >= limit)
            {
                return Ok(());
            }
            let tick_result = tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("market-maker shutdown requested");
                    return Ok(());
                }
                result = self.tick() => result,
            };
            match tick_result {
                Ok(submitted) => {
                    consecutive_failures = 0;
                    completed_ticks = completed_ticks.saturating_add(1);
                    if submitted > 0 {
                        tracing::info!(
                            submitted,
                            tick = completed_ticks,
                            "market-maker tick complete"
                        );
                    }
                }
                Err(error) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    tracing::warn!(
                        error = %error,
                        consecutive_failures,
                        "market-maker tick failed"
                    );
                    if consecutive_failures >= self.config.max_consecutive_failures {
                        return Err(error)
                            .context("market-maker stopped after consecutive failures");
                    }
                }
            }
            if self
                .config
                .ticks
                .is_some_and(|limit| completed_ticks >= limit)
            {
                return Ok(());
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("market-maker shutdown requested");
                    return Ok(());
                }
                _ = tokio::time::sleep(Duration::from_millis(self.config.interval_ms)) => {}
            }
        }
    }

    async fn ensure_funded(&mut self, index: usize) -> Result<()> {
        let address = self.identities[index].address().to_string();
        let account = self.client.account(&address).await?;
        let total = account.balance.saturating_add(account.locked_collateral);
        if total >= self.config.target_balance {
            return Ok(());
        }
        let amount = self.config.target_balance.saturating_sub(total);
        self.acquire_submission_slot().await;
        self.client.deposit(&address, amount).await?;

        let deadline = Instant::now() + self.config.finality_timeout();
        loop {
            let account = self.client.account(&address).await?;
            let total = account.balance.saturating_add(account.locked_collateral);
            if total >= self.config.target_balance {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("dev faucet deposit did not finalize for {address}");
            }
            tokio::time::sleep(self.config.receipt_poll()).await;
        }
    }

    async fn tick(&mut self) -> Result<usize> {
        let context = self.client.context(&self.config.symbol).await?;
        let orderbook = self.client.orderbook(&self.config.symbol).await?;
        let reference_price = context
            .oracle_price
            .and_then(positive)
            .or_else(|| positive(context.mark_price))
            .or_else(|| positive(context.mid_price))
            .or_else(|| {
                orderbook
                    .bids
                    .first()
                    .and_then(|level| positive(level.price))
            })
            .or_else(|| {
                orderbook
                    .asks
                    .first()
                    .and_then(|level| positive(level.price))
            })
            .or(self.config.reference_price)
            .ok_or_else(|| anyhow!("no usable oracle, mark, mid, or configured reference price"))?;
        let best_bid = orderbook.bids.first().map(|level| level.price);
        let best_ask = orderbook.asks.first().map(|level| level.price);
        let generated = self
            .state
            .tick(Some(reference_price), best_bid, best_ask, now_millis());
        let mut grouped: Vec<Vec<Transaction>> = vec![Vec::new(); self.identities.len()];
        for transaction in generated {
            let key = transaction.trader_address().to_ascii_lowercase();
            let index = self
                .identity_by_address
                .get(&key)
                .copied()
                .ok_or_else(|| anyhow!("strategy produced an unknown trader address"))?;
            grouped[index].push(transaction);
        }

        let mut remaining = self.config.max_orders_per_tick;
        let mut submitted = 0usize;
        for (index, transactions) in grouped.into_iter().enumerate() {
            if remaining == 0 {
                break;
            }
            let transactions: Vec<_> = transactions
                .into_iter()
                .filter(|transaction| matches!(transaction, Transaction::PlaceOrder { .. }))
                .take(remaining)
                .collect();
            if transactions.is_empty() {
                continue;
            }
            let placed = self.reconcile_and_submit(index, transactions).await?;
            remaining = remaining.saturating_sub(placed);
            submitted = submitted.saturating_add(placed);
        }
        Ok(submitted)
    }

    async fn reconcile_and_submit(
        &mut self,
        index: usize,
        transactions: Vec<Transaction>,
    ) -> Result<usize> {
        let address = self.identities[index].address().to_string();
        let mut active = self
            .client
            .orders(&address)
            .await?
            .into_iter()
            .filter(|order| order.symbol == self.config.symbol && is_open_status(&order.status))
            .collect::<Vec<_>>();
        active.sort_by_key(|order| order.timestamp);
        let active_count = active.len();
        let excess = active_count
            .saturating_add(transactions.len())
            .saturating_sub(self.config.max_open_orders_per_account);
        let cancel_count = excess.min(self.config.max_orders_per_tick);
        for order in active.into_iter().take(cancel_count) {
            let action = Transaction::CancelOrder {
                trader: address.clone(),
                order_id: order.id,
            };
            self.submit_action(index, action, Some(order.symbol))
                .await?;
        }
        let available_slots = self
            .config
            .max_open_orders_per_account
            .saturating_sub(active_count.saturating_sub(cancel_count));
        let transactions = transactions.into_iter().take(available_slots);
        let mut placed = 0usize;
        for transaction in transactions {
            self.submit_action(index, transaction, None).await?;
            placed = placed.saturating_add(1);
        }
        Ok(placed)
    }

    async fn submit_action(
        &mut self,
        index: usize,
        action: Transaction,
        cancel_symbol: Option<String>,
    ) -> Result<()> {
        let nonce = {
            let address = self.identities[index].address().to_string();
            self.client.nonce(&address).await?
        };
        let valid_until = now_millis().saturating_add(ORDER_VALIDITY_MS);
        let domain = self
            .chain_domain
            .ok_or_else(|| anyhow!("chain domain has not been discovered"))?;
        let request = {
            let identity = &self.identities[index];
            signed_request(
                domain,
                &identity.signer,
                nonce,
                valid_until,
                action,
                cancel_symbol,
            )?
        };
        let hash = self.submit_with_retry(request).await?;
        self.wait_for_receipt(&hash).await
    }

    async fn submit_with_retry(&mut self, request: SignedRequest) -> Result<String> {
        let mut last_error = None;
        for attempt in 0..=self.config.max_retries {
            self.acquire_submission_slot().await;
            match self.client.submit_signed(&request).await {
                Ok(hash) => return Ok(hash),
                Err(error) => {
                    let retryable = error.downcast_ref::<reqwest::Error>().is_some();
                    last_error = Some(error);
                    if !retryable || attempt == self.config.max_retries {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50 * (attempt as u64 + 1))).await;
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow!("transaction submission failed")))
    }

    async fn wait_for_receipt(&self, hash: &str) -> Result<()> {
        let deadline = Instant::now() + self.config.finality_timeout();
        loop {
            let response = self
                .client
                .receipt(hash)
                .await
                .with_context(|| format!("receipt request failed for {hash}"))?;
            if response.status().is_success() {
                let receipt: ReceiptResponse = response
                    .json()
                    .await
                    .context("invalid finalized transaction receipt")?;
                if receipt.receipt_status == 0 {
                    return Ok(());
                }
                bail!(
                    "transaction {hash} finalized with receipt_status={}",
                    receipt.receipt_status
                );
            }
            if response.status() != reqwest::StatusCode::NOT_FOUND {
                bail!("receipt request returned HTTP {}", response.status());
            }
            if Instant::now() >= deadline {
                bail!("transaction {hash} was not finalized before timeout");
            }
            tokio::time::sleep(self.config.receipt_poll()).await;
        }
    }

    async fn acquire_submission_slot(&mut self) {
        let window = Duration::from_secs(60);
        loop {
            while self
                .submission_times
                .front()
                .is_some_and(|timestamp| timestamp.elapsed() >= window)
            {
                self.submission_times.pop_front();
            }
            if self.submission_times.len() < self.config.max_submissions_per_minute {
                self.submission_times.push_back(Instant::now());
                return;
            }
            let wait = self
                .submission_times
                .front()
                .map(|timestamp| window.saturating_sub(timestamp.elapsed()))
                .unwrap_or_default();
            tokio::time::sleep(wait).await;
        }
    }
}

fn positive(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

fn is_open_status(status: &str) -> bool {
    !matches!(
        status.to_ascii_lowercase().as_str(),
        "closed" | "cancelled" | "filled"
    )
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
