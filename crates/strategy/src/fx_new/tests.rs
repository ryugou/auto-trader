use auto_trader_core::types::{Exchange, ExitReason, Trade, TradeStatus};
use chrono::{Duration, Utc};
use uuid::Uuid;

use super::*;

#[test]
fn one_percent_risk_sizing_includes_leverage() {
    assert_eq!(FxNewV1::allocation_for(dec!(0.01)), dec!(0.1));
    assert_eq!(FxNewV1::allocation_for(dec!(0.02)), dec!(0.05));
}

fn candle(timestamp: chrono::DateTime<Utc>, high: Decimal, low: Decimal) -> Candle {
    Candle {
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        timeframe: "M15".to_string(),
        open: dec!(100),
        high,
        low,
        close: dec!(100),
        volume: Some(0),
        best_bid: None,
        best_ask: None,
        timestamp,
    }
}

fn long_position(entry_at: chrono::DateTime<Utc>) -> Position {
    Position {
        trade: Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "fx_new_v1".to_string(),
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::GmoFx,
            direction: Direction::Long,
            entry_price: dec!(100),
            exit_price: None,
            stop_loss: dec!(99),
            take_profit: None,
            quantity: dec!(1),
            leverage: dec!(10),
            fees: Decimal::ZERO,
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None::<ExitReason>,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: None,
        },
    }
}

fn short_position(entry_at: chrono::DateTime<Utc>) -> Position {
    Position {
        trade: Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "fx_new_v1".to_string(),
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::GmoFx,
            direction: Direction::Short,
            entry_price: dec!(100),
            exit_price: None,
            stop_loss: dec!(101),
            take_profit: None,
            quantity: dec!(1),
            leverage: dec!(10),
            fees: Decimal::ZERO,
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None::<ExitReason>,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: None,
        },
    }
}

#[test]
fn one_r_gate_remains_active_after_price_retraces() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([
        candle(entry_at + Duration::minutes(15), dec!(101.2), dec!(100)),
        candle(entry_at + Duration::minutes(30), dec!(100.7), dec!(100.2)),
    ]);
    let current = candle(entry_at + Duration::minutes(45), dec!(100.5), dec!(99.8));

    assert!(FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_ignores_bar_fully_completed_before_entry() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(20),
        dec!(101.2),
        dec!(99.8),
    )]);
    let current = candle(entry_at + Duration::minutes(5), dec!(100.8), dec!(99.9));

    assert!(!FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_includes_bar_covering_entry_timestamp() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(5),
        dec!(101.2),
        dec!(99.8),
    )]);
    let current = candle(entry_at + Duration::minutes(10), dec!(100.1), dec!(99.9));

    assert!(FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_includes_bar_covering_entry_timestamp_for_short() {
    let entry_at = Utc::now();
    let position = short_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(5),
        dec!(100.2),
        dec!(98.8),
    )]);
    let current = candle(entry_at + Duration::minutes(10), dec!(99.9), dec!(99.1));

    assert!(FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_excludes_bar_whose_period_ends_exactly_at_entry_at() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(15),
        dec!(101.2),
        dec!(99.8),
    )]);
    let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

    assert!(!FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_includes_bar_whose_period_ends_one_second_after_entry_at() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(15) + Duration::seconds(1),
        dec!(101.2),
        dec!(99.8),
    )]);
    let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

    assert!(FxNewV1::reached_one_r(&position, &history, &current));
}

#[test]
fn one_r_gate_excludes_bar_whose_period_ends_one_second_before_entry_at() {
    let entry_at = Utc::now();
    let position = long_position(entry_at);
    let history = VecDeque::from([candle(
        entry_at - Duration::minutes(15) - Duration::seconds(1),
        dec!(101.2),
        dec!(99.8),
    )]);
    let current = candle(entry_at + Duration::minutes(5), dec!(100.5), dec!(99.9));

    assert!(!FxNewV1::reached_one_r(&position, &history, &current));
}

