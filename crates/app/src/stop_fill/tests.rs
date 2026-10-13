use auto_trader_core::types::{Direction, Exchange, Pair, Trade, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

fn make_owned(account_type: &str, stop_order_id: Option<&str>) -> OpenTradeWithAccount {
    let trade = Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "s".into(),
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        direction: Direction::Long,
        entry_price: dec!(150),
        exit_price: None,
        stop_loss: dec!(147),
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
        exchange_position_id: None,
        stop_order_id: stop_order_id.map(|s| s.to_string()),
    };
    OpenTradeWithAccount {
        trade,
        account_name: Some("acct".into()),
        account_type: Some(account_type.into()),
    }
}

#[test]
fn live_trade_with_stop_id_is_target() {
    let t = make_owned("live", Some("stop-1"));
    assert!(is_stop_detection_target(&t, false));
}

#[test]
fn paper_trade_is_never_target() {
    let t = make_owned("paper", Some("stop-1"));
    assert!(!is_stop_detection_target(&t, false));
}

#[test]
fn live_without_stop_id_is_not_target() {
    let t = make_owned("live", None);
    assert!(!is_stop_detection_target(&t, false));
}

#[test]
fn live_forces_dry_run_excludes_live_trade() {
    let t = make_owned("live", Some("stop-1"));
    assert!(
        !is_stop_detection_target(&t, true),
        "LIVE_DRY_RUN で live trade も dry_run 扱いになり対象外"
    );
}
