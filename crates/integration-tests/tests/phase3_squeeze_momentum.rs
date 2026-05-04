//! Phase 3A: Squeeze Momentum V1 strategy signal + exit tests.
//!
//! CSV フィクスチャから PriceEvent を生成し、戦略に直接流して
//! シグナルの発火/非発火を検証する。DB 不要。
//!
//! Exit tests (3.31-3.33, 3.35-3.36): エントリー CSV でウォームアップし、
//! Position を構築してから on_open_positions でエグジット判定を検証。

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::{Strategy, StrategyExitReason};
use auto_trader_core::types::{
    Candle, Direction, Exchange, Pair, Position, Trade, TradeStatus,
};
use auto_trader_integration_tests::helpers::trade_flow::{fixtures_dir, load_events_from_csv};
use auto_trader_strategy::squeeze_momentum::SqueezeMomentumV1;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use uuid::Uuid;

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

// ─── Exit test helpers ────────────────────────────────────��──────────────

fn make_h1_event(pair: &str, close: Decimal, high: Decimal, low: Decimal, ts: DateTime<Utc>) -> PriceEvent {
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
    entry_at: DateTime<Utc>,
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
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
        },
    }
}

/// Warm up with long entry CSV, get signal, then feed additional bars
/// at elevated prices to get past the delay phase and build Chandelier Exit history.
/// Returns (strategy, entry_ts) where entry_ts is the timestamp of the entry bar.
async fn warmup_long_with_chandelier_history() -> (SqueezeMomentumV1, DateTime<Utc>) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_long_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy.on_price(&trigger[0]).await
        .expect("squeeze_long_entry fixture must produce a Long signal");
    assert_eq!(signal.direction, Direction::Long);

    // Entry bar timestamp: 2026-05-03T02:00:00Z (last row of CSV).
    let entry_ts = chrono::DateTime::parse_from_rfc3339("2026-05-03T02:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    // Feed 20 bars of rising prices with small ATR (~0.100 range).
    // Price rises from 152 to 154. This builds Chandelier history:
    // - highest_high(22) ≈ 154.150 (high of last bar)
    // - ATR(14) ≈ 0.1 (dominated by the small range bars)
    // - Chandelier stop = 154.15 - 0.1 * 3 = 153.85
    // For 1R: entry=151.5, SL=151.0, sl_distance=0.5
    // At close=153.5: unrealized=2.0 >= 0.5 → 1R passes.
    // 153.5 < 153.85 → Chandelier fires!
    for i in 1..=20 {
        let ts = entry_ts + Duration::hours(i);
        let p = dec!(152.000) + Decimal::from(i) * dec!(0.100);
        let _ = strategy.on_price(&make_h1_event(
            PAIR,
            p,
            p + dec!(0.050),
            p - dec!(0.050),
            ts,
        )).await;
    }

    (strategy, entry_ts)
}

/// Same for short entry.
async fn warmup_short_with_chandelier_history() -> (SqueezeMomentumV1, DateTime<Utc>) {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_short_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let signal = strategy.on_price(&trigger[0]).await
        .expect("squeeze_short_entry fixture must produce a Short signal");
    assert_eq!(signal.direction, Direction::Short);

    let entry_ts = chrono::DateTime::parse_from_rfc3339("2026-05-03T02:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    // Feed 20 bars of falling prices with small ATR (~0.100 range).
    // Price falls from 148 to 146. Chandelier history:
    // - lowest_low(22) ≈ 145.850 (low of last bar)
    // - ATR(14) ≈ 0.1
    // - Chandelier stop (short) = 145.85 + 0.1 * 3 = 146.15
    // For 1R: entry=148.5, SL=149.0, sl_distance=0.5
    // At close=146.5: unrealized=2.0 >= 0.5 → 1R passes.
    // 146.5 > 146.15 → Chandelier fires!
    for i in 1..=20 {
        let ts = entry_ts + Duration::hours(i);
        let p = dec!(148.000) - Decimal::from(i) * dec!(0.100);
        let _ = strategy.on_price(&make_h1_event(
            PAIR,
            p,
            p + dec!(0.050),
            p - dec!(0.050),
            ts,
        )).await;
    }

    (strategy, entry_ts)
}

// ─── 3.31: Long Chandelier exit ──────────────────────────────────────────

/// Long position exits when close drops below Chandelier stop
/// (highest_high(22) - ATR(14)*3) after delay phase.
#[tokio::test]
async fn squeeze_long_chandelier_exit() {
    let (mut strategy, entry_ts) = warmup_long_with_chandelier_history().await;

    // Position entered at entry_ts. After 20 extra bars (well past DELAY_BARS=3).
    // Entry 151.500, SL 151.000 → sl_distance=0.500.
    // At close=153.500: unrealized=2.0 >= 0.5 → 1R passes.
    // highest_high(22) ≈ 154.05, ATR ≈ 0.1, stop ≈ 153.75.
    // 153.500 < stop → Chandelier fires.
    let pos = make_position(
        "squeeze_momentum_v1", PAIR, Direction::Long,
        dec!(151.500), dec!(151.000), entry_ts,
    );

    let drop_ts = entry_ts + Duration::hours(21);
    let drop = make_h1_event(PAIR, dec!(153.500), dec!(153.550), dec!(153.450), drop_ts);
    let _ = strategy.on_price(&drop).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(exits.len(), 1, "expected Chandelier exit for Long");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
    assert_eq!(exits[0].close_price, dec!(153.500));
}

// ─── 3.32: Short Chandelier exit ─────────────────────────────────────────

/// Short position exits when close rises above Chandelier stop
/// (lowest_low(22) + ATR(14)*3) after delay phase.
#[tokio::test]
async fn squeeze_short_chandelier_exit() {
    let (mut strategy, entry_ts) = warmup_short_with_chandelier_history().await;

    // Entry 148.500, SL 149.000 → sl_distance=0.500.
    // At close=146.500: unrealized=2.0 >= 0.5 → 1R passes.
    // lowest_low(22) ≈ 145.95, ATR ≈ 0.1, stop ≈ 146.25.
    // 146.500 > 146.25 → Chandelier fires.
    let pos = make_position(
        "squeeze_momentum_v1", PAIR, Direction::Short,
        dec!(148.500), dec!(149.000), entry_ts,
    );

    let spike_ts = entry_ts + Duration::hours(21);
    let spike = make_h1_event(PAIR, dec!(146.500), dec!(146.550), dec!(146.450), spike_ts);
    let _ = strategy.on_price(&spike).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &spike).await;
    assert_eq!(exits.len(), 1, "expected Chandelier exit for Short");
    assert_eq!(exits[0].trade_id, pos.trade.id);
    assert_eq!(exits[0].reason, StrategyExitReason::TrailingMa);
    assert_eq!(exits[0].close_price, dec!(146.500));
}

