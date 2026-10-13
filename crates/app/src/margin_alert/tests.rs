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
