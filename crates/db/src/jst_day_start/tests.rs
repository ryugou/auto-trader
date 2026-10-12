use chrono::{NaiveDate, TimeZone, Utc};

use super::*;

#[test]
fn day_start_converts_jst_midnight_to_previous_day_15h_utc() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    assert_eq!(
        jst_day_start_utc(date).unwrap(),
        Utc.with_ymd_and_hms(2026, 10, 11, 15, 0, 0).unwrap()
    );
}

#[test]
fn next_day_start_converts_following_jst_midnight() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    assert_eq!(
        jst_next_day_start_utc(date).unwrap(),
        Utc.with_ymd_and_hms(2026, 10, 12, 15, 0, 0).unwrap()
    );
}

#[test]
fn next_day_start_rejects_max_date() {
    let err = jst_next_day_start_utc(NaiveDate::MAX).unwrap_err();
    assert_eq!(err.to_string(), "invalid date: out of range");
}

#[test]
fn day_start_rejects_min_date() {
    let err = jst_day_start_utc(NaiveDate::MIN).unwrap_err();
    assert_eq!(err.to_string(), "invalid date: out of range");
}