fn m15_bucket_aligned_base_time() -> chrono::DateTime<Utc> {
    use chrono::TimeZone;
    Utc.with_ymd_and_hms(2026, 1, 5, 0, 0, 0).unwrap()
}

fn price_event_with_exchange(
    pair: &str,
    exchange: Exchange,
    timeframe: &str,
    timestamp: chrono::DateTime<Utc>,
    ohlc: (Decimal, Decimal, Decimal, Decimal),
    volume: u64,
) -> PriceEvent {
    let (open, high, low, close) = ohlc;
    PriceEvent {
        pair: Pair::new(pair),
        exchange,
        candle: Candle {
            pair: Pair::new(pair),
            exchange,
            timeframe: timeframe.to_string(),
            open,
            high,
            low,
            close,
            volume: Some(volume),
            best_bid: None,
            best_ask: None,
            timestamp,
        },
        indicators: HashMap::new(),
        timestamp,
    }
}

fn price_event(
    pair: &str,
    timeframe: &str,
    timestamp: chrono::DateTime<Utc>,
    ohlc: (Decimal, Decimal, Decimal, Decimal),
    volume: u64,
) -> PriceEvent {
    price_event_with_exchange(pair, Exchange::GmoFx, timeframe, timestamp, ohlc, volume)
}

fn m5(minute: i64, ohlc: (Decimal, Decimal, Decimal, Decimal), volume: u64) -> PriceEvent {
    price_event(
        "USD_JPY",
        "M5",
        m15_bucket_aligned_base_time() + Duration::minutes(minute),
        ohlc,
        volume,
    )
}

fn strategy() -> FxNewV1 {
    FxNewV1::new("fx_new_v1".to_string(), vec![Pair::new("USD_JPY")])
}

#[test]
fn aggregate_m15_combines_three_m5_bars_into_one_bucket_candle() {
    let mut s = strategy();

    assert!(
        s.aggregate_m15(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
            .is_none()
    );
    assert!(
        s.aggregate_m15(&m5(5, (dec!(100.5), dec!(103), dec!(100), dec!(102)), 20))
            .is_none()
    );
    let m15 = s
        .aggregate_m15(&m5(10, (dec!(102), dec!(102.5), dec!(98), dec!(101)), 30))
        .expect("third M5 bar completes the M15 candle");

    assert_eq!(m15.timeframe, "M15");
    assert_eq!(m15.timestamp, m15_bucket_aligned_base_time());
    assert_eq!(m15.open, dec!(100));
    assert_eq!(m15.close, dec!(101));
    assert_eq!(m15.high, dec!(103));
    assert_eq!(m15.low, dec!(98));
    assert_eq!(m15.volume, Some(60));
}

#[test]
fn aggregate_m15_ignores_duplicate_m5_timestamp() {
    let mut s = strategy();
    let bar = (dec!(100), dec!(101), dec!(99), dec!(100));

    assert!(s.aggregate_m15(&m5(0, bar, 1)).is_none());
    assert!(s.aggregate_m15(&m5(0, bar, 1)).is_none());
    assert!(
        s.aggregate_m15(&m5(5, bar, 1)).is_none(),
        "duplicate 00:00 must not count as a third bar"
    );
}

#[test]
fn aggregate_m15_discards_incomplete_previous_bucket() {
    let mut s = strategy();

    assert!(
        s.aggregate_m15(&m5(5, (dec!(100), dec!(200), dec!(50), dec!(100)), 1000))
            .is_none()
    );
    assert!(
        s.aggregate_m15(&m5(10, (dec!(100), dec!(200), dec!(50), dec!(100)), 1000))
            .is_none()
    );

    let ohlc = (dec!(100), dec!(101), dec!(99), dec!(100));
    assert!(s.aggregate_m15(&m5(15, ohlc, 1)).is_none());
    assert!(s.aggregate_m15(&m5(20, ohlc, 1)).is_none());
    let m15 = s
        .aggregate_m15(&m5(25, ohlc, 1))
        .expect("00:15 bucket completes");

    assert_eq!(
        m15.timestamp,
        m15_bucket_aligned_base_time() + Duration::minutes(15)
    );
    assert_eq!(m15.high, dec!(101));
    assert_eq!(m15.low, dec!(99));
    assert_eq!(m15.volume, Some(3));
}

#[test]
fn allocation_is_clipped_to_one_for_tiny_stop_loss() {
    assert_eq!(FxNewV1::allocation_for(dec!(0.0005)), Decimal::ONE);
}

fn push_h1_closes(s: &mut FxNewV1, closes: impl IntoIterator<Item = Decimal>) {
    for (i, close) in closes.into_iter().enumerate() {
        let event = price_event(
            "USD_JPY",
            "H1",
            m15_bucket_aligned_base_time() + Duration::hours(i as i64),
            (close, close, close, close),
            0,
        );
        s.push_h1(&event);
    }
}

#[test]
fn h1_direction_is_none_with_fewer_than_fifty_bars() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=49).map(Decimal::from));

    assert_eq!(s.h1_direction("USD_JPY"), None);
}

