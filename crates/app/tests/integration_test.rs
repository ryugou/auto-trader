//! Integration tests that do not require a database.
//!
//! PaperTrader is now DB-backed and cannot be exercised in pure unit tests.
//! Remaining coverage here focuses on DB-independent components such as
//! technical indicators and channel wiring.

use auto_trader_core::event::SignalEvent;
use auto_trader_core::types::*;
use auto_trader_market::indicators;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;

#[tokio::test]
async fn indicators_consistency() {
    let prices: Vec<Decimal> = (0..100).map(|i| dec!(100) + Decimal::from(i) / dec!(10)).collect();
    let sma20 = indicators::sma(&prices, 20).unwrap();
    let sma50 = indicators::sma(&prices, 50).unwrap();
    // In an uptrend, short MA > long MA
    assert!(sma20 > sma50, "sma20={sma20} should be > sma50={sma50}");
}

#[tokio::test]
async fn channel_pipeline() {
    let (signal_tx, mut signal_rx) = mpsc::channel::<SignalEvent>(16);

    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        entry_price: dec!(150.00),
        stop_loss: dec!(149.50),
        take_profit: dec!(151.00),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(0.5),
        max_hold_until: None,
    };

    signal_tx.send(SignalEvent { signal: signal.clone() }).await.unwrap();
    let received = signal_rx.recv().await.unwrap();
    assert_eq!(received.signal.pair, Pair::new("USD_JPY"));
    assert_eq!(received.signal.direction, Direction::Long);
}
