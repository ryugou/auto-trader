use auto_trader_core::types::{Direction, Pair, Trade, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

fn make_trade(strategy_name: &str) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: strategy_name.to_string(),
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
        status: TradeStatus::Open,
        max_hold_until: None,
        exchange_position_id: None,
        stop_order_id: None,
    }
}

fn make_row(strategy_name: &str) -> OpenTradeWithAccount {
    OpenTradeWithAccount {
        trade: make_trade(strategy_name),
        account_name: Some("test-account".to_string()),
        account_type: Some("paper".to_string()),
    }
}

#[test]
fn group_positions_by_strategy_groups_rows_by_strategy_name() {
    let rows = vec![make_row("strategy_a"), make_row("strategy_b")];

    let by_strategy = group_positions_by_strategy(rows);

    assert_eq!(by_strategy.len(), 2);
    assert_eq!(by_strategy["strategy_a"].len(), 1);
    assert_eq!(by_strategy["strategy_b"].len(), 1);
}

#[test]
fn group_positions_by_strategy_keeps_multiple_rows_of_same_strategy_in_input_order() {
    let first = make_row("strategy_a");
    let second = make_row("strategy_a");
    let first_id = first.trade.id;
    let second_id = second.trade.id;

    let by_strategy = group_positions_by_strategy(vec![first, second]);

    let positions = &by_strategy["strategy_a"];
    assert_eq!(positions.len(), 2);
    assert_eq!(positions[0].trade.id, first_id);
    assert_eq!(positions[1].trade.id, second_id);
}

#[test]
fn group_positions_by_strategy_returns_empty_map_for_empty_input() {
    let by_strategy = group_positions_by_strategy(Vec::new());

    assert!(by_strategy.is_empty());
}