#[test]
fn h1_direction_is_long_when_fast_sma_above_slow_sma() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).map(Decimal::from));

    assert_eq!(s.h1_direction("USD_JPY"), Some(Direction::Long));
}

#[test]
fn h1_direction_is_short_when_fast_sma_below_slow_sma() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));

    assert_eq!(s.h1_direction("USD_JPY"), Some(Direction::Short));
}

fn oanda_m5_bars() -> [(i64, (Decimal, Decimal, Decimal, Decimal)); 3] {
    [
        (0_i64, (dec!(100), dec!(101), dec!(99), dec!(100.5))),
        (5, (dec!(100.5), dec!(103), dec!(100), dec!(102))),
        (10, (dec!(102), dec!(102.5), dec!(98), dec!(101))),
    ]
}

fn make_oanda_m5(minute: i64, ohlc: (Decimal, Decimal, Decimal, Decimal)) -> PriceEvent {
    price_event_with_exchange(
        "USD_JPY",
        Exchange::Oanda,
        "M5",
        m15_bucket_aligned_base_time() + Duration::minutes(minute),
        ohlc,
        1,
    )
}

fn oanda_h1() -> PriceEvent {
    price_event_with_exchange(
        "USD_JPY",
        Exchange::Oanda,
        "H1",
        m15_bucket_aligned_base_time(),
        (dec!(100), dec!(100), dec!(100), dec!(100)),
        0,
    )
}

#[tokio::test]
async fn on_price_ignores_oanda_events_for_a_pair_the_strategy_also_trades_on_gmo_fx() {
    let mut s = strategy();
    for (minute, ohlc) in oanda_m5_bars() {
        assert!(s.on_price(&make_oanda_m5(minute, ohlc)).await.is_none());
    }
    assert!(s.on_price(&oanda_h1()).await.is_none());
    assert!(
        s.m5_pending.is_empty(),
        "OANDA M5 bars must not be buffered by the GMO-FX-only strategy"
    );
    assert!(
        s.h1.is_empty(),
        "OANDA H1 bar must not be stored by the GMO-FX-only strategy"
    );
    assert!(
        s.m15.is_empty(),
        "OANDA M5 bars must not complete an M15 candle for the GMO-FX-only strategy"
    );
}

#[tokio::test]
async fn warmup_ignores_oanda_events_for_a_pair_the_strategy_also_trades_on_gmo_fx() {
    let mut warmup_events: Vec<PriceEvent> = oanda_m5_bars()
        .iter()
        .map(|(minute, ohlc)| make_oanda_m5(*minute, *ohlc))
        .collect();
    warmup_events.push(oanda_h1());
    let mut s = strategy();
    s.warmup(&warmup_events).await;
    assert!(
        s.m5_pending.is_empty(),
        "OANDA M5 bars must not be buffered during warmup"
    );
    assert!(
        s.h1.is_empty(),
        "OANDA H1 bar must not be stored during warmup"
    );
    assert!(
        s.m15.is_empty(),
        "OANDA M5 bars must not complete an M15 candle during warmup"
    );
}

