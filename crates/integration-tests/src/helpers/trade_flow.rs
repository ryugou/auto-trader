//! Phase 3 trade flow test helpers.
//!
//! CSV フィクスチャからキャンドルを読み取り、PriceEvent に変換して
//! 戦略に直接流すためのユーティリティ群。

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::DateTime;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::Path;

/// CSV 行の中間構造体。
#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    timestamp: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: i64,
    bid: Option<Decimal>,
    ask: Option<Decimal>,
}

/// CSV ファイルからキャンドルを読み込み、PriceEvent のベクタに変換する。
///
/// `exchange` と `pair` は呼び出し側が指定する。`timeframe` は戦略に応じて
/// "M5" や "H1" を指定する。
pub fn load_events_from_csv(
    csv_path: &Path,
    exchange: Exchange,
    pair: &str,
    timeframe: &str,
) -> Vec<PriceEvent> {
    let mut reader = csv::Reader::from_path(csv_path)
        .unwrap_or_else(|e| panic!("CSV を開けません: {}: {e}", csv_path.display()));

    let mut events = Vec::new();
    for result in reader.deserialize() {
        let row: CsvRow =
            result.unwrap_or_else(|e| panic!("CSV パース失敗: {}: {e}", csv_path.display()));

        let ts = DateTime::parse_from_rfc3339(&row.timestamp)
            .unwrap_or_else(|e| panic!("タイムスタンプパース失敗: {}: {e}", row.timestamp))
            .with_timezone(&chrono::Utc);

        let candle = Candle {
            pair: Pair::new(pair),
            exchange,
            timeframe: timeframe.to_string(),
            open: row.open,
            high: row.high,
            low: row.low,
            close: row.close,
            volume: Some(u64::try_from(row.volume).expect("fixture volume must be non-negative")),
            best_bid: row.bid,
            best_ask: row.ask,
            timestamp: ts,
        };

        events.push(PriceEvent {
            pair: Pair::new(pair),
            exchange,
            timestamp: ts,
            candle,
            indicators: HashMap::new(),
        });
    }

    events
}

/// フィクスチャディレクトリのパスを返す。
pub fn fixtures_dir() -> std::path::PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(manifest)
        .join("fixtures")
        .join("phase3")
}
