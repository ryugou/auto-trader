//! Phase 3A: Squeeze Momentum V1 strategy signal tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。

use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1;
use auto_trader_core::strategy::Strategy;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

const PAIR: &str = "USD_JPY";
const TIMEFRAME: &str = "H1";

fn new_strategy() -> SqueezeMomentumV1 {
    SqueezeMomentumV1::new(
        "squeeze_momentum_v1".to_string(),
        vec![Pair::new(PAIR)],
    )
}

/// TTM Squeeze 解除 + 正の上昇モメンタム → Long シグナル。
#[tokio::test]
async fn squeeze_long_entry() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_long_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    assert!(
        events.len() >= 25,
        "squeeze_long_entry.csv must have at least 25 rows, got {}",
        events.len()
    );

    // Feed warmup (all except last)
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Long squeeze signal from squeeze_long_entry fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert_eq!(sig.strategy_name, "squeeze_momentum_v1");
    assert_eq!(sig.pair, Pair::new(PAIR));
    // ATR-based SL: positive and at most 5%
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    // Dynamic exit → no fixed TP
    assert!(sig.take_profit_pct.is_none());
    // 48h time limit
    assert!(sig.max_hold_until.is_some());
}

/// TTM Squeeze 解除 + 負の下降モメンタム → Short シグナル。
#[tokio::test]
async fn squeeze_short_entry() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_short_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;

    let signal = strategy.on_price(&trigger[0]).await;
    assert!(
        signal.is_some(),
        "expected Short squeeze signal from squeeze_short_entry fixture"
    );
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Short);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.stop_loss_pct <= dec!(0.05));
    assert!(sig.take_profit_pct.is_none());
    assert!(sig.max_hold_until.is_some());
}

/// BB が KC 内に留まり続ける → シグナルなし。
#[tokio::test]
async fn squeeze_no_signal() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_no_signal.csv"),
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
        "expected no signal when BB stays inside KC (squeeze never fires)"
    );
}

/// 全バー同一価格 → ATR=0 → シグナルなし。
#[tokio::test]
async fn squeeze_atr_zero() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_atr_zero.csv"),
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

/// 24 本未満 → 履歴不足でシグナルなし。
#[tokio::test]
async fn squeeze_history_insufficient() {
    let mut strategy = new_strategy();
    // 10 bars — below the 24-bar minimum
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_no_signal.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );

    let short = &events[..10.min(events.len())];
    let mut any_signal = false;
    for event in short {
        if strategy.on_price(event).await.is_some() {
            any_signal = true;
        }
    }

    assert!(
        !any_signal,
        "expected no signal with insufficient history (10 bars)"
    );
}

/// BitflyerCfd exchange でも同じロジックが動くことを確認。
#[tokio::test]
async fn squeeze_long_entry_bitflyer() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_long_entry.csv"),
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
