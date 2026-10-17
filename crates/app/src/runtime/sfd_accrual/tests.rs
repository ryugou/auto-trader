use chrono::{DateTime, Duration, TimeZone as _, Utc};
use rust_decimal_macros::dec;

use super::*;

#[test]
fn truncate_to_hour_truncates_to_whole_hour_or_returns_none_beyond_timestamp_limit() {
    let far_future = Utc.with_ymd_and_hms(2300, 12, 12, 0, 0, 0).unwrap();
    let normal: DateTime<Utc> = "2024-01-01T13:45:30Z".parse().unwrap();
    let normal_truncated: DateTime<Utc> = "2024-01-01T13:00:00Z".parse().unwrap();

    let cases = [
        (
            "normal_time_truncates_to_whole_hour",
            normal,
            Some(normal_truncated),
        ),
        (
            "timestamp_exceeding_limit_returns_none_instead_of_panicking",
            far_future,
            None,
        ),
    ];

    for (name, input, expected) in cases {
        let result = truncate_to_hour(input);
        assert_eq!(result, expected, "case {name}");
    }
}

#[test]
fn clamp_catch_up_caps_pending_hours_at_max_catch_up() {
    let current_hour: DateTime<Utc> = "2024-01-02T00:00:00Z".parse().unwrap();

    let cases = [
        (
            "within_limit_leaves_lh_and_pending_unchanged",
            current_hour - Duration::hours(3),
            (current_hour - Duration::hours(3), 3),
        ),
        (
            "exceeding_limit_clamps_lh_and_pending_to_max_catch_up",
            current_hour - Duration::hours(30),
            (
                current_hour - Duration::hours(SFD_MAX_CATCH_UP_HOURS),
                SFD_MAX_CATCH_UP_HOURS,
            ),
        ),
    ];

    for (name, lh, expected) in cases {
        let result = clamp_catch_up(lh, current_hour);
        assert_eq!(result, expected, "case {name}");
    }
}

#[test]
fn both_feeds_fresh_requires_both_ages_within_stale_threshold() {
    let cases = [
        ("both_within_threshold_is_fresh", Some(300), Some(300), true),
        ("fx_missing_is_not_fresh", None, Some(300), false),
        (
            "spot_over_threshold_is_not_fresh",
            Some(300),
            Some(301),
            false,
        ),
    ];

    for (name, fx_age, spot_age, expected) in cases {
        let result = both_feeds_fresh(fx_age, spot_age);
        assert_eq!(result, expected, "case {name}");
    }
}

#[test]
fn mid_price_averages_bid_and_ask() {
    let mid = mid_price(dec!(100), dec!(102));

    assert_eq!(mid, dec!(101));
}

#[test]
fn sfd_applies_only_to_fx_btc_jpy() {
    assert!(trade_is_subject_to_bitflyer_sfd("FX_BTC_JPY"));
    assert!(!trade_is_subject_to_bitflyer_sfd("ETH_JPY"));
}

#[test]
fn trade_not_yet_open_at_event_time_is_excluded_from_catchup_billing() {
    let event_at = chrono::Utc::now();
    let entry_after_event = event_at + chrono::Duration::minutes(1);
    let entry_before_event = event_at - chrono::Duration::minutes(1);
    assert!(trade_was_not_yet_open_at_event_time(
        entry_after_event,
        event_at
    ));
    assert!(!trade_was_not_yet_open_at_event_time(
        entry_before_event,
        event_at
    ));
}