#[tokio::test]
async fn on_price_buffers_gmo_fx_events_for_the_same_pair() {
    let mut s = strategy();
    let bars = oanda_m5_bars();
    let (first_minute, first_ohlc) = bars[0];
    let gmo_m5_first = price_event(
        "USD_JPY",
        "M5",
        m15_bucket_aligned_base_time() + Duration::minutes(first_minute),
        first_ohlc,
        1,
    );
    assert!(s.on_price(&gmo_m5_first).await.is_none());
    assert_eq!(
        s.m5_pending.get("USD_JPY").map(Vec::len),
        Some(1),
        "GMO FX M5 bar must be buffered"
    );

    let gmo_h1 = price_event(
        "USD_JPY",
        "H1",
        m15_bucket_aligned_base_time(),
        (dec!(100), dec!(100), dec!(100), dec!(100)),
        0,
    );
    assert!(s.on_price(&gmo_h1).await.is_none());
    assert_eq!(
        s.h1.get("USD_JPY").map(VecDeque::len),
        Some(1),
        "GMO FX H1 bar must be stored"
    );

    for (minute, ohlc) in &bars[1..] {
        let event = price_event(
            "USD_JPY",
            "M5",
            m15_bucket_aligned_base_time() + Duration::minutes(*minute),
            *ohlc,
            1,
        );
        s.on_price(&event).await;
    }
    assert_eq!(
        s.m15.get("USD_JPY").map(VecDeque::len),
        Some(1),
        "GMO FX M5 bars must complete an M15 candle"
    );
}

#[tokio::test]
async fn on_price_ignores_other_pairs() {
    let mut s = strategy();
    let event = price_event(
        "EUR_USD",
        "M5",
        m15_bucket_aligned_base_time(),
        (dec!(1), dec!(1), dec!(1), dec!(1)),
        0,
    );

    assert!(s.on_price(&event).await.is_none());
    assert!(s.m5_pending.is_empty(), "other pair must not be buffered");
}

#[tokio::test]
async fn on_price_ignores_timeframes_other_than_m5_and_h1() {
    let mut s = strategy();
    for timeframe in ["M1", "M15", "D1"] {
        let event = price_event(
            "USD_JPY",
            timeframe,
            m15_bucket_aligned_base_time(),
            (dec!(100), dec!(100), dec!(100), dec!(100)),
            0,
        );
        assert!(s.on_price(&event).await.is_none(), "{timeframe}");
    }
    assert!(s.m5_pending.is_empty(), "non-M5 must not be buffered");
    assert!(s.h1.is_empty(), "non-H1 must not be stored as H1");
}

#[tokio::test]
async fn on_price_rejects_zero_close_without_panicking() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));

    let calm: VecDeque<Candle> = (1..=50)
        .map(|i| {
            candle(
                m15_bucket_aligned_base_time() - Duration::minutes(15 * (51 - i)),
                dec!(100.1),
                dec!(99.9),
            )
        })
        .collect();
    s.m15.insert("USD_JPY".to_string(), calm);

    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(1000), dec!(100), dec!(100)), 1))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(100), dec!(100), dec!(1), dec!(100)), 1))
            .await
            .is_none()
    );

    let signal = s
        .on_price(&m5(10, (dec!(100), dec!(100), dec!(100), dec!(0)), 1))
        .await;

    assert!(
        signal.is_none(),
        "non-positive close must not produce a signal"
    );
}

