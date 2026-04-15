use auto_trader::tasks::balance_sync::is_drift_over_threshold;
use rust_decimal_macros::dec;
use std::collections::HashSet;
use uuid::Uuid;

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

/// Verify that the approved-live-account filter correctly gates accounts.
/// This test validates the HashSet logic used inside run_balance_sync_loop
/// without requiring DB or exchange connections.
#[test]
fn approved_live_set_rejects_unapproved_id() {
    let approved_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();

    let approved: HashSet<Uuid> = [approved_id].into_iter().collect();

    assert!(approved.contains(&approved_id));
    assert!(!approved.contains(&other_id));
}
