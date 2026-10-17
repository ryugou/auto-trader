use auto_trader_core::types::{Direction, Pair, Trade, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;

use super::*;

pub(crate) fn make_owned(
    exchange: Exchange,
    account_id: Uuid,
    account_type: Option<&str>,
) -> OpenTradeWithAccount {
    let trade = Trade {
        id: Uuid::new_v4(),
        account_id,
        strategy_name: "s".into(),
        pair: Pair::new("USD_JPY"),
        exchange,
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
        stop_order_id: None,
    };
    OpenTradeWithAccount {
        trade,
        account_name: Some("acct".into()),
        account_type: account_type.map(|s| s.to_string()),
    }
}

#[test]
fn group_open_trades_by_account_filters_exchange_and_groups_by_account() {
    let account_a = Uuid::new_v4();
    let account_b = Uuid::new_v4();
    let trades = vec![
        make_owned(Exchange::GmoFx, account_a, Some("paper")),
        make_owned(Exchange::GmoFx, account_a, Some("paper")),
        make_owned(Exchange::GmoFx, account_b, Some("paper")),
        make_owned(Exchange::BitflyerCfd, account_a, Some("paper")),
    ];
    let buckets = group_open_trades_by_account(&trades, Exchange::GmoFx);
    assert_eq!(buckets.len(), 2);
    assert_eq!(buckets[&account_a].len(), 2);
    assert_eq!(buckets[&account_b].len(), 1);
}

#[test]
fn is_dry_run_bucket_follows_first_trade_account_type() {
    let account = Uuid::new_v4();
    for (account_type, live_forces_dry_run, expected) in [
        (Some("live"), false, false),
        (Some("paper"), false, true),
        (None, false, true),
        (Some("live"), true, true),
    ] {
        let owned = make_owned(Exchange::GmoFx, account, account_type);
        assert_eq!(
            is_dry_run_bucket(&[&owned], live_forces_dry_run),
            expected,
            "account_type={account_type:?} live_forces_dry_run={live_forces_dry_run}"
        );
    }
}
