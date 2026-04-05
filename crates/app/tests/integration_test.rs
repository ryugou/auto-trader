//! Integration tests for paper trading pipeline.
//! These tests run in-memory and do not require external services.

use auto_trader_core::event::SignalEvent;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_market::indicators;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::test]
async fn paper_trade_roundtrip() {
    let trader = PaperTrader::new(dec!(100000), dec!(25), None);

    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        entry_price: dec!(150.00),
        stop_loss: dec!(149.50),
        take_profit: dec!(151.00),
        confidence: 0.8,
        timestamp: Utc::now(),
    };

    // Open
    let trade = trader.execute(&signal).await.unwrap();
    assert_eq!(trade.status, TradeStatus::Open);

    // Close with profit
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(151.00))
        .await
        .unwrap();
    assert_eq!(closed.status, TradeStatus::Closed);
    // USD_JPY: 1.00 price diff / 0.01 pip size = 100 pips
    assert_eq!(closed.pnl_pips.unwrap(), dec!(100));

    // Balance: price_diff * leverage = 1.00 * 25 = 25
    assert_eq!(trader.balance().await, dec!(100025));
}

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
    };

    signal_tx.send(SignalEvent { signal: signal.clone() }).await.unwrap();
    let received = signal_rx.recv().await.unwrap();
    assert_eq!(received.signal.pair, Pair::new("USD_JPY"));
    assert_eq!(received.signal.direction, Direction::Long);
}

#[tokio::test]
async fn paper_trader_close_at_sl_price() {
    let trader = PaperTrader::new(dec!(100000), dec!(25), None);

    // Open a long position
    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        entry_price: dec!(150.00),
        stop_loss: dec!(149.50),
        take_profit: dec!(151.00),
        confidence: 0.8,
        timestamp: Utc::now(),
    };
    let trade = trader.execute(&signal).await.unwrap();
    assert_eq!(trade.status, TradeStatus::Open);

    // SL hit: close at stop_loss price
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::SlHit, dec!(149.50))
        .await
        .unwrap();
    assert_eq!(closed.status, TradeStatus::Closed);
    assert_eq!(closed.exit_reason, Some(ExitReason::SlHit));
    assert_eq!(closed.exit_price, Some(dec!(149.50)));
    // PnL: (149.50 - 150.00) / 0.01 = -50 pips
    assert_eq!(closed.pnl_pips.unwrap(), dec!(-50));
}

#[tokio::test]
async fn crypto_paper_trade_with_quantity() {
    let trader = PaperTrader::new(dec!(100000), dec!(2), Some(Uuid::new_v4()));

    let signal = Signal {
        strategy_name: "crypto_trend_v1".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        entry_price: dec!(15000000),
        stop_loss: dec!(14800000),
        take_profit: dec!(15400000),
        confidence: 0.8,
        timestamp: Utc::now(),
    };

    let trade = trader.execute_with_quantity(&signal, dec!(0.01)).await.unwrap();
    assert_eq!(trade.exchange, Exchange::BitflyerCfd);
    assert_eq!(trade.quantity, Some(dec!(0.01)));

    // Close: pnl = (15400000 - 15000000) * 0.01 = 4000 JPY
    let closed = trader
        .close_position(&trade.id.to_string(), ExitReason::TpHit, dec!(15400000))
        .await
        .unwrap();
    assert_eq!(closed.pnl_amount, Some(dec!(4000)));

    // Balance: 100000 + 4000 = 104000
    assert_eq!(trader.balance().await, dec!(104000));
}

#[tokio::test]
async fn overnight_fee_deducted() {
    let trader = PaperTrader::new(dec!(100000), dec!(2), Some(Uuid::new_v4()));

    let signal = Signal {
        strategy_name: "crypto_trend_v1".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        entry_price: dec!(15000000),
        stop_loss: dec!(14800000),
        take_profit: dec!(15400000),
        confidence: 0.8,
        timestamp: Utc::now(),
    };
    trader.execute_with_quantity(&signal, dec!(0.01)).await.unwrap();

    // fee_rate = 0.04% → notional = 15000000 * 0.01 = 150000 → fee = 150000 * 0.0004 = 60
    let fees = trader.apply_overnight_fees(dec!(0.0004)).await;
    assert_eq!(fees, dec!(60));
    assert_eq!(trader.balance().await, dec!(99940)); // 100000 - 60
}
