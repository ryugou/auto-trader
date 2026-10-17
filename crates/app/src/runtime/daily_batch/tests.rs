use chrono::{DateTime, NaiveDate, Utc};

use super::*;

#[test]
fn is_jst_sunday_is_true_only_when_utc_plus_nine_hours_is_sunday() {
    let cases = [
        (
            "utc_saturday_15_00_is_jst_sunday_00_00",
            "2024-01-06T15:00:00Z",
            true,
        ),
        (
            "utc_sunday_15_00_is_jst_monday_00_00",
            "2024-01-07T15:00:00Z",
            false,
        ),
        (
            "utc_sunday_14_59_is_jst_sunday_23_59",
            "2024-01-07T14:59:00Z",
            true,
        ),
    ];

    for (name, input, expected) in cases {
        let now_utc: DateTime<Utc> = input.parse().unwrap();

        let result = is_jst_sunday(now_utc);

        assert_eq!(result, expected, "case {name}");
    }
}

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2024, 1, day).unwrap()
}

#[test]
fn backfill_dates_returns_oldest_first_per_case() {
    let today = date(10);
    let cases = [
        (
            "none_defaults_to_seven_days_oldest_first",
            None,
            vec![
                date(3),
                date(4),
                date(5),
                date(6),
                date(7),
                date(8),
                date(9),
            ],
        ),
        (
            "some_two_returns_two_days_oldest_first",
            Some(2),
            vec![date(8), date(9)],
        ),
        ("some_zero_returns_empty", Some(0), Vec::new()),
    ];

    for (name, backfill_days, expected) in cases {
        let dates = backfill_dates(today, backfill_days);

        assert_eq!(dates, expected, "case {name}");
    }
}
