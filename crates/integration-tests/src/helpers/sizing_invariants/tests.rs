use auto_trader_core::types::{Exchange, Pair, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

fn expected_quantity_with_min_lot_0_001(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
) -> Option<Decimal> {
    expected_quantity(
        balance,
        leverage,
        allocation_pct,
        stop_loss_pct,
        liquidation_margin_level,
        entry_price,
        dec!(0.001),
    )
}

#[test]
fn expected_quantity_matches_bitflyer_cfd_sizer_case() {
    let qty = expected_quantity(
        dec!(30000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
        dec!(12500000),
        dec!(0.001),
    );
    assert_eq!(qty, Some(dec!(0.004)));
}

#[test]
fn expected_quantity_matches_gmo_fx_sizer_case() {
    let qty = expected_quantity(
        dec!(30000),
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
        dec!(157),
        dec!(1),
    );
    assert_eq!(qty, Some(dec!(1592)));
}

#[test]
fn expected_quantity_returns_none_when_below_min_lot() {
    let qty = expected_quantity(
        dec!(10_000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
        dec!(25_000_000),
        dec!(0.001),
    );
    assert_eq!(qty, None);
}

#[test]
fn expected_quantity_rejects_non_positive_inputs() {
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(0),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.5),
            dec!(1)
        ),
        None
    );
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(0),
            dec!(1.0),
            dec!(0.02),
            dec!(0.5),
            dec!(1)
        ),
        None
    );
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(2),
            dec!(0),
            dec!(0.02),
            dec!(0.5),
            dec!(1)
        ),
        None
    );
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(2),
            dec!(1.0),
            dec!(0),
            dec!(0.5),
            dec!(1)
        ),
        None
    );
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0),
            dec!(1)
        ),
        None
    );
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.5),
            dec!(0)
        ),
        None
    );
}

#[test]
fn expected_quantity_rejects_allocation_above_one() {
    assert_eq!(
        expected_quantity_with_min_lot_0_001(
            dec!(30000),
            dec!(2),
            dec!(1.5),
            dec!(0.02),
            dec!(0.5),
            dec!(1)
        ),
        None
    );
}

#[test]
fn compute_max_alloc_rejects_non_positive_inputs() {
    assert_eq!(compute_max_alloc(dec!(0), dec!(0.02), dec!(0.5)), None);
    assert_eq!(compute_max_alloc(dec!(2), dec!(0), dec!(0.5)), None);
    assert_eq!(compute_max_alloc(dec!(2), dec!(0.02), dec!(0)), None);
    assert_eq!(compute_max_alloc(dec!(-1), dec!(0.02), dec!(0.5)), None);
    let m = compute_max_alloc(dec!(2), dec!(0.02), dec!(0.5)).expect("positive inputs");
    assert!(m > Decimal::ZERO);
}

#[test]
fn expected_stop_loss_price_long_subtracts() {
    let sl = expected_stop_loss_price(dec!(100), Direction::Long, dec!(0.02));
    assert_eq!(sl, dec!(98.00));
}

#[test]
fn expected_stop_loss_price_short_adds() {
    let sl = expected_stop_loss_price(dec!(100), Direction::Short, dec!(0.02));
    assert_eq!(sl, dec!(102.00));
}

#[test]
fn expected_take_profit_price_long_adds() {
    let tp = expected_take_profit_price(dec!(100), Direction::Long, dec!(0.04));
    assert_eq!(tp, dec!(104.00));
}

#[test]
fn expected_take_profit_price_short_subtracts() {
    let tp = expected_take_profit_price(dec!(100), Direction::Short, dec!(0.04));
    assert_eq!(tp, dec!(96.00));
}

#[test]
fn expected_pnl_long_winning_is_positive() {
    let pnl = expected_pnl(dec!(100), dec!(110), dec!(2), Direction::Long);
    assert_eq!(pnl, dec!(20));
}

#[test]
fn expected_pnl_long_losing_is_negative() {
    let pnl = expected_pnl(dec!(100), dec!(95), dec!(2), Direction::Long);
    assert_eq!(pnl, dec!(-10));
}

#[test]
fn expected_pnl_short_winning_is_positive() {
    let pnl = expected_pnl(dec!(100), dec!(90), dec!(2), Direction::Short);
    assert_eq!(pnl, dec!(20));
}

#[test]
fn expected_pnl_short_losing_is_negative() {
    let pnl = expected_pnl(dec!(100), dec!(105), dec!(2), Direction::Short);
    assert_eq!(pnl, dec!(-10));
}

fn make_trade(
    direction: Direction,
    entry: Decimal,
    sl: Decimal,
    qty: Decimal,
    leverage: Decimal,
) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "t".to_string(),
        pair: Pair::new("USD_JPY"),
        exchange: Exchange::GmoFx,
        direction,
        entry_price: entry,
        exit_price: None,
        stop_loss: sl,
        take_profit: None,
        quantity: qty,
        leverage,
        fees: Decimal::ZERO,
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

#[test]
fn assert_post_sl_passes_at_exact_cap() {
    let balance = dec!(30000);
    let leverage = dec!(10);
    let sl_pct = dec!(0.02);
    let y = dec!(1.0);
    let entry = dec!(157);
    let raw_qty = compute_raw_quantity(balance, leverage, dec!(1.0), sl_pct, y, entry)
        .expect("positive inputs should produce a raw qty");
    let sl = expected_stop_loss_price(entry, Direction::Long, sl_pct);
    let trade = make_trade(Direction::Long, entry, sl, raw_qty, leverage);
    assert_post_sl_margin_level_at_least_y(&trade, balance, y);
}

#[test]
#[should_panic(expected = "invalid post-SL margin level")]
fn assert_post_sl_panics_on_over_alloc() {
    let balance = dec!(30000);
    let leverage = dec!(10);
    let entry = dec!(157);
    let sl = expected_stop_loss_price(entry, Direction::Long, dec!(0.05));
    let trade = make_trade(Direction::Long, entry, sl, dec!(2000), leverage);
    assert_post_sl_margin_level_at_least_y(&trade, balance, dec!(1.0));
}
