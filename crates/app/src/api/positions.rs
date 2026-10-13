use super::{ApiError, AppState};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct PositionResponse {
    pub trade_id: Uuid,
    pub account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub exchange: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Option<Decimal>,
    pub quantity: Decimal,
    pub fees: Decimal,
    pub entry_at: DateTime<Utc>,
    pub account_id: Uuid,
}

fn exclude_unreachable_take_profit_sentinel(
    entry_price: Decimal,
    take_profit: Option<Decimal>,
) -> Option<Decimal> {
    take_profit.and_then(|tp| {
        if entry_price.is_zero() {
            Some(tp)
        } else {
            let ratio = tp / entry_price;
            if ratio > Decimal::from(100) || ratio < Decimal::from(1) / Decimal::from(100) {
                None
            } else {
                Some(tp)
            }
        }
    })
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<PositionResponse>>, ApiError> {
    let rows = auto_trader_db::trades::list_open_with_account_name(&state.pool)
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to list open positions: {e}"),
            )
        })?;

    let result = rows
        .into_iter()
        .map(|row| {
            let t = row.trade;
            let direction = t.direction.as_str().to_string();
            let take_profit =
                exclude_unreachable_take_profit_sentinel(t.entry_price, t.take_profit);

            PositionResponse {
                trade_id: t.id,
                account_name: row.account_name.unwrap_or_default(),
                strategy_name: t.strategy_name,
                pair: t.pair.0,
                exchange: t.exchange.as_str().to_string(),
                direction,
                entry_price: t.entry_price,
                stop_loss: t.stop_loss,
                take_profit,
                quantity: t.quantity,
                fees: t.fees,
                entry_at: t.entry_at,
                account_id: t.account_id,
            }
        })
        .collect();

    Ok(Json(result))
}
