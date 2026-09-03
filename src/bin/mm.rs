//! Dev-only external market-maker process.

use anyhow::{bail, Result};
use clap::Parser;
use hyperlicked::app::market_maker::Intensity;
use hyperlicked::market_maker_service::{MarketMakerService, ServiceConfig};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "hl-mm", about = "Run the dev-only external market maker")]
struct Args {
    /// Loopback hl-node API origin.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    node_url: String,
    /// Market symbol to quote.
    #[arg(long, default_value = "BTC-USDT")]
    symbol: String,
    /// Deterministic development account seed.
    #[arg(long, default_value_t = 12345)]
    seed: u64,
    /// Strategy intensity: low, medium, or high.
    #[arg(long, default_value = "low")]
    intensity: String,
    /// Milliseconds between strategy ticks.
    #[arg(long, default_value_t = 1_000)]
    interval_ms: u64,
    /// Simulated collateral target per account, in base units.
    #[arg(long, default_value_t = 100_000_000)]
    target_balance: i64,
    /// Explicit cents price fallback if the node has no oracle/mark/mid price.
    #[arg(long)]
    reference_price: Option<i64>,
    /// Maximum newly placed orders per strategy tick.
    #[arg(long, default_value_t = 4)]
    max_orders_per_tick: usize,
    /// Maximum open orders retained per account.
    #[arg(long, default_value_t = 4)]
    max_open_orders_per_account: usize,
    /// Maximum POST submissions in any rolling one-minute window (max 60).
    #[arg(long, default_value_t = 60)]
    max_submissions_per_minute: usize,
    /// Run a finite number of ticks, useful for smoke tests.
    #[arg(long)]
    ticks: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let args = Args::parse();
    let intensity = match args.intensity.to_ascii_lowercase().as_str() {
        "low" | "showcase" => Intensity::Low,
        "medium" => Intensity::Medium,
        "high" | "stress" => Intensity::High,
        value => bail!("invalid market-maker intensity `{value}`; use low, medium, or high"),
    };
    let config = ServiceConfig {
        node_url: args.node_url,
        symbol: args.symbol,
        seed: args.seed,
        intensity,
        interval_ms: args.interval_ms,
        target_balance: args.target_balance,
        reference_price: args.reference_price,
        max_orders_per_tick: args.max_orders_per_tick,
        max_open_orders_per_account: args.max_open_orders_per_account,
        max_submissions_per_minute: args.max_submissions_per_minute,
        ticks: args.ticks,
        ..ServiceConfig::default()
    };
    MarketMakerService::new(config)?.run().await
}
