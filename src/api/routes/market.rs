//! Market Endpoints
//!
//! Market data, orderbook, trades, candles, and funding information.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;

use crate::api::state::PriceLevel;
use crate::api::types::{ApiState, CandleInfo, FundingInfo, MarketInfo, OrderbookSnapshot};

pub async fn get_markets(State(_state): State<ApiState>) -> Json<Vec<MarketInfo>> {
    Json(vec![MarketInfo::default()])
}

pub async fn get_market(
    State(_state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<MarketInfo>, StatusCode> {
    if symbol == "BTC-USDT" {
        Ok(Json(MarketInfo::default()))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub async fn get_orderbook(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<OrderbookSnapshot>, StatusCode> {
    let app = state.shared.app.read().await;
    let book = app.orderbook(&symbol).ok_or(StatusCode::NOT_FOUND)?;

    let bids: Vec<PriceLevel> = book
        .bid_levels(20)
        .iter()
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    let asks: Vec<PriceLevel> = book
        .ask_levels(20)
        .iter()
        .map(|l| PriceLevel {
            price: l.price,
            size: l.size,
        })
        .collect();

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    Ok(Json(OrderbookSnapshot {
        symbol,
        bids,
        asks,
        timestamp,
    }))
}

/// Query parameters for trades endpoint
#[derive(Deserialize)]
pub struct TradesQuery {
    limit: Option<usize>,
}

/// Trade response format
#[derive(serde::Serialize)]
pub struct TradeResponse {
    /// Deterministic trade ID for deduplication
    id: String,
    price: i64,
    size: i64,
    side: String,
    timestamp: u64,
}

pub async fn get_trades(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
    Query(params): Query<TradesQuery>,
) -> Json<Vec<TradeResponse>> {
    let limit = params.limit.unwrap_or(100).min(1000);
    let app = state.shared.app.read().await;

    let trades: Vec<TradeResponse> = app
        .get_trades(&symbol, limit)
        .into_iter()
        .map(|f| {
            let side = match f.side {
                crate::app::Side::Bid => "buy",
                crate::app::Side::Ask => "sell",
            };
            // Generate deterministic ID from trade content for deduplication
            let id = format!("{}-{}-{}-{}", f.timestamp, f.price, f.size, side);
            TradeResponse {
                id,
                price: f.price,
                size: f.size,
                side: side.to_string(),
                timestamp: f.timestamp,
            }
        })
        .collect();

    Json(trades)
}

/// Query parameters for candles endpoint
#[derive(Deserialize)]
pub struct CandlesQuery {
    interval: Option<String>,
    limit: Option<usize>,
}

pub async fn get_candles(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
    Query(params): Query<CandlesQuery>,
) -> Result<Json<Vec<CandleInfo>>, StatusCode> {
    use crate::app::Interval;

    let interval_str = params.interval.as_deref().unwrap_or("1m");
    let interval = Interval::from_str(interval_str).ok_or(StatusCode::BAD_REQUEST)?;
    let limit = params.limit.unwrap_or(500).min(500);

    let app = state.shared.app.read().await;

    // Check if market exists
    if app.orderbook(&symbol).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let candles: Vec<CandleInfo> = app
        .get_candles(&symbol, interval, limit)
        .into_iter()
        .map(|c| CandleInfo {
            time: c.time,
            open: c.open,
            high: c.high,
            low: c.low,
            close: c.close,
            volume: c.volume,
            trades: c.trades,
        })
        .collect();

    Ok(Json(candles))
}

pub async fn get_funding(
    State(state): State<ApiState>,
    Path(symbol): Path<String>,
) -> Result<Json<FundingInfo>, StatusCode> {
    let app = state.shared.app.read().await;

    // Check if market exists
    if app.orderbook(&symbol).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let funding_rate_bps = app.funding_rate(&symbol);
    let last_funding_time = app.last_funding_time(&symbol);
    let next_funding_time = app.next_funding_time(&symbol);

    Ok(Json(FundingInfo {
        symbol,
        funding_rate: funding_rate_bps as f64 / 10000.0, // Convert bps to decimal
        funding_rate_bps,
        next_funding_time,
        last_funding_time,
    }))
}
