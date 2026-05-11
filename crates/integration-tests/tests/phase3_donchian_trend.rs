//! Phase 3A: Donchian Trend V1 strategy signal + exit tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。
//!
//! Exit tests (3.16-3.19, 3.21-3.22): エントリー CSV でウォームアップし、
//! Position を構築してから on_open_positions でエグジット判定を検証。

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{Strategy, StrategyExitReason};
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Trade, TradeStatus};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::donchian_trend::DonchianTrendV1;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use uuid::Uuid;

const PAIR: &str = "USD_JPY";
const TIMEFRAME: &str = "H1";

fn new_strategy() -> DonchianTrendV1 {
    DonchianTrendV1::new("donchian_trend_v1".to_string(), vec![Pair::new(PAIR)])
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

    assert!(!any_signal, "expected no signal when ATR is zero");
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

// ─── Exit test helpers ───────────────────────────────────────────────────

fn make_h1_event(
    pair: &str,
    close: Decimal,
    high: Decimal,
    low: Decimal,
    ts: DateTime<Utc>,
) -> PriceEvent {
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

/// Warm up with long breakout CSV, get entry signal, then feed more
/// candles at elevated levels to build exit channel history.
async fn warmup_long_with_exit_channel() -> (DonchianTrendV1, Decimal) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_long_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy
        .on_price(&trigger[0])
        .await
        .expect("donchian_long_breakout fixture must produce a Long signal");
    assert_eq!(signal.direction, Direction::Long);
    let entry_price = dec!(152.000);

    // Feed 12 additional bars at elevated prices so exit channel (10-bar low)
    // stabilizes around 153.0 (lows at 152.950).
    for i in 1..=12 {
        let base = chrono::DateTime::parse_from_rfc3339("2026-05-03T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = base + chrono::Duration::hours(i as i64 - 1);
        let _ = strategy
            .on_price(&make_h1_event(
                PAIR,
                dec!(153.500),
                dec!(153.550),
                dec!(153.450),
                ts,
            ))
            .await;
    }

    (strategy, entry_price)
}

/// Warm up with short breakout CSV, get entry signal, then feed more
/// candles at depressed levels to build exit channel history.
async fn warmup_short_with_exit_channel() -> (DonchianTrendV1, Decimal) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("donchian_short_breakout.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy
        .on_price(&trigger[0])
        .await
        .expect("donchian_short_breakout fixture must produce a Short signal");
    assert_eq!(signal.direction, Direction::Short);
    let entry_price = dec!(148.000);

    // Feed 12 additional bars at depressed prices so exit channel (10-bar high)
    // stabilizes around 146.5 (highs at 146.550).
    for i in 1..=12 {
        let base = chrono::DateTime::parse_from_rfc3339("2026-05-03T13:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts = base + chrono::Duration::hours(i as i64 - 1);
        let _ = strategy
            .on_price(&make_h1_event(
                PAIR,
                dec!(146.500),
                dec!(146.550),
                dec!(146.450),
                ts,
            ))
            .await;
    }

    (strategy, entry_price)
}

// ─── 3.16: Long trailing exit ────────────────────────────────────────────

/// Long position exits when close < prior 10-bar low (trailing Donchian channel).
#[tokio::test]
async fn donchian_long_trailing_exit() {
    let (mut strategy, _entry_price) = warmup_long_with_exit_channel().await;

    // Long entry at 152.0, SL at 151.0 → sl_distance=1.0
    // Position already in profit at 153.5 level → unrealized=1.5 >= 1.0 (1R passes).
    // Exit channel 10-bar low ≈ 153.450 (12 bars with low=153.450).
    // Drop close to 153.0 < 153.450 → trailing break fires.
    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Long,
        dec!(152.000),
        dec!(151.000),
    );

    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(153.000), dec!(153.100), dec!(152.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "expected trailing channel exit for Long");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
    assert_eq!(exits[0].close_price, dec!(153.000));
}

// ─── 3.17: Short trailing exit ───────────────────────────────────────────

/// Short position exits when close > prior 10-bar high (trailing Donchian channel).
#[tokio::test]
async fn donchian_short_trailing_exit() {
    let (mut strategy, _entry_price) = warmup_short_with_exit_channel().await;

    // Short entry at 148.0, SL at 149.0 → sl_distance=1.0
    // Position in profit at 146.5 level → unrealized=1.5 >= 1.0 (1R passes).
    // Exit channel 10-bar high ≈ 146.550.
    // Spike close to 147.0 > 146.550 → trailing break fires.
    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Short,
        dec!(148.000),
        dec!(149.000),
    );

    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(147.000), dec!(147.100), dec!(146.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert_eq!(exits.len(), 1, "expected trailing channel exit for Short");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingChannel);
    assert_eq!(exits[0].close_price, dec!(147.000));
}

// ─── 3.18: Long trailing no break ───────────────────────────────────────

/// Price stays above the 10-bar exit channel low → no trailing exit for Long.
#[tokio::test]
async fn donchian_long_trailing_no_break() {
    let (mut strategy, _entry_price) = warmup_long_with_exit_channel().await;

    // Long in profit, 1R reached.
    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Long,
        dec!(152.000),
        dec!(151.000),
    );

    // Close at 153.500 ≥ 10-bar low 153.450 → no trailing break.
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(153.500), dec!(153.600), dec!(153.400), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert!(
        exits.is_empty(),
        "price above 10-bar low → no trailing exit, got {} exits",
        exits.len()
    );
}

