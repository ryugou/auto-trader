//! Phase 3A: BB Mean Revert V1 strategy signal tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。

use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::bb_mean_revert::BbMeanRevertV1;
use auto_trader_core::strategy::Strategy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const PAIR: &str = "USD_JPY";
const TIMEFRAME: &str = "M5";

fn new_strategy() -> BbMeanRevertV1 {
    BbMeanRevertV1::new(
        "bb_mean_revert_v1".to_string(),
        vec![Pair::new(PAIR)],
    )
}

/// BB 下限 + RSI < 25 + lower-low → Long シグナル発火。
#[tokio::test]
async fn bb_long_entry() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_long_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    assert!(
        events.len() >= 22,
        "bb_long_entry.csv must have at least 22 rows, got {}",
        events.len()
    );

    // Feed warmup events (all except the last)
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    // Feed the final trigger event
    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long signal from bb_long_entry fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert_eq!(sig.strategy_name, "bb_mean_revert_v1");
    assert_eq!(sig.pair, Pair::new(PAIR));
    // ATR-based SL: positive and at most 3%
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.03));
    // Dynamic exit → no fixed TP
    assert!(sig.take_profit_pct.is_none());
    // 24h time limit
    assert!(sig.max_hold_until.is_some());
}

/// BB 上限 + RSI > 75 + higher-high → Short シグナル発火。
#[tokio::test]
async fn bb_short_entry() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_short_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Short signal from bb_short_entry fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Short);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.03));
    assert!(sig.take_profit_pct.is_none());
    assert!(sig.max_hold_until.is_some());
}

/// BB 内 + RSI 中間 → シグナルなし。
#[tokio::test]
async fn bb_no_signal() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_no_signal.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let mut last_signal = None;
    for event in &events {
        last_signal = strategy.on_price(event).await;
    }

    assert!(
        last_signal.is_none(),
        "expected no signal from bb_no_signal fixture"
    );
}

/// 全バー同一価格 → ATR=0 → シグナルなし。
#[tokio::test]
async fn bb_atr_zero() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_atr_zero.csv"),
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
        "expected no signal when ATR is zero (all prices identical)"
    );
}

/// 21 本未満 → 履歴不足でシグナルなし。
#[tokio::test]
async fn bb_history_insufficient() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_history_insufficient.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    assert!(
        events.len() < 21,
        "bb_history_insufficient.csv should have fewer than 21 rows"
    );

    let mut any_signal = false;
    for event in &events {
        if strategy.on_price(event).await.is_some() {
            any_signal = true;
        }
    }

    assert!(
        !any_signal,
        "expected no signal with insufficient history ({} bars < 21 minimum)",
        events.len()
    );
}

/// BitflyerCfd exchange でも同じロジックが動くことを確認（パラメタライズ）。
#[tokio::test]
async fn bb_long_entry_bitflyer() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_long_entry.csv"),
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
