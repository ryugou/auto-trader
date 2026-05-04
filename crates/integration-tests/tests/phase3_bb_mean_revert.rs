//! Phase 3A: BB Mean Revert V1 strategy signal + exit tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。
//!
//! Exit tests (3.3-3.6, 3.8-3.9): エントリー CSV でウォームアップし、
//! Position を構築してから on_open_positions でエグジット判定を検証。

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{Strategy, StrategyExitReason};
use auto_trader_core::types::{
    Candle, Direction, Exchange, Pair, Position, Trade, TradeStatus,
};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::bb_mean_revert::BbMeanRevertV1;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use uuid::Uuid;

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

// ─── Exit tests helpers ──────────────────────────────────────────────────

fn make_m5_event(pair: &str, close: Decimal, high: Decimal, low: Decimal, ts: DateTime<Utc>) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(pair),
        exchange: Exchange::GmoFx,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(pair),
            exchange: Exchange::GmoFx,
            timeframe: TIMEFRAME.to_string(),
            open: close,
            high,
            low,
            close,
            volume: Some(0),
            best_bid: None,
            best_ask: None,
            timestamp: ts,
        },
        indicators: HashMap::new(),
    }
}

fn make_position(
    strategy_name: &str,
    pair: &str,
    direction: Direction,
    entry_price: Decimal,
    stop_loss: Decimal,
) -> Position {
    Position {
        trade: Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: strategy_name.to_string(),
            pair: Pair::new(pair),
            exchange: Exchange::GmoFx,
            direction,
            entry_price,
            exit_price: None,
            stop_loss,
            take_profit: None,
            quantity: dec!(1000),
            leverage: dec!(25),
            fees: dec!(0),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
        },
    }
}

/// Warm up strategy using the bb_long_entry CSV and obtain the Long entry signal.
/// Returns the strategy and signal for further exit testing.
async fn warmup_and_get_long_entry() -> (BbMeanRevertV1, auto_trader_core::types::Signal) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_long_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy.on_price(&trigger[0]).await
        .expect("bb_long_entry fixture must produce a Long signal");
    assert_eq!(signal.direction, Direction::Long);
    (strategy, signal)
}

/// Warm up strategy using the bb_short_entry CSV and obtain the Short entry signal.
async fn warmup_and_get_short_entry() -> (BbMeanRevertV1, auto_trader_core::types::Signal) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("bb_short_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy.on_price(&trigger[0]).await
        .expect("bb_short_entry fixture must produce a Short signal");
    assert_eq!(signal.direction, Direction::Short);
    (strategy, signal)
}

// ─── 3.3: Long midline exit ─────────────────────────────────────────────

/// Long position closes when price returns to BB midline (SMA20) after
/// 1R has been reached.
#[tokio::test]
async fn bb_long_midline_exit() {
    let (mut strategy, signal) = warmup_and_get_long_entry().await;

    // Entry is the last close of bb_long_entry.csv = 147.200
    let entry_price = dec!(147.200);
    // SL = entry * (1 - stop_loss_pct)
    let sl_distance = entry_price * signal.stop_loss_pct;
    let stop_loss = entry_price - sl_distance;

    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Long, entry_price, stop_loss);

    // Feed a candle where price is above midline (~150) AND 1R is reached.
    // SMA20 after the entry CSV ≈ 150 (first 20 bars centered around 150).
    // Unrealized = 150.5 - 147.2 = 3.3, SL distance ≈ 147.2 * 0.03 ≈ 4.4 max
    // To ensure 1R: price must be >= entry + sl_distance.
    // Use 152.0 which is well above both midline and 1R threshold.
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(152.000), dec!(152.100), dec!(151.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert_eq!(exits.len(), 1, "expected 1 mean-reached exit for Long");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
    assert_eq!(exits[0].close_price, dec!(152.000));
}

// ─── 3.4: Short midline exit ────────────────────────────────────────────

