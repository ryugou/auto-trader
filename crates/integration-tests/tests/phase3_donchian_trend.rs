//! Phase 3A: Donchian Trend V1 strategy signal tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。

use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use auto_trader_core::strategy::Strategy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const PAIR: &str = "USD_JPY";
const TIMEFRAME: &str = "H1";

fn new_strategy() -> DonchianTrendV1 {
    DonchianTrendV1::new(
        "donchian_trend_v1".to_string(),
        vec![Pair::new(PAIR)],
    )
}

/// 20bar 高値ブレイク + ATR > baseline → Long シグナル。
#[tokio::test]
async fn donchian_long_breakout() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    assert!(
        events.len() >= 56,
        "donchian_long_breakout.csv must have at least 56 rows, got {}",
        events.len()
    );

    // Feed warmup (all except last)
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long breakout signal from donchian_long_breakout fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert_eq!(sig.strategy_name, "donchian_trend_v1");
    assert_eq!(sig.pair, Pair::new(PAIR));
    // ATR-based SL: positive and at most 5%
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    // Turtle has no fixed TP
    assert!(sig.take_profit_pct.is_none());
    // Donchian has no max_hold_until
    assert!(sig.max_hold_until.is_none());
}

/// 20bar 安値ブレイク + ATR > baseline → Short シグナル。
#[tokio::test]
async fn donchian_short_breakout() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_short_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Short breakout signal from donchian_short_breakout fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Short);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    assert!(sig.take_profit_pct.is_none());
}

/// チャネル内推移 → シグナルなし。
#[tokio::test]
async fn donchian_no_signal() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_no_signal.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let mut any_signal = false;
    for event in &events {
        if strategy.on_price(event).await.is_some() {
            any_signal = true;
        }
    }

    assert!(
        !any_signal,
        "expected no signal when price stays within Donchian channel"
    );
}

/// 全バー同一価格 → ATR=0 → シグナルなし。
#[tokio::test]
async fn donchian_atr_zero() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_atr_zero.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let mut any_signal = false;
    for event in &events {
        if strategy.on_price(event).await.is_some() {
            any_signal = true;
        }
    }

    assert!(
        !any_signal,
        "expected no signal when ATR is zero"
    );
}

/// 55 本未満 → 履歴不足でシグナルなし。
#[tokio::test]
async fn donchian_history_insufficient() {
    let mut strategy = new_strategy();
    // 10 bars inline — well below the 55-bar minimum
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_no_signal.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    // Feed only the first 10 bars
    let short = &events[..10.min(events.len())];
    let mut any_signal = false;
    for event in short {
        if strategy.on_price(event).await.is_some() {
            any_signal = true;
        }
    }

    assert!(
        !any_signal,
        "expected no signal with insufficient history (10 bars < 55 minimum)"
    );
}

/// BitflyerCfd exchange でも同じロジックが動くことを確認。
#[tokio::test]
async fn donchian_long_breakout_bitflyer() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::BitflyerCfd,
        PAIR,
        TIMEFRAME,
    );

    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long signal on BitflyerCfd exchange too"
    );
    assert_eq!(signal.unwrap().direction, Direction::Long);
}
