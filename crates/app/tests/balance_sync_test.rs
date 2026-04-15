use auto_trader::tasks::balance_sync::is_drift_over_threshold;
use rust_decimal_macros::dec;

#[test]
fn drift_over_one_percent_is_reported() {
    assert!(is_drift_over_threshold(
        dec!(30000),
        dec!(30400),
        dec!(0.01)
    ));
}

#[test]
fn drift_at_exactly_threshold_is_not_reported() {
    assert!(!is_drift_over_threshold(
        dec!(30000),
        dec!(30300),
        dec!(0.01)
    ));
}

#[test]
fn no_drift_when_values_equal() {
    assert!(!is_drift_over_threshold(
        dec!(30000),
        dec!(30000),
        dec!(0.01)
    ));
}

#[test]
fn drift_works_with_negative_diff() {
    assert!(is_drift_over_threshold(
        dec!(30000),
        dec!(29500),
        dec!(0.01)
    ));
}

#[test]
fn zero_db_balance_never_triggers_drift_div_by_zero() {
    assert!(!is_drift_over_threshold(dec!(0), dec!(100), dec!(0.01)));
}
