use super::{ApiError, AppState};
use crate::api::filters::DashboardFilter;
use auto_trader_db::dashboard;
use axum::extract::{Query, State};
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SummaryResponse {
    pub trade_count: i64,
    pub win_count: i64,
    pub loss_count: i64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub net_pnl: Decimal,
    pub total_fees: Decimal,
    pub max_drawdown: Decimal,
    pub expected_value: f64,
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub async fn summary(
    State(state): State<AppState>,
    Query(filter): Query<DashboardFilter>,
) -> Result<Json<SummaryResponse>, ApiError> {
    let from = filter.from.as_deref().and_then(parse_date);
    let to = filter.to.as_deref().and_then(parse_date);

    let stats = dashboard::get_summary(
        &state.pool,
        filter.exchange.as_deref(),
        filter.paper_account_id,
        from,
        to,
    )
    .await
    .map_err(ApiError::from)?;

    let loss_count = stats.total_trades - stats.win_count;
    let win_rate = if stats.total_trades > 0 {
        stats.win_count as f64 / stats.total_trades as f64
    } else {
        0.0
    };
    let net_pnl = stats.total_pnl - stats.total_fees;
    let expected_value = if stats.total_trades > 0 {
        // Convert net_pnl Decimal to f64 for expected value calculation
        let pnl_f64: f64 = net_pnl.to_string().parse().unwrap_or(0.0);
        pnl_f64 / stats.total_trades as f64
    } else {
        0.0
    };

    Ok(Json(SummaryResponse {
        trade_count: stats.total_trades,
        win_count: stats.win_count,
        loss_count,
        win_rate,
        total_pnl: stats.total_pnl,
        net_pnl,
        total_fees: stats.total_fees,
        max_drawdown: stats.max_drawdown,
        expected_value,
    }))
}

pub async fn pnl_history(
    State(state): State<AppState>,
    Query(filter): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::PnlHistoryRow>>, ApiError> {
    let from = filter.from.as_deref().and_then(parse_date);
    let to = filter.to.as_deref().and_then(parse_date);

    dashboard::get_pnl_history(
        &state.pool,
        filter.exchange.as_deref(),
        filter.paper_account_id,
        from,
        to,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

pub async fn strategies(
    State(state): State<AppState>,
    Query(filter): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::StrategyStats>>, ApiError> {
    dashboard::get_strategy_stats(
        &state.pool,
        filter.exchange.as_deref(),
        filter.paper_account_id,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

pub async fn pairs(
    State(state): State<AppState>,
    Query(filter): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::PairStats>>, ApiError> {
    dashboard::get_pair_stats(
        &state.pool,
        filter.exchange.as_deref(),
        filter.paper_account_id,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}

pub async fn hourly_winrate(
    State(state): State<AppState>,
    Query(filter): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::HourlyWinrate>>, ApiError> {
    dashboard::get_hourly_winrate(
        &state.pool,
        filter.exchange.as_deref(),
        filter.paper_account_id,
    )
    .await
    .map(Json)
    .map_err(ApiError::from)
}