/// Short position closes when price returns down to BB midline (SMA20)
/// after 1R has been reached.
#[tokio::test]
async fn bb_short_midline_exit() {
    let (mut strategy, signal) = warmup_and_get_short_entry().await;

    // Entry is the last close of bb_short_entry.csv = 152.800
    let entry_price = dec!(152.800);
    let sl_distance = entry_price * signal.stop_loss_pct;
    let stop_loss = entry_price + sl_distance;

    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Short, entry_price, stop_loss);

    // Price drops to 148.0 — well below midline (~150) AND 1R reached.
    // Unrealized = 152.8 - 148.0 = 4.8, SL distance ≈ max 4.584
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(148.000), dec!(148.100), dec!(147.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert_eq!(exits.len(), 1, "expected 1 mean-reached exit for Short");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::MeanReached);
    assert_eq!(exits[0].close_price, dec!(148.000));
}

// ─── 3.5: Long midline not reached ──────────────────────────────────────

/// Price stays below BB midline (SMA20) → no exit for Long.
#[tokio::test]
async fn bb_long_midline_not_reached() {
    let (mut strategy, signal) = warmup_and_get_long_entry().await;

    let entry_price = dec!(147.200);
    let sl_distance = entry_price * signal.stop_loss_pct;
    let stop_loss_price = entry_price - sl_distance;

    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Long, entry_price, stop_loss_price);

    // Price rises slightly but stays below midline (~150).
    // SMA20 ≈ 150; close=148.0 < 150 → midline NOT reached.
    // Unrealized = 148.0 - 147.2 = 0.8, which is also likely < sl_distance, so
    // 1R wouldn't pass either. But the primary test is midline not reached.
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(148.000), dec!(148.100), dec!(147.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert!(
        exits.is_empty(),
        "price below midline → no mean-reached exit, got {} exits",
        exits.len()
    );
}

// ─── 3.6: Short midline not reached ─────────────────────────────────────

/// Price stays above BB midline (SMA20) → no exit for Short.
#[tokio::test]
async fn bb_short_midline_not_reached() {
    let (mut strategy, signal) = warmup_and_get_short_entry().await;

    let entry_price = dec!(152.800);
    let sl_distance = entry_price * signal.stop_loss_pct;
    let stop_loss = entry_price + sl_distance;

    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Short, entry_price, stop_loss);

    // Price drops slightly but stays above midline (~150).
    // close=152.0 > 150 → midline NOT reached.
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(152.000), dec!(152.100), dec!(151.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert!(
        exits.is_empty(),
        "price above midline → no mean-reached exit for Short, got {} exits",
        exits.len()
    );
}

// ─── 3.8: 1R not reached Long ───────────────────────────────────────────

/// Midline condition met but profit < SL distance → exit suppressed by 1R guard.
#[tokio::test]
async fn bb_1r_not_reached_long() {
    let (mut strategy, _signal) = warmup_and_get_long_entry().await;

    // Use a wide SL to make 1R hard to reach: entry 149.0, SL 146.0 → sl_distance=3.0
    // With close at 150.5 (above midline): unrealized = 150.5 - 149.0 = 1.5 < 3.0 → 1R fails.
    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Long, dec!(149.000), dec!(146.000));

    // Price at 150.5 — above midline (~150) but unrealized (1.5) < SL distance (3.0).
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(150.500), dec!(150.600), dec!(150.400), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert!(
        exits.is_empty(),
        "1R not reached → exit suppressed even though midline reached, got {} exits",
        exits.len()
    );
}

// ─── 3.9: 1R not reached Short ──────────────────────────────────────────

/// Midline condition met for Short but profit < SL distance → exit suppressed.
#[tokio::test]
async fn bb_1r_not_reached_short() {
    let (mut strategy, _signal) = warmup_and_get_short_entry().await;

    // Short entry 151.0, SL 154.0 → sl_distance=3.0
    // Close at 149.5 (below midline ~150): unrealized = 151.0 - 149.5 = 1.5 < 3.0 → 1R fails.
    let pos = make_position("bb_mean_revert_v1", PAIR, Direction::Short, dec!(151.000), dec!(154.000));

    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-01T02:10:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_m5_event(PAIR, dec!(149.500), dec!(149.600), dec!(149.400), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &exit_event).await;
    assert!(
        exits.is_empty(),
        "1R not reached → exit suppressed for Short, got {} exits",
        exits.len()
    );
}
