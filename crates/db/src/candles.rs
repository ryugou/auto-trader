use auto_trader_core::types::Candle;
use auto_trader_core::types::Exchange;
use auto_trader_core::types::Pair;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;

pub async fn upsert_candle(pool: &PgPool, candle: &Candle) -> anyhow::Result<()> {
    sqlx::query(
        r#"INSERT INTO price_candles (exchange, pair, timeframe, open, high, low, close, volume, timestamp)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (exchange, pair, timeframe, timestamp) DO UPDATE
           SET open = $4, high = $5, low = $6, close = $7, volume = $8"#,
    )
    .bind(candle.exchange.as_str())
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
    exchange: &str,
    pair: &str,
    timeframe: &str,
    limit: i64,
) -> anyhow::Result<Vec<Candle>> {
    let rows = sqlx::query_as::<_, CandleRow>(
        r#"SELECT exchange, pair, timeframe, open, high, low, close, volume, timestamp
           FROM price_candles
           WHERE exchange = $1 AND pair = $2 AND timeframe = $3
           ORDER BY timestamp DESC
           LIMIT $4"#,
    )
    .bind(exchange)
    .bind(pair)
    .bind(timeframe)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.into()).collect())
}

#[derive(sqlx::FromRow)]
struct CandleRow {
    exchange: String,
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
        let exchange = match r.exchange.as_str() {
            "bitflyer_cfd" => Exchange::BitflyerCfd,
            _ => Exchange::Oanda,
        };
        Candle {
            pair: Pair::new(&r.pair),
            exchange,
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
