use super::{ApiError, AppState};
use auto_trader_core::executor::OrderExecutor;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
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
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub entry_at: DateTime<Utc>,
    pub paper_account_id: Option<Uuid>,
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<PositionResponse>>, ApiError> {
    let mut result = Vec::new();

    for (account_name, trader) in &state.paper_traders {
        let positions = trader.open_positions().await.map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to get positions for {account_name}: {e}"),
            )
        })?;

        for pos in positions {
            let t = &pos.trade;
            let direction = serde_json::to_string(&t.direction)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string();

            result.push(PositionResponse {
                trade_id: t.id,
                paper_account_name: account_name.clone(),
                strategy_name: t.strategy_name.clone(),
                pair: t.pair.0.clone(),
                exchange: t.exchange.as_str().to_string(),
                direction,
                entry_price: t.entry_price,
                stop_loss: t.stop_loss,
                take_profit: t.take_profit,
                quantity: t.quantity,
                entry_at: t.entry_at,
                paper_account_id: t.paper_account_id,
            });
        }
    }

    Ok(Json(result))
}
