//! Account Endpoints
//!
//! Account information, positions, orders, and nonces.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;

use crate::api::types::{AccountInfo, ApiState, FillInfo, FundingPayment, OrderInfo, PositionInfo};

pub async fn get_account(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<AccountInfo> {
    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => {
            return Json(AccountInfo {
                address: address.clone(),
                balance: 0,
                locked_collateral: 0,
                available_balance: 0,
                unrealized_pnl: 0,
                total_equity: 0,
            })
        }
    };

    Json(AccountInfo {
        address: account.address.clone(),
        balance: account.balance,
        locked_collateral: account.locked,
        available_balance: account.balance,
        unrealized_pnl: 0,
        total_equity: account.balance + account.locked,
    })
}

pub async fn get_positions(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<PositionInfo>> {
    use crate::app::MAINTENANCE_MARGIN_BPS;

    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => return Json(vec![]),
    };

    // Get available margin for liquidation price calculation
    let available_margin = account.balance + account.locked;

    let positions: Vec<PositionInfo> = account
        .positions
        .iter()
        .filter(|(_, pos)| pos.size != 0)
        .map(|(symbol, pos)| {
            let mark = app.mark_price(symbol).unwrap_or(pos.entry_price);
            let notional = pos.notional(mark);
            // Approximate margin allocated to this position (proportional to notional)
            let total_notional: i64 = account
                .positions
                .values()
                .filter(|p| p.size != 0)
                .map(|p| p.notional(mark))
                .sum();
            let position_margin = if total_notional > 0 {
                (available_margin * notional) / total_notional
            } else {
                available_margin
            };

            PositionInfo {
                symbol: symbol.clone(),
                size: pos.size,
                entry_price: pos.entry_price,
                mark_price: mark,
                liquidation_price: pos.liquidation_price(position_margin, MAINTENANCE_MARGIN_BPS),
                unrealized_pnl: pos.unrealized_pnl(mark),
                margin: position_margin,
                leverage: if position_margin > 0 {
                    notional as f64 / position_margin as f64
                } else {
                    0.0
                },
            }
        })
        .collect();

    Json(positions)
}

pub async fn get_nonce(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let app = state.shared.app.read().await;
    let nonce = app.accounts().get_nonce(&address);
    Json(serde_json::json!({ "address": address, "nonce": nonce }))
}

pub async fn get_orders(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<OrderInfo>> {
    let app = state.shared.app.read().await;
    let orders = app.orders_by_address(&address);

    let order_infos: Vec<OrderInfo> = orders
        .iter()
        .map(|o| {
            let side = match o.side {
                crate::app::Side::Bid => "buy",
                crate::app::Side::Ask => "sell",
            };
            let order_type = match o.order_type {
                crate::app::OrderType::Gtc => "limit",
                crate::app::OrderType::Ioc => "market",
                crate::app::OrderType::Alo => "limit",
            };
            let filled = o.original_size - o.size;
            let status = if filled > 0 && o.size > 0 {
                "partial"
            } else {
                "open"
            };

            OrderInfo {
                id: o.id.clone(),
                symbol: o.symbol.clone(),
                side: side.to_string(),
                order_type: order_type.to_string(),
                price: o.price,
                size: o.original_size,
                filled,
                status: status.to_string(),
                timestamp: o.timestamp,
            }
        })
        .collect();

    Json(order_infos)
}

pub async fn get_account_funding(
    State(state): State<ApiState>,
    Path(address): Path<String>,
) -> Json<Vec<FundingPayment>> {
    let app = state.shared.app.read().await;

    let account = match app.account(&address) {
        Some(acc) => acc,
        None => return Json(vec![]),
    };

    // Return cumulative funding for each position
    // Only include positions that have had funding applied (timestamp > 0)
    let payments: Vec<FundingPayment> = account
        .positions
        .iter()
        .filter(|(_, pos)| pos.last_funding_timestamp > 0 && (pos.cumulative_funding != 0 || pos.size != 0))
        .map(|(symbol, pos)| FundingPayment {
            symbol: symbol.clone(),
            payment: pos.cumulative_funding,
            payment_usd: pos.cumulative_funding as f64 / 100.0,
            funding_rate_bps: app.funding_rate(symbol),
            timestamp: pos.last_funding_timestamp,
        })
        .collect();

    Json(payments)
}

#[derive(Debug, Deserialize)]
pub struct FillsQuery {
    pub limit: Option<usize>,
}

/// Get user's trade fills (trades where user is taker or maker)
pub async fn get_user_fills(
    State(state): State<ApiState>,
    Path(address): Path<String>,
    Query(query): Query<FillsQuery>,
) -> Json<Vec<FillInfo>> {
    use crate::app::orderbook::Side;

    let app = state.shared.app.read().await;
    let limit = query.limit.unwrap_or(100).min(500);
    let address_lower = address.to_lowercase();

    // Get all user fills using the AppState method
    let raw_fills = app.get_user_fills(&address, limit);

    // Convert to FillInfo, determining if user is maker or taker for each fill
    let fills: Vec<FillInfo> = raw_fills
        .iter()
        .map(|fill| {
            let is_maker = fill.maker.to_lowercase() == address_lower;
            let side = if is_maker {
                // Maker is opposite side of taker
                match fill.side {
                    Side::Bid => "sell",
                    Side::Ask => "buy",
                }
            } else {
                // Taker
                match fill.side {
                    Side::Bid => "buy",
                    Side::Ask => "sell",
                }
            };
            let id = if is_maker {
                fill.maker_order_id.clone()
            } else {
                format!("{}-taker", fill.taker_order_id)
            };

            FillInfo {
                id,
                symbol: fill.symbol.clone(),
                side: side.to_string(),
                price: fill.price,
                size: fill.size,
                fee: 0, // TODO: Calculate actual fee
                is_maker,
                timestamp: fill.timestamp,
            }
        })
        .collect();

    Json(fills)
}
