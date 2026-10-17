use std::path::Path;

use anyhow::{Context, Result};
use chrono::DateTime;
use rust_decimal::Decimal;
use sqlx::PgPool;

#[derive(Debug, serde::Deserialize)]
struct CandleRow {
    timestamp: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: i64,
    #[expect(dead_code, reason = "csv column absent from price_candles")]
    bid: Option<Decimal>,
    #[expect(dead_code, reason = "csv column absent from price_candles")]
    ask: Option<Decimal>,
}

/// CSV ファイルからローソク足データを読み込み、`price_candles` テーブルに挿入する。
///
/// 同一 `(exchange, pair, timeframe, timestamp)` の行が既に存在する場合は
/// `ON CONFLICT ... DO UPDATE` で上書きする。
///
/// 挿入(または更新)した行数を返す。
pub async fn load_price_candles(
    pool: &PgPool,
    exchange: &str,
    pair: &str,
    timeframe: &str,
    csv_path: &Path,
) -> Result<usize> {
    let mut reader = csv::Reader::from_path(csv_path)
        .with_context(|| format!("failed to open csv file \"{}\"", csv_path.display()))?;

    let mut count = 0usize;

    for result in reader.deserialize() {
        let row: CandleRow = result
            .with_context(|| format!("failed to parse csv row \"{}\"", csv_path.display()))?;

        let ts = DateTime::parse_from_rfc3339(&row.timestamp)
            .with_context(|| format!("failed to parse timestamp \"{}\"", row.timestamp))?
            .with_timezone(&chrono::Utc);

        sqlx::query(
            r#"
            INSERT INTO price_candles (exchange, pair, timeframe, open, high, low, close, volume, timestamp)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (exchange, pair, timeframe, timestamp)
            DO UPDATE SET open = EXCLUDED.open,
                          high = EXCLUDED.high,
                          low  = EXCLUDED.low,
                          close = EXCLUDED.close,
                          volume = EXCLUDED.volume
            "#,
        )
        .bind(exchange)
        .bind(pair)
        .bind(timeframe)
        .bind(row.open)
        .bind(row.high)
        .bind(row.low)
        .bind(row.close)
        .bind(row.volume)
        .bind(ts)
        .execute(pool)
        .await
        .with_context(|| format!("failed to insert price candle: ts={ts}"))?;

        count += 1;
    }

    Ok(count)
}
