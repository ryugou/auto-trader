use std::collections::HashMap;
use std::path::Path;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::DateTime;
use rust_decimal::Decimal;

#[derive(Debug, serde::Deserialize)]
struct CsvRow {
    timestamp: String,
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: u64,
    bid: Option<Decimal>,
    ask: Option<Decimal>,
}

/// Reads the candles of a CSV file and returns one price event per row, in file order, for the given exchange, pair and timeframe.
///
/// # Panics
///
/// - Panics if the CSV file cannot be opened.
/// - Panics if a CSV row cannot be parsed.
/// - Panics if the timestamp of a row is not in RFC 3339 format.
pub fn load_events_from_csv(
    csv_path: &Path,
    exchange: Exchange,
    pair: &str,
    timeframe: &str,
) -> Vec<PriceEvent> {
    let mut reader = csv::Reader::from_path(csv_path)
        .unwrap_or_else(|e| panic!("failed to open csv file \"{}\": {e}", csv_path.display()));

    let mut events = Vec::new();
    for result in reader.deserialize() {
        let row: CsvRow = result
            .unwrap_or_else(|e| panic!("failed to parse csv row \"{}\": {e}", csv_path.display()));

        let ts = DateTime::parse_from_rfc3339(&row.timestamp)
            .unwrap_or_else(|e| panic!("failed to parse timestamp \"{}\": {e}", row.timestamp))
            .with_timezone(&chrono::Utc);

        let candle = Candle {
            pair: Pair::new(pair),
            exchange,
            timeframe: timeframe.to_string(),
            open: row.open,
            high: row.high,
            low: row.low,
            close: row.close,
            volume: Some(row.volume),
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

/// Returns the path `fixtures/phase3` under the directory in `CARGO_MANIFEST_DIR`, or under the current directory if that variable is not set.
pub fn fixtures_dir() -> std::path::PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(manifest)
        .join("fixtures")
        .join("phase3")
}
