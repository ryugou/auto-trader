use super::*;

#[test]
fn decimal_to_milli_rejecting_fraction_rejects_sub_milli_yen_remainder() {
    let d = Decimal::new(1_497_001, 4);
    let err = decimal_to_milli_rejecting_fraction(d, "bid_open", 1_000).unwrap_err();
    assert!(matches!(err, SimError::Other(_)), "{err:?}");
}

#[test]
fn decimal_to_milli_rejecting_fraction_accepts_exact_milli_yen_value() {
    let d = Decimal::new(149_700, 3);
    assert_eq!(
        decimal_to_milli_rejecting_fraction(d, "bid_open", 1_000).unwrap(),
        149_700
    );
}

#[test]
fn epoch_secs_to_datetime_rejects_seconds_outside_the_representable_range() {
    let err = epoch_secs_to_datetime(i64::MAX, "test").unwrap_err();
    assert!(matches!(err, SimError::Other(_)), "{err:?}");
}

#[test]
fn epoch_secs_to_datetime_accepts_the_unix_epoch() {
    let dt = epoch_secs_to_datetime(0, "test").unwrap();
    assert_eq!(dt.timestamp(), 0);
}

#[test]
fn decimal_to_milli_rejects_fractional_remainder_and_reports_scaled_value() {
    let d = Decimal::new(1_497_001, 4);
    assert_eq!(
        decimal_to_milli(d),
        Err(MilliScaleRejection::Fractional {
            scaled: d * Decimal::from(1000)
        })
    );
}

#[test]
fn decimal_to_milli_rejects_values_that_overflow_i64_at_milli_scale() {
    let d = Decimal::from(i64::MAX);
    assert_eq!(decimal_to_milli(d), Err(MilliScaleRejection::Overflow));
}

#[test]
fn decimal_to_milli_accepts_exact_milli_value() {
    assert_eq!(decimal_to_milli(Decimal::new(149_605, 3)), Ok(149_605));
}

#[test]
fn format_epoch_secs_rfc3339_renders_utc_with_z_suffix() {
    assert_eq!(
        format_epoch_secs_rfc3339(0).as_deref(),
        Some("1970-01-01T00:00:00Z")
    );
}

#[test]
fn format_epoch_secs_rfc3339_rejects_unrepresentable_seconds() {
    assert_eq!(format_epoch_secs_rfc3339(i64::MAX), None);
}
