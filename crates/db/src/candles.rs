use auto_trader_core::types::Candle;
use auto_trader_core::types::Exchange;
use auto_trader_core::types::Pair;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn upsert_candle(pool: &PgPool, candle: &Candle) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO price_candles (pair, timeframe, open, high, low, close, volume, timestamp)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           ON CONFLICT (pair, timeframe, timestamp) DO UPDATE
           SET open = $3, high = $4, low = $5, close = $6, volume = $7"#,
    )
    .bind(&candle.pair.0)
    .bind(&candle.timeframe)
    .bind(candle.open)
    .bind(candle.high)
    .bind(candle.low)
    .bind(candle.close)
    .bind(candle.volume.map(|v| v as i64))
    .bind(candle.timestamp)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_candles(
    pool: &PgPool,
    pair: &str,
    timeframe: &str,
    limit: i64,
) -> anyhow::Result<Vec<Candle>> {
    let rows = sqlx::query_as::<_, CandleRow>(
        r#"SELECT pair, timeframe, open, high, low, close, volume, timestamp
           FROM price_candles
           WHERE pair = $1 AND timeframe = $2
           ORDER BY timestamp DESC
           LIMIT $3"#,
    )
    .bind(pair)
    .bind(timeframe)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into()).collect())
}

#[derive(sqlx::FromRow)]
struct CandleRow {
    pair: String,
    timeframe: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Option<i64>,
    timestamp: DateTime<Utc>,
}

impl From<CandleRow> for Candle {
    fn from(r: CandleRow) -> Self {
        Candle {
            pair: Pair::new(&r.pair),
            exchange: Exchange::Oanda,
            timeframe: r.timeframe,
            open: r.open,
            high: r.high,
            low: r.low,
            close: r.close,
            volume: r.volume.map(|v| v as u64),
            timestamp: r.timestamp,
        }
    }
}
