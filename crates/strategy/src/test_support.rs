use std::collections::HashMap;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Candle, Direction, Exchange, Pair, Position, Trade, TradeStatus};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

pub(crate) fn make_event_at(
    pair: &str,
    timeframe: &str,
    close: Decimal,
    high: Decimal,
    low: Decimal,
    ts: DateTime<Utc>,
) -> PriceEvent {
    PriceEvent {
        pair: Pair::new(pair),
        exchange: Exchange::BitflyerCfd,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(pair),
            exchange: Exchange::BitflyerCfd,
            timeframe: timeframe.to_string(),
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

pub(crate) fn make_position_with_sl_at(
    strategy: &str,
    pair: &str,
    direction: Direction,
    entry: Decimal,
    stop_loss: Decimal,
    entry_at: DateTime<Utc>,
) -> Position {
    Position {
        trade: Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: strategy.to_string(),
            pair: Pair::new(pair),
            exchange: Exchange::BitflyerCfd,
            direction,
            entry_price: entry,
            exit_price: None,
            stop_loss,
            take_profit: None,
            quantity: dec!(0.001),
            leverage: dec!(2),
            fees: dec!(0),
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: None,
        },
    }
}

pub(crate) fn make_event(
    pair: &str,
    timeframe: &str,
    close: Decimal,
    high: Decimal,
    low: Decimal,
) -> PriceEvent {
    make_event_at(pair, timeframe, close, high, low, Utc::now())
}

pub(crate) fn make_position_with_sl(
    strategy: &str,
    pair: &str,
    direction: Direction,
    entry: Decimal,
    stop_loss: Decimal,
) -> Position {
    make_position_with_sl_at(strategy, pair, direction, entry, stop_loss, Utc::now())
}

pub(crate) async fn warmup_with_flat_candles(
    s: &mut impl Strategy,
    pair: &str,
    timeframe: &str,
    close: Decimal,
    high: Decimal,
    low: Decimal,
    bars: usize,
) {
    for _ in 0..bars {
        let _ = s
            .on_price(&make_event(pair, timeframe, close, high, low))
            .await;
    }
}

pub(crate) async fn warmup_with_drift(
    s: &mut impl Strategy,
    pair: &str,
    timeframe: &str,
    base_close: Decimal,
    spread: Decimal,
    bars: usize,
    drift_at: impl Fn(usize) -> Decimal,
) {
    for i in 0..bars {
        let drift = drift_at(i);
        let _ = s
            .on_price(&make_event(
                pair,
                timeframe,
                base_close + drift,
                base_close + spread + drift,
                base_close - spread + drift,
            ))
            .await;
    }
}
