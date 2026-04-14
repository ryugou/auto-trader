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
    pub paper_account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub exchange: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Option<Decimal>,
    pub quantity: Option<Decimal>,
    /// Accumulated fees on this open position (overnight fees, etc.).
    /// Used by the Positions page to compute 純損益 = 含み損益 - fees.
    pub fees: Decimal,
    pub entry_at: DateTime<Utc>,
    pub paper_account_id: Option<Uuid>,
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
            let direction = serde_json::to_string(&t.direction)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();
            // Strategies that exit dynamically (mean-revert, trailing
            // channel, etc.) park take_profit at an unreachable sentinel
            // (entry × 1000 for Long, entry / 1000 for Short).  Expose
            // these as null so the UI shows "-" instead of nonsense.
            let tp_ratio = if t.entry_price.is_zero() {
                Decimal::ONE
            } else {
                t.take_profit / t.entry_price
            };
            let take_profit = if tp_ratio > Decimal::from(100)
                || tp_ratio < Decimal::from(1) / Decimal::from(100)
            {
                None
            } else {
                Some(t.take_profit)
            };

            PositionResponse {
                trade_id: t.id,
                paper_account_name: row.paper_account_name.unwrap_or_default(),
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
                paper_account_id: t.paper_account_id,
            }
        })
        .collect();

    Ok(Json(result))
}