// ─── 3.19: Short trailing no break ──────────────────────────────────────

/// Price stays below the 10-bar exit channel high → no trailing exit for Short.
#[tokio::test]
async fn donchian_short_trailing_no_break() {
    let (mut strategy, _entry_price) = warmup_short_with_exit_channel().await;

    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Short,
        dec!(148.000),
        dec!(149.000),
    );

    // Close at 146.500 ≤ 10-bar high 146.550 → no trailing break.
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(146.500), dec!(146.550), dec!(146.450), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert!(
        exits.is_empty(),
        "price below 10-bar high → no trailing exit for Short, got {} exits",
        exits.len()
    );
}

// ─── 3.21: 1R not reached Long ──────────────────────────────────────────

/// Trailing channel broken but unrealized profit < SL distance → 1R guard blocks exit.
#[tokio::test]
async fn donchian_1r_not_reached_long() {
    let (mut strategy, _entry_price) = warmup_long_with_exit_channel().await;

    // Entry 153.0, SL 151.0 → sl_distance=2.0
    // Close 153.0 → unrealized = 0 < 2.0 → 1R not reached.
    // 153.0 < exit_low 153.450 → trailing would fire, but 1R blocks.
    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Long,
        dec!(153.000),
        dec!(151.000),
    );

    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(153.000), dec!(153.100), dec!(152.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert!(
        exits.is_empty(),
        "1R not reached → trailing exit suppressed, got {} exits",
        exits.len()
    );
}

// ─── 3.22: 1R not reached Short ─────────────────────────────────────────

/// Trailing channel broken for Short but unrealized profit < SL distance.
#[tokio::test]
async fn donchian_1r_not_reached_short() {
    let (mut strategy, _entry_price) = warmup_short_with_exit_channel().await;

    // Entry 146.5, SL 148.5 → sl_distance=2.0
    // Close 147.0 → unrealized = 146.5 - 147.0 = -0.5 (loss!) → 1R not reached.
    // 147.0 > exit_high 146.550 → trailing would fire, but 1R blocks.
    let pos = make_position(
        "donchian_trend_v1",
        PAIR,
        Direction::Short,
        dec!(146.500),
        dec!(148.500),
    );

    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-04T01:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let exit_event = make_h1_event(PAIR, dec!(147.000), dec!(147.100), dec!(146.900), ts);
    let _ = strategy.on_price(&exit_event).await;

    let exits = strategy
        .on_open_positions(std::slice::from_ref(&pos), &exit_event)
        .await;
    assert!(
        exits.is_empty(),
        "1R not reached → trailing exit suppressed for Short, got {} exits",
        exits.len()
    );
}
