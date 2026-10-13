use super::*;

fn rate(long: Decimal, short: Decimal) -> SwapRateEntry {
    SwapRateEntry { long, short }
}

#[test]
fn long_positive_rate_pays_fee() {
    let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, dec!(10_000));
    assert_eq!(fee, dec!(100));
}

#[test]
fn short_negative_rate_receives_fee() {
    let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Short, dec!(10_000));
    assert_eq!(fee, dec!(-120));
}

#[test]
fn partial_lot_scales_proportionally() {
    let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, dec!(5_000));
    assert_eq!(fee, dec!(50));
}

#[test]
fn truncates_fractional_yen_to_zero() {
    let fee = compute_daily_swap(rate(dec!(1), dec!(-1)), Direction::Long, dec!(3_300));
    assert_eq!(fee, Decimal::ZERO);
}

#[test]
fn zero_rate_returns_zero() {
    let fee = compute_daily_swap(rate(dec!(0), dec!(0)), Direction::Long, dec!(10_000));
    assert_eq!(fee, Decimal::ZERO);
}

#[test]
fn zero_quantity_returns_zero() {
    let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, Decimal::ZERO);
    assert_eq!(fee, Decimal::ZERO);
}

#[test]
fn staleness_boundary_is_exclusive() {
    use chrono::NaiveDate;
    let updated = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
    let on_limit = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
    let over_limit = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
    assert!(!is_swap_rates_stale(updated, on_limit, 35));
    assert!(is_swap_rates_stale(updated, over_limit, 35));
}

#[test]
fn future_updated_on_is_not_stale() {
    use chrono::NaiveDate;
    let updated = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
    assert!(!is_swap_rates_stale(updated, today, 35));
}
