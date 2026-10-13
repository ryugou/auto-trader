use auto_trader_core::types::{Direction, Pair};
use rust_decimal_macros::dec;

use super::*;

#[test]
fn trigger_price_rounds_to_safe_side() {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let mut units = HashMap::new();
    units.insert(Pair::new("USD_JPY"), dec!(0.001));
    let sizer = PositionSizer::new(min_sizes, dec!(0.10)).with_price_units(units);
    assert_eq!(
        sizer.round_trigger_price(&Pair::new("USD_JPY"), dec!(156.78912), Direction::Long),
        dec!(156.790)
    );
    assert_eq!(
        sizer.round_trigger_price(&Pair::new("USD_JPY"), dec!(156.78912), Direction::Short),
        dec!(156.789)
    );
}

#[test]
fn trigger_price_unrounded_when_no_unit_configured() {
    let sizer = PositionSizer::new(HashMap::new(), Decimal::ZERO);
    assert_eq!(
        sizer.round_trigger_price(&Pair::new("USD_JPY"), dec!(156.78912), Direction::Long),
        dec!(156.78912)
    );
}

fn btc_sizer() -> PositionSizer {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
    PositionSizer::new(min_sizes, Decimal::ZERO)
}

fn fx_sizer() -> PositionSizer {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    PositionSizer::new(min_sizes, Decimal::ZERO)
}

#[test]
fn gmo_fx_loose_sl_caps_at_alloc_below_one() {
    let qty = fx_sizer().calculate_quantity(
        &Pair::new("USD_JPY"),
        dec!(30000),
        dec!(157),
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
    );
    assert_eq!(qty, Some(dec!(1592)));
}

#[test]
fn gmo_fx_tight_sl_higher_allocation() {
    let qty = fx_sizer().calculate_quantity(
        &Pair::new("USD_JPY"),
        dec!(30000),
        dec!(157),
        dec!(10),
        dec!(1.0),
        dec!(0.005),
        dec!(1.00),
    );
    assert_eq!(qty, Some(dec!(1819)));
}

#[test]
fn margin_buffer_tightens_allocation() {
    let mut min_sizes = HashMap::new();
    min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
    let sizer = PositionSizer::new(min_sizes, dec!(0.10));
    let qty = sizer.calculate_quantity(
        &Pair::new("USD_JPY"),
        dec!(30000),
        dec!(157),
        dec!(10),
        dec!(1.0),
        dec!(0.02),
        dec!(1.00),
    );
    assert_eq!(qty, Some(dec!(1469)));
}

#[test]
fn bitflyer_cfd_typical_sl_uses_full_allocation() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(30000),
        dec!(12500000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(qty, Some(dec!(0.004)));
}

#[test]
fn lc_constraint_binds_at_high_leverage_and_wide_sl() {
    let qty = fx_sizer().calculate_quantity(
        &Pair::new("USD_JPY"),
        dec!(30000),
        dec!(157),
        dec!(10),
        dec!(1.0),
        dec!(0.10),
        dec!(1.00),
    );
    assert_eq!(qty, Some(dec!(955)));
}

#[test]
fn allocation_pct_dominates_when_smaller_than_max_alloc() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(30000),
        dec!(12500000),
        dec!(2),
        dec!(0.5),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(qty, Some(dec!(0.002)));
}

#[test]
fn rejects_zero_or_negative_liquidation_margin_level() {
    let s = fx_sizer();
    let p = Pair::new("USD_JPY");
    let cases = [("zero", dec!(0)), ("negative", dec!(-0.5))];
    for (name, level) in cases {
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                level
            ),
            None,
            "liquidation margin level {name}"
        );
    }
}

#[test]
fn rejects_zero_or_negative_inputs() {
    let s = fx_sizer();
    let p = Pair::new("USD_JPY");
    let cases = [
        (
            "zero balance",
            [
                dec!(0),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00),
            ],
        ),
        (
            "zero price",
            [
                dec!(30000),
                dec!(0),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00),
            ],
        ),
        (
            "zero leverage",
            [
                dec!(30000),
                dec!(157),
                dec!(0),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00),
            ],
        ),
        (
            "zero allocation",
            [
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(0),
                dec!(0.02),
                dec!(1.00),
            ],
        ),
        (
            "allocation above one",
            [
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.5),
                dec!(0.02),
                dec!(1.00),
            ],
        ),
        (
            "zero stop loss",
            [
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0),
                dec!(1.00),
            ],
        ),
    ];
    for (name, [balance, price, leverage, allocation, stop_loss, level]) in cases {
        assert_eq!(
            s.calculate_quantity(&p, balance, price, leverage, allocation, stop_loss, level),
            None,
            "{name}"
        );
    }
}

#[test]
fn rejects_when_account_too_small_for_one_min_lot() {
    let qty = btc_sizer().calculate_quantity(
        &Pair::new("FX_BTC_JPY"),
        dec!(5000),
        dec!(12500000),
        dec!(2),
        dec!(1.0),
        dec!(0.02),
        dec!(0.50),
    );
    assert_eq!(qty, None);
}

#[test]
fn post_sl_margin_level_equals_threshold_invariant() {
    let cases = [
        (dec!(10), dec!(0.005), dec!(1.00)),
        (dec!(10), dec!(0.02), dec!(1.00)),
        (dec!(10), dec!(0.10), dec!(1.00)),
        (dec!(2), dec!(0.05), dec!(0.50)),
        (dec!(2), dec!(0.02), dec!(0.50)),
    ];
    let mut saw_lc_binding = false;
    let mut saw_cap_binding = false;
    for (lev, sl, y) in cases {
        let a_unbounded = Decimal::ONE / (y + lev * sl);
        if a_unbounded < Decimal::ONE {
            saw_lc_binding = true;
            let ml = (Decimal::ONE - lev * a_unbounded * sl) / a_unbounded;
            let diff = (ml - y).abs();
            assert!(
                diff < dec!(0.0001),
                "LC-binding: lev={lev}, sl={sl}, y={y}: margin level {ml} != threshold (diff={diff})"
            );
        } else {
            saw_cap_binding = true;
            let a = Decimal::ONE;
            let ml = (Decimal::ONE - lev * a * sl) / a;
            assert!(
                ml + dec!(0.0001) >= y,
                "cap-binding: lev={lev}, sl={sl}, y={y}: margin level {ml} < threshold (slack 1bp)"
            );
        }
    }
    assert!(saw_lc_binding, "test must cover the LC-binding branch");
    assert!(saw_cap_binding, "test must cover the cap-binding branch");
}
