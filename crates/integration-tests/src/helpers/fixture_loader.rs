use std::path::Path;

use anyhow::{Context, Result};
use chrono::DateTime;
use rust_decimal::Decimal;
use sqlx::PgPool;

/// CSV の各行に対応する中間構造体。
#[derive(Debug, serde::Deserialize)]
struct CandleRow {
    timestamp: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: i64,
    /// CSV に含まれるが DB カラムには存在しないためスキップ。
    #[allow(dead_code)]
    bid: Option<Decimal>,
    /// CSV に含まれるが DB カラムには存在しないためスキップ。
    #[allow(dead_code)]
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
        .with_context(|| format!("CSV ファイルを開けません: {}", csv_path.display()))?;

    let mut count = 0usize;

    for result in reader.deserialize() {
        let row: CandleRow =
            result.with_context(|| format!("CSV 行のパースに失敗: {}", csv_path.display()))?;

        let ts = DateTime::parse_from_rfc3339(&row.timestamp)
            .with_context(|| format!("タイムスタンプのパースに失敗: {}", row.timestamp))?
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
        .with_context(|| format!("price_candles への INSERT に失敗: ts={ts}"))?;

        count += 1;
    }

    Ok(count)
}
