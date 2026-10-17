use auto_trader_core::types::Exchange;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn classify_levels() {
    let liquidation_level = dec!(0.50);
    assert_eq!(classify_alert_level(dec!(0.70), liquidation_level), None);
    assert_eq!(
        classify_alert_level(dec!(0.64), liquidation_level),
        Some(AlertLevel::Warn)
    );
    assert_eq!(
        classify_alert_level(dec!(0.54), liquidation_level),
        Some(AlertLevel::Critical)
    );
}

#[test]
fn classify_boundaries_are_exclusive_upper() {
    let liquidation_level = dec!(0.50);
    assert_eq!(classify_alert_level(dec!(0.65), liquidation_level), None);
    assert_eq!(
        classify_alert_level(dec!(0.55), liquidation_level),
        Some(AlertLevel::Warn)
    );
}

#[test]
fn build_margin_alert_returns_none_above_warn_level() {
    let account_id = Uuid::new_v4();
    assert!(build_margin_alert(account_id, &[], dec!(0.70), dec!(0.50)).is_none());
}

#[test]
fn build_margin_alert_falls_back_to_account_id_when_name_missing() {
    let account_id = Uuid::new_v4();
    let mut owned =
        crate::liquidation::tests::make_owned(Exchange::GmoFx, account_id, Some("live"));
    owned.account_name = None;
    let alert = build_margin_alert(account_id, &[&owned], dec!(0.54), dec!(0.50))
        .expect("ratio below critical must alert");
    assert_eq!(alert.account_id, account_id);
    assert_eq!(alert.account_name, account_id.to_string());
    assert_eq!(alert.ratio, dec!(0.54));
    assert_eq!(alert.threshold, dec!(0.50));
    assert_eq!(alert.level, AlertLevel::Critical);
}