// ─── 3.33: Delay phase suppression ──────────────────────────────────────

/// During the first DELAY_BARS=3 bars after entry, the Chandelier exit
/// must NOT fire even if the stop level is breached.
#[tokio::test]
async fn squeeze_delay_phase_suppression() {
    let mut strategy = new_strategy();
    let events = load_events_from_csv(
        &fixtures_dir().join("squeeze_long_entry.csv"),
        Exchange::GmoFx,
        PAIR,
        TIMEFRAME,
    );
    let (warmup, trigger) = events.split_at(events.len() - 1);
    strategy.warmup(warmup).await;
    let _signal = strategy.on_price(&trigger[0]).await
        .expect("fixture must produce signal");

    let entry_ts = chrono::DateTime::parse_from_rfc3339("2026-05-03T02:00:00Z")
        .unwrap()
        .with_timezone(&Utc);

    // Position entered at the breakout bar.
    let pos = make_position(
        "squeeze_momentum_v1", PAIR, Direction::Long,
        dec!(151.500), dec!(150.000), entry_ts,
    );

    // Feed only 2 bars after entry (within DELAY_BARS=3).
    // Even though price drops dramatically, the delay phase should suppress exit.
    for i in 1..=2 {
        let ts = entry_ts + Duration::hours(i);
        let _ = strategy.on_price(&make_h1_event(
            PAIR,
            dec!(152.000),
            dec!(152.500),
            dec!(151.500),
            ts,
        )).await;
    }

    // This event is bar 3 after entry (bars_held=2 < DELAY_BARS=3 → still in delay).
    let drop_ts = entry_ts + Duration::hours(3);
    let drop = make_h1_event(PAIR, dec!(140.000), dec!(140.100), dec!(139.900), drop_ts);
    let _ = strategy.on_price(&drop).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert_eq!(
        exits.len(), 0,
        "delay phase (bars_held < 3) should suppress Chandelier exit"
    );
}

// ─── 3.35: 1R not reached Long ──────────────────────────────────────────

/// Chandelier stop breached and delay phase over, but unrealized profit < SL distance.
#[tokio::test]
async fn squeeze_1r_not_reached_long() {
    let (mut strategy, entry_ts) = warmup_long_with_chandelier_history().await;

    // Entry 153.500, SL 151.500 → sl_distance=2.000.
    // Close 153.600 → unrealized = 0.100 < 2.000 → 1R not reached.
    // Even though Chandelier stop might be breached, 1R guard blocks.
    let pos = make_position(
        "squeeze_momentum_v1", PAIR, Direction::Long,
        dec!(153.500), dec!(151.500), entry_ts,
    );

    // Price slightly above entry — tiny unrealized profit < sl_distance.
    let drop_ts = entry_ts + Duration::hours(21);
    let drop = make_h1_event(PAIR, dec!(153.600), dec!(153.650), dec!(153.550), drop_ts);
    let _ = strategy.on_price(&drop).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &drop).await;
    assert!(
        exits.is_empty(),
        "1R not reached → Chandelier exit suppressed, got {} exits",
        exits.len()
    );
}

// ─── 3.36: 1R not reached Short ��────────────────────────────────────────

/// Chandelier stop breached for Short but unrealized profit < SL distance.
#[tokio::test]
async fn squeeze_1r_not_reached_short() {
    let (mut strategy, entry_ts) = warmup_short_with_chandelier_history().await;

    // Entry 146.500, SL 148.500 → sl_distance=2.000.
    // Close 146.400 → unrealized = 146.500 - 146.400 = 0.100 < 2.000 → 1R not reached.
    let pos = make_position(
        "squeeze_momentum_v1", PAIR, Direction::Short,
        dec!(146.500), dec!(148.500), entry_ts,
    );

    // Price slightly below entry — tiny unrealized profit < sl_distance.
    let spike_ts = entry_ts + Duration::hours(21);
    let spike = make_h1_event(PAIR, dec!(146.400), dec!(146.450), dec!(146.350), spike_ts);
    let _ = strategy.on_price(&spike).await;

    let exits = strategy.on_open_positions(std::slice::from_ref(&pos), &spike).await;
    assert!(
        exits.is_empty(),
        "1R not reached → Chandelier exit suppressed for Short, got {} exits",
        exits.len()
    );
}