#[tokio::test]
async fn on_price_excludes_m5_bar_with_zero_open_from_aggregation() {
    let mut s = strategy();
    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(0), dec!(101), dec!(99), dec!(100.2)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10))
            .await
            .is_none()
    );

    assert_eq!(
        s.m5_pending.get("USD_JPY").map(Vec::len),
        Some(2),
        "the invalid middle M5 bar must not be buffered for aggregation"
    );
    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "a bucket missing its invalid middle M5 bar must never complete into an M15 candle"
    );
}

#[tokio::test]
async fn on_price_excludes_m5_bar_with_negative_close_from_aggregation() {
    let mut s = strategy();
    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(-1)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10))
            .await
            .is_none()
    );

    assert_eq!(
        s.m5_pending.get("USD_JPY").map(Vec::len),
        Some(2),
        "the invalid middle M5 bar must not be buffered for aggregation"
    );
    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "a bucket missing its invalid middle M5 bar must never complete into an M15 candle"
    );
}

#[tokio::test]
async fn warmup_excludes_m5_bar_with_zero_open_from_aggregation() {
    let mut s = strategy();
    let events = vec![
        m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
        m5(5, (dec!(0), dec!(101), dec!(99), dec!(100.2)), 10),
        m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10),
    ];

    s.warmup(&events).await;

    assert_eq!(
        s.m5_pending.get("USD_JPY").map(Vec::len),
        Some(2),
        "the invalid middle M5 bar must not be buffered for aggregation during warmup"
    );
    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "a bucket missing its invalid middle M5 bar must never complete into an M15 candle during warmup"
    );
}

#[tokio::test]
async fn warmup_excludes_m5_bar_with_negative_close_from_aggregation() {
    let mut s = strategy();
    let events = vec![
        m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
        m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(-1)), 10),
        m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(100)), 10),
    ];

    s.warmup(&events).await;

    assert_eq!(
        s.m5_pending.get("USD_JPY").map(Vec::len),
        Some(2),
        "the invalid middle M5 bar must not be buffered for aggregation during warmup"
    );
    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "a bucket missing its invalid middle M5 bar must never complete into an M15 candle during warmup"
    );
}

#[tokio::test]
async fn warmup_discards_m15_bucket_when_aggregated_close_is_zero() {
    let mut s = strategy();
    let events = vec![
        m5(0, (dec!(100), dec!(101), dec!(99), dec!(100.5)), 10),
        m5(5, (dec!(100.5), dec!(101), dec!(99), dec!(100.2)), 10),
        m5(10, (dec!(100.2), dec!(101), dec!(99), dec!(0)), 10),
    ];

    s.warmup(&events).await;

    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "an M15 bucket whose aggregated close is non-positive must not enter history"
    );
}

