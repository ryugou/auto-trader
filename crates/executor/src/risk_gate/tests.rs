use chrono::TimeZone;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn jst_day_start_is_15h_utc_of_previous_day() {
    let now = Utc.with_ymd_and_hms(2026, 7, 7, 2, 0, 0).unwrap();
    let start = jst_day_start(now);
    assert_eq!(start, Utc.with_ymd_and_hms(2026, 7, 6, 15, 0, 0).unwrap());
}

#[test]
fn rejects_at_exactly_limit_and_beyond() {
    let balance = dec!(100000);
    let pct = dec!(0.05);
    assert!(matches!(
        eval_daily_loss(dec!(-5000), balance, pct),
        GateDecision::Reject(RejectReason::DailyLossLimit { .. })
    ));
    assert!(matches!(
        eval_daily_loss(dec!(-5001), balance, pct),
        GateDecision::Reject(RejectReason::DailyLossLimit { .. })
    ));
}

#[test]
fn passes_below_limit_and_on_profit() {
    let balance = dec!(100000);
    let pct = dec!(0.05);
    assert!(matches!(
        eval_daily_loss(dec!(-4999), balance, pct),
        GateDecision::Pass
    ));
    assert!(matches!(
        eval_daily_loss(dec!(10000), balance, pct),
        GateDecision::Pass
    ));
}

#[test]
fn passes_when_day_start_balance_non_positive() {
    let pct = dec!(0.05);
    assert!(matches!(
        eval_daily_loss(dec!(-99999), dec!(0), pct),
        GateDecision::Pass
    ));
    assert!(matches!(
        eval_daily_loss(dec!(-99999), dec!(-100), pct),
        GateDecision::Pass
    ));
}
