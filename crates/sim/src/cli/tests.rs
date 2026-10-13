use super::*;

fn bar_at(open_time: i64) -> Bar {
    Bar {
        open_time,
        bid_open: 0,
        bid_high: 0,
        bid_low: 0,
        bid_close: 0,
        ask_open: 0,
        ask_high: 0,
        ask_low: 0,
        ask_close: 0,
    }
}

fn utc_secs(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

const M5: i64 = 300;

#[test]
fn from_omitted_resolves_to_the_next_midnight_after_the_warmup_boundary_bar() {
    let start = utc_secs(2024, 1, 3, 22, 0);
    let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect();

    let (from, _to) = resolve_period(
        &bars,
        None,
        Some(NaiveDate::from_ymd_opt(2024, 1, 10).unwrap()),
        5,
    )
    .expect("enough total bars to satisfy warmup");
    assert_eq!(from, utc_secs(2024, 1, 4, 0, 0));
}

#[test]
fn from_omitted_resolves_to_the_same_instant_when_the_warmup_boundary_bar_is_exactly_midnight() {
    let start = utc_secs(2024, 1, 3, 23, 35);
    let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect();
    assert_eq!(bars[5].open_time, utc_secs(2024, 1, 4, 0, 0));

    let (from, _to) = resolve_period(
        &bars,
        None,
        Some(NaiveDate::from_ymd_opt(2024, 1, 10).unwrap()),
        5,
    )
    .unwrap();
    assert_eq!(from, utc_secs(2024, 1, 4, 0, 0));
}

#[test]
fn to_omitted_resolves_to_the_day_after_the_last_bars_utc_date() {
    let start = utc_secs(2024, 1, 1, 0, 0);
    let bars: Vec<Bar> = (0..10).map(|i| bar_at(start + i * M5)).collect();

    let (_from, to) = resolve_period(
        &bars,
        Some(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        None,
        0,
    )
    .unwrap();
    assert_eq!(to, utc_secs(2024, 1, 2, 0, 0));
}

#[test]
fn from_at_or_after_to_is_rejected() {
    let bars: Vec<Bar> = (0..5).map(|i| bar_at(i * M5)).collect();
    let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();

    let err = resolve_period(&bars, Some(d), Some(d), 0).unwrap_err();
    assert!(matches!(err, SimError::Args(_)), "from == to");

    let after = NaiveDate::from_ymd_opt(2024, 1, 3).unwrap();
    let err = resolve_period(&bars, Some(after), Some(d), 0).unwrap_err();
    assert!(matches!(err, SimError::Args(_)), "from > to");
}

#[test]
fn empty_bars_is_rejected_regardless_of_from_to() {
    let err = resolve_period(&[], None, None, 0).unwrap_err();
    assert!(matches!(err, SimError::Args(_)));

    let d = NaiveDate::from_ymd_opt(2024, 1, 2).unwrap();
    let err = resolve_period(&[], Some(d), Some(d.succ_opt().unwrap()), 0).unwrap_err();
    assert!(
        matches!(err, SimError::Args(_)),
        "even with explicit from/to, zero loaded bars must be rejected"
    );
}

#[test]
fn from_omitted_without_enough_warmup_bars_is_rejected() {
    let bars: Vec<Bar> = (0..3).map(|i| bar_at(i * M5)).collect();
    let err = resolve_period(
        &bars,
        None,
        Some(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        10,
    )
    .unwrap_err();
    assert!(matches!(err, SimError::Args(_)));
}

#[test]
fn naive_date_to_utc_secs_is_midnight() {
    let d = NaiveDate::from_ymd_opt(2024, 3, 10).unwrap();
    assert_eq!(naive_date_to_utc_secs(d), utc_secs(2024, 3, 10, 0, 0));
}

#[test]
fn effective_jobs_keeps_the_requested_value_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(2, 10, 8), 2);
}

#[test]
fn effective_jobs_clamps_to_available_parallelism_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(16, 10, 8), 8);
}

#[test]
fn effective_jobs_clamps_to_the_run_count_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(16, 3, 8), 3);
}

#[test]
fn effective_jobs_returns_one_when_requested_is_zero() {
    assert_eq!(effective_jobs(0, 5, 8), 1);
}

#[test]
fn effective_jobs_returns_one_when_the_run_count_is_zero() {
    assert_eq!(effective_jobs(4, 0, 8), 1);
}

#[test]
fn effective_jobs_returns_one_when_available_parallelism_is_zero() {
    assert_eq!(effective_jobs(4, 10, 0), 1);
}