#[tokio::test]
async fn warmup_discards_m15_bucket_when_aggregated_high_is_zero() {
    let mut s = strategy();
    let events = vec![
        m5(0, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
        m5(5, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
        m5(10, (dec!(100), dec!(0), dec!(99), dec!(100)), 10),
    ];

    s.warmup(&events).await;

    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "an M15 bucket whose aggregated high is non-positive must not enter history"
    );
}

#[tokio::test]
async fn warmup_discards_m15_bucket_when_aggregated_low_is_negative() {
    let mut s = strategy();
    let events = vec![
        m5(0, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
        m5(5, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
        m5(10, (dec!(100), dec!(101), dec!(-1), dec!(100)), 10),
    ];

    s.warmup(&events).await;

    assert!(
        s.m15.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "an M15 bucket whose aggregated low is negative must not enter history"
    );
}

#[tokio::test]
async fn warmup_discards_h1_bar_with_zero_close() {
    let mut s = strategy();
    let event = price_event(
        "USD_JPY",
        "H1",
        m15_bucket_aligned_base_time(),
        (dec!(100), dec!(101), dec!(99), dec!(0)),
        0,
    );

    s.warmup(&[event]).await;

    assert!(
        s.h1.get("USD_JPY").is_none_or(VecDeque::is_empty),
        "an H1 bar with a non-positive close must not enter history"
    );
}

fn history_after_entry(entry_at: chrono::DateTime<Utc>, peak_high: Decimal) -> VecDeque<Candle> {
    (1..=10)
        .map(|i| {
            let high = if i == 1 { peak_high } else { dec!(100.5) };
            candle(entry_at + Duration::minutes(15 * i), high, dec!(99.5))
        })
        .collect()
}

#[tokio::test]
async fn on_open_positions_holds_before_one_r_even_below_exit_channel() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = long_position(entry_at);
    s.m15.insert(
        "USD_JPY".to_string(),
        history_after_entry(entry_at, dec!(100.5)),
    );
    let event = price_event(
        "USD_JPY",
        "M5",
        entry_at + Duration::minutes(15 * 11),
        (dec!(99.8), dec!(100.2), dec!(99), dec!(99)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert!(exits.is_empty(), "1R not reached: trailing exit is gated");
}

#[tokio::test]
async fn on_open_positions_exits_below_channel_once_one_r_reached() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = long_position(entry_at);
    s.m15.insert(
        "USD_JPY".to_string(),
        history_after_entry(entry_at, dec!(101)),
    );
    let event = price_event(
        "USD_JPY",
        "M5",
        entry_at + Duration::minutes(15 * 11),
        (dec!(99.8), dec!(100.2), dec!(99), dec!(99)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert_eq!(exits.len(), 1);
    assert_eq!(exits[0].trade_id, position.trade.id);
    assert_eq!(exits[0].close_price, dec!(99));
}

#[tokio::test]
async fn on_open_positions_suppresses_exit_when_current_candle_low_is_non_positive() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = long_position(entry_at);
    s.m15.insert(
        "USD_JPY".to_string(),
        history_after_entry(entry_at, dec!(101)),
    );
    let event = price_event(
        "USD_JPY",
        "M5",
        entry_at + Duration::minutes(15 * 11),
        (dec!(99.8), dec!(100.2), dec!(0), dec!(99)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert!(
        exits.is_empty(),
        "a non-positive OHLC value on the current candle must suppress exit evaluation"
    );
}

#[tokio::test]
async fn on_open_positions_uses_exit_channel_updated_by_latest_completed_bar_long() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = long_position(entry_at);
    let history: VecDeque<Candle> = (1..=9)
        .map(|i| {
            let (high, low) = match i {
                1 => (dec!(101.2), dec!(99.5)),
                9 => (dec!(100.5), dec!(98.0)),
                _ => (dec!(100.5), dec!(99.5)),
            };
            candle(entry_at + Duration::minutes(15 * i), high, low)
        })
        .collect();
    s.m15.insert("USD_JPY".to_string(), history);
    let event = price_event(
        "USD_JPY",
        "M5",
        entry_at + Duration::minutes(15 * 10),
        (dec!(99.2), dec!(99.5), dec!(98.5), dec!(99.0)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert!(
        exits.is_empty(),
        "close 99.0 is above the updated exit_low 98.0; the stale \
         pre-fix channel (exit_low 99.5, ignoring the latest completed \
         bar) would have exited here"
    );
}

fn history_with_bucket_completed_by_event(
    entry_at: chrono::DateTime<Utc>,
    older_bar: impl Fn(i64) -> (Decimal, Decimal),
    completed_bucket: (Decimal, Decimal),
) -> (VecDeque<Candle>, chrono::DateTime<Utc>) {
    let mut history: VecDeque<Candle> = (1..=9)
        .map(|i| {
            let (high, low) = older_bar(i);
            candle(entry_at + Duration::minutes(15 * i), high, low)
        })
        .collect();
    let bucket_start = entry_at + Duration::minutes(15 * 10);
    history.push_back(candle(bucket_start, completed_bucket.0, completed_bucket.1));
    (history, bucket_start + Duration::minutes(10))
}

#[tokio::test]
async fn on_open_positions_exits_long_on_tick_that_completes_m15_bar() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = long_position(entry_at);
    let (history, event_ts) = history_with_bucket_completed_by_event(
        entry_at,
        |i| {
            if i == 1 {
                (dec!(101.2), dec!(99.5))
            } else {
                (dec!(100.5), dec!(99.5))
            }
        },
        (dec!(100.5), dec!(98.8)),
    );
    s.m15.insert("USD_JPY".to_string(), history);
    let event = price_event(
        "USD_JPY",
        "M5",
        event_ts,
        (dec!(99.2), dec!(99.4), dec!(98.8), dec!(99.0)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert_eq!(
        exits.len(),
        1,
        "close 99.0 is below the 8 prior completed bars' low (99.5); the \
         bucket containing the current M5 (low 98.8) must not widen the channel"
    );
}

#[tokio::test]
async fn on_open_positions_exits_short_on_tick_that_completes_m15_bar() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = short_position(entry_at);
    let (history, event_ts) = history_with_bucket_completed_by_event(
        entry_at,
        |i| {
            if i == 1 {
                (dec!(100.5), dec!(98.8))
            } else {
                (dec!(100.5), dec!(99.5))
            }
        },
        (dec!(101.2), dec!(99.5)),
    );
    s.m15.insert("USD_JPY".to_string(), history);
    let event = price_event(
        "USD_JPY",
        "M5",
        event_ts,
        (dec!(100.8), dec!(101.2), dec!(100.6), dec!(101.0)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert_eq!(
        exits.len(),
        1,
        "close 101.0 is above the 8 prior completed bars' high (100.5); the \
         bucket containing the current M5 (high 101.2) must not widen the channel"
    );
}

#[tokio::test]
async fn on_open_positions_uses_exit_channel_updated_by_latest_completed_bar_short() {
    let mut s = strategy();
    let entry_at = m15_bucket_aligned_base_time();
    let position = short_position(entry_at);
    let history: VecDeque<Candle> = (1..=9)
        .map(|i| {
            let (high, low) = match i {
                1 => (dec!(100.5), dec!(98.8)),
                9 => (dec!(102.0), dec!(99.5)),
                _ => (dec!(100.5), dec!(99.5)),
            };
            candle(entry_at + Duration::minutes(15 * i), high, low)
        })
        .collect();
    s.m15.insert("USD_JPY".to_string(), history);
    let event = price_event(
        "USD_JPY",
        "M5",
        entry_at + Duration::minutes(15 * 10),
        (dec!(100.8), dec!(101.5), dec!(100.5), dec!(101.0)),
        0,
    );

    let exits = s
        .on_open_positions(std::slice::from_ref(&position), &event)
        .await;

    assert!(
        exits.is_empty(),
        "close 101.0 is below the updated exit_high 102.0; the stale \
         pre-fix channel (exit_high 100.5, ignoring the latest \
         completed bar) would have exited here"
    );
}

const CALM_M15_TRUE_RANGE: Decimal = dec!(0.2);

fn calm_m15_history_with_constant_true_range() -> VecDeque<Candle> {
    (1..=50)
        .map(|i| {
            candle(
                m15_bucket_aligned_base_time() - Duration::minutes(15 * (51 - i)),
                dec!(100.1),
                dec!(99.9),
            )
        })
        .collect()
}

#[tokio::test]
async fn on_price_emits_long_signal_on_breakout_with_h1_uptrend_and_atr_expansion() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).map(Decimal::from));
    s.m15.insert(
        "USD_JPY".to_string(),
        calm_m15_history_with_constant_true_range(),
    );

    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(103), dec!(99.9), dec!(100.5)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(100.5), dec!(103), dec!(99.8), dec!(101)), 10))
            .await
            .is_none()
    );
    let signal = s
        .on_price(&m5(10, (dec!(101), dec!(103), dec!(99.8), dec!(101.8)), 10))
        .await
        .expect("upward breakout + H1 uptrend + ATR expansion must emit a signal");

    assert_eq!(signal.direction, Direction::Long);
    let period = Decimal::from(ATR_PERIOD);
    let expected_atr = (CALM_M15_TRUE_RANGE * (period - Decimal::ONE) + dec!(3.2)) / period;
    let expected_stop_loss_pct = (expected_atr * ATR_MULT / dec!(101.8)).min(SL_CAP);
    assert!(signal.stop_loss_pct > Decimal::ZERO && signal.stop_loss_pct <= SL_CAP);
    assert_eq!(signal.stop_loss_pct, expected_stop_loss_pct);
    assert_eq!(
        signal.allocation_pct,
        FxNewV1::allocation_for(signal.stop_loss_pct)
    );
}

#[tokio::test]
async fn on_price_emits_short_signal_on_breakdown_with_h1_downtrend_and_atr_expansion() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));
    s.m15.insert(
        "USD_JPY".to_string(),
        calm_m15_history_with_constant_true_range(),
    );

    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(100.2), dec!(97), dec!(99.5)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(99.5), dec!(100.2), dec!(96.8), dec!(99)), 10))
            .await
            .is_none()
    );
    let signal = s
        .on_price(&m5(10, (dec!(99), dec!(100.2), dec!(96.8), dec!(98.2)), 10))
        .await
        .expect("downward breakout + H1 downtrend + ATR expansion must emit a signal");

    assert_eq!(signal.direction, Direction::Short);
    let period = Decimal::from(ATR_PERIOD);
    let expected_atr = (CALM_M15_TRUE_RANGE * (period - Decimal::ONE) + dec!(3.4)) / period;
    let expected_stop_loss_pct = (expected_atr * ATR_MULT / dec!(98.2)).min(SL_CAP);
    assert!(signal.stop_loss_pct > Decimal::ZERO && signal.stop_loss_pct <= SL_CAP);
    assert_eq!(signal.stop_loss_pct, expected_stop_loss_pct);
    assert_eq!(
        signal.allocation_pct,
        FxNewV1::allocation_for(signal.stop_loss_pct)
    );
}

