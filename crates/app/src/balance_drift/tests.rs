use auto_trader_core::types::{Direction, Exchange, Pair, Trade, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

#[test]
fn drift_threshold_is_1pct_or_500yen_whichever_larger() {
    let small_equity_500_yen_floor = dec!(30000);
    assert!(!is_drift(small_equity_500_yen_floor, dec!(30400)));
    assert!(is_drift(small_equity_500_yen_floor, dec!(30501)));

    let large_equity_1pct_threshold = dec!(1000000);
    assert!(!is_drift(large_equity_1pct_threshold, dec!(1009999)));
    assert!(is_drift(large_equity_1pct_threshold, dec!(1010001)));
}

fn make_trade(status: TradeStatus) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "test".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        exchange: Exchange::BitflyerCfd,
        direction: Direction::Long,
        entry_price: dec!(100),
        exit_price: None,
        stop_loss: dec!(90),
        take_profit: None,
        quantity: dec!(1),
        leverage: dec!(1),
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status,
        max_hold_until: None,
        exchange_position_id: None,
        stop_order_id: None,
    }
}

#[test]
fn split_for_equity_all_open_returns_open_with_all_trades() {
    let t1 = make_trade(TradeStatus::Open);
    let t2 = make_trade(TradeStatus::Open);
    let ids = [t1.id, t2.id];
    let trades = vec![t1, t2];

    match split_for_equity(trades) {
        EquityInput::Open(open) => {
            assert_eq!(open.len(), 2);
            assert!(open.iter().all(|t| ids.contains(&t.id)));
        }
        EquityInput::CloseInProgress { closing } => {
            panic!("expected Open, got CloseInProgress {{ closing: {closing} }}")
        }
    }
}

#[test]
fn split_for_equity_one_closing_among_open_returns_close_in_progress() {
    let trades = vec![
        make_trade(TradeStatus::Open),
        make_trade(TradeStatus::Closing),
        make_trade(TradeStatus::Open),
    ];

    match split_for_equity(trades) {
        EquityInput::CloseInProgress { closing } => assert_eq!(closing, 1),
        EquityInput::Open(open) => panic!("expected CloseInProgress, got Open({})", open.len()),
    }
}

#[test]
fn split_for_equity_empty_returns_open_empty() {
    match split_for_equity(vec![]) {
        EquityInput::Open(open) => assert!(open.is_empty()),
        EquityInput::CloseInProgress { closing } => {
            panic!("expected Open, got CloseInProgress {{ closing: {closing} }}")
        }
    }
}
