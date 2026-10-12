use rust_decimal_macros::dec;

use super::*;

fn long_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
    OpenPosition {
        direction: Direction::Long,
        entry_price: entry,
        current_price: current,
        quantity: qty,
        leverage: lev,
    }
}

fn short_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
    OpenPosition {
        direction: Direction::Short,
        entry_price: entry,
        current_price: current,
        quantity: qty,
        leverage: lev,
    }
}

#[test]
fn compute_maintenance_ratio_long_in_profit() {
    let pos = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
    let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
    assert_eq!(ratio, dec!(110000) / dec!(60000));
}

#[test]
fn compute_maintenance_ratio_short_in_loss() {
    let pos = short_at(dec!(150), dec!(152), dec!(10000), dec!(25));
    let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
    assert_eq!(ratio, dec!(80000) / dec!(60000));
}

#[test]
fn compute_maintenance_ratio_open_immediately_after_fill() {
    let pos = long_at(dec!(150), dec!(150), dec!(10000), dec!(25));
    let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
    assert_eq!(ratio, dec!(100000) / dec!(60000));
}

#[test]
fn compute_maintenance_ratio_multiple_positions_sum() {
    let p1 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
    let p2 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
    let ratio_split = compute_maintenance_ratio(dec!(100000), &[p1, p2]).unwrap();
    let p_combined = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
    let ratio_single = compute_maintenance_ratio(dec!(100000), &[p_combined]).unwrap();
    assert_eq!(ratio_split, ratio_single);
}

#[test]
fn compute_maintenance_ratio_zero_required_returns_none() {
    assert!(compute_maintenance_ratio(dec!(100000), &[]).is_none());
}

#[test]
fn required_margin_is_truncated_to_yen_to_match_ledger() {
    let pos = long_at(dec!(150.123), dec!(150.123), dec!(10000), dec!(25));
    assert_eq!(pos.required_margin(), dec!(60049));
}

#[test]
fn compute_maintenance_ratio_negative_equity_returns_negative_ratio() {
    let pos = short_at(dec!(150), dec!(400), dec!(10000), dec!(25));
    let ratio = compute_maintenance_ratio(dec!(0), &[pos]).unwrap();
    assert!(ratio.is_sign_negative(), "ratio={ratio}, expected negative");
}