#[tokio::test]
async fn on_price_suppresses_signal_when_breakout_direction_disagrees_with_h1() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).rev().map(Decimal::from));
    s.m15.insert(
        "USD_JPY".to_string(),
        calm_m15_history_with_constant_true_range(),
    );

    assert!(
        s.on_price(&m5(0, (dec!(100), dec!(103), dec!(99.9), dec!(100.5)), 10))
            .await
            .is_none()
    );
    assert!(
        s.on_price(&m5(5, (dec!(100.5), dec!(103), dec!(99.8), dec!(101)), 10))
            .await
            .is_none()
    );
    let signal = s
        .on_price(&m5(10, (dec!(101), dec!(103), dec!(99.8), dec!(101.8)), 10))
        .await;

    assert!(
        signal.is_none(),
        "upward breakout with a disagreeing H1 downtrend must not emit a signal"
    );
}

#[tokio::test]
async fn on_price_suppresses_signal_when_atr_does_not_expand() {
    let mut s = strategy();
    push_h1_closes(&mut s, (1..=50).map(Decimal::from));
    s.m15.insert(
        "USD_JPY".to_string(),
        calm_m15_history_with_constant_true_range(),
    );

    assert!(
        s.on_price(&m5(
            0,
            (dec!(100), dec!(100.1), dec!(100), dec!(100.05)),
            10
        ))
        .await
        .is_none()
    );
    assert!(
        s.on_price(&m5(
            5,
            (dec!(100.05), dec!(100.1), dec!(100), dec!(100.1)),
            10
        ))
        .await
        .is_none()
    );
    let signal = s
        .on_price(&m5(
            10,
            (dec!(100.1), dec!(100.15), dec!(100), dec!(100.15)),
            10,
        ))
        .await;

    assert!(
        signal.is_none(),
        "breakout close without ATR expansion must not emit a signal"
    );
}
