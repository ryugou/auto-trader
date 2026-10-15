use chrono::NaiveDate;

use super::*;
use crate::indicators::{IndicatorKey, IndicatorKind};

fn bar_at_secs(open_time: i64, mid_close_yen: f64) -> Bar {
    let milli = (mid_close_yen * 1000.0).round() as i64;
    Bar {
        open_time,
        bid_open: milli,
        bid_high: milli,
        bid_low: milli,
        bid_close: milli,
        ask_open: milli,
        ask_high: milli,
        ask_low: milli,
        ask_close: milli,
    }
}

fn bar_ohlc(open_time: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    let to_milli = |v: f64| (v * 1000.0).round() as i64;
    Bar {
        open_time,
        bid_open: to_milli(open),
        bid_high: to_milli(high),
        bid_low: to_milli(low),
        bid_close: to_milli(close),
        ask_open: to_milli(open),
        ask_high: to_milli(high),
        ask_low: to_milli(low),
        ask_close: to_milli(close),
    }
}

const MIN: i64 = 60;

#[test]
fn m15_bucket_takes_first_open_max_high_min_low_last_close() {
    let bars = vec![
        bar_ohlc(0, 150.00, 150.05, 149.95, 150.02),
        bar_ohlc(5 * MIN, 150.02, 150.10, 150.00, 150.08),
        bar_ohlc(10 * MIN, 150.08, 150.09, 149.90, 150.03),
    ];
    let series = aggregate(&bars, Tf::M15);
    assert_eq!(series.open.len(), 1);
    assert_eq!(series.open[0], 150.00);
    assert_eq!(series.high[0], 150.10);
    assert_eq!(series.low[0], 149.90);
    assert_eq!(series.close[0], 150.03);
    assert_eq!(series.end_time[0], 15 * MIN);
}

#[test]
fn leading_partial_bucket_uses_only_the_m5_bars_present() {
    let bars = vec![
        bar_ohlc(5 * MIN, 150.02, 150.10, 150.00, 150.08),
        bar_ohlc(10 * MIN, 150.08, 150.09, 149.90, 150.03),
    ];
    let series = aggregate(&bars, Tf::M15);
    assert_eq!(series.open.len(), 1);
    assert_eq!(series.open[0], 150.02);
    assert_eq!(series.high[0], 150.10);
    assert_eq!(series.low[0], 149.90);
    assert_eq!(series.close[0], 150.03);
    assert_eq!(series.end_time[0], 15 * MIN);
}

#[test]
fn bucket_with_no_m5_bars_is_not_created() {
    let friday_close = 23 * 3600 + 55 * MIN;
    let monday_open = friday_close + 300 + 2 * 24 * 3600;
    let bars = vec![
        bar_ohlc(friday_close, 150.00, 150.01, 149.99, 150.00),
        bar_ohlc(monday_open, 150.50, 150.51, 150.49, 150.50),
    ];
    for tf in [Tf::M15, Tf::H1, Tf::H4] {
        let series = aggregate(&bars, tf);
        assert_eq!(
            series.open.len(),
            2,
            "{tf:?}: expected no bucket for the empty weekend gap"
        );
    }
}

#[test]
fn completed_increases_exactly_when_the_bucket_last_bar_arrives() {
    let bars = vec![
        bar_at_secs(0, 150.00),
        bar_at_secs(5 * MIN, 150.01),
        bar_at_secs(10 * MIN, 150.02),
    ];
    let series = aggregate(&bars, Tf::M15);
    let completed = completed_counts(&bars, &series.end_time);
    assert_eq!(completed[0], 0, "at :00 the M15 bucket is not complete yet");
    assert_eq!(completed[1], 0, "at :05 the M15 bucket is not complete yet");
    assert_eq!(completed[2], 1, "at :10 the M15 bucket just completed");
}

#[test]
fn future_bars_do_not_affect_past_completed_or_series_values() {
    let n = 240usize;
    let base: Vec<Bar> = (0..n)
        .map(|i| {
            let i = i as i64;
            bar_ohlc(
                i * 5 * MIN,
                150.0 + i as f64 * 0.01,
                150.0 + i as f64 * 0.01 + 0.03,
                150.0 + i as f64 * 0.01 - 0.03,
                150.0 + i as f64 * 0.01 + 0.01,
            )
        })
        .collect();
    let to = base.last().unwrap().open_time + M5_SECS;
    let dataset_a = Dataset::new(base.clone(), 0, to, 0, 64).unwrap();

    let probe_ts = [2usize, 3, 11, 12, 47, 48, 100, 200];
    let mut nonempty_seen = [false; TF_COUNT];

    for &t in &probe_ts {
        let mut mutated = base.clone();
        for bar in mutated.iter_mut().skip(t + 1) {
            *bar = bar_ohlc(bar.open_time, 999.0, 1000.0, 998.0, 999.5);
        }
        let dataset_b = Dataset::new(mutated, 0, to, 0, 64).unwrap();

        for tf in ALL_TFS {
            let visible = dataset_a.completed(tf, t);
            assert_eq!(
                visible,
                dataset_b.completed(tf, t),
                "{tf:?} t={t}: completed(t) must not depend on future bars"
            );
            if visible >= 1 {
                nonempty_seen[tf.idx()] = true;
            }
            let (a, b) = (dataset_a.series(tf), dataset_b.series(tf));
            assert_eq!(a.open[..visible], b.open[..visible], "{tf:?} t={t}: open");
            assert_eq!(a.high[..visible], b.high[..visible], "{tf:?} t={t}: high");
            assert_eq!(a.low[..visible], b.low[..visible], "{tf:?} t={t}: low");
            assert_eq!(
                a.close[..visible],
                b.close[..visible],
                "{tf:?} t={t}: close"
            );
            assert_eq!(
                a.end_time[..visible],
                b.end_time[..visible],
                "{tf:?} t={t}: end_time"
            );

            let sma_key = IndicatorKey {
                tf,
                kind: IndicatorKind::Sma,
                period: 3,
                mult_x100: 0,
            };
            let bb_key = IndicatorKey {
                tf,
                kind: IndicatorKind::Bb,
                period: 3,
                mult_x100: 200,
            };
            let bits = |v: &[f64]| -> Vec<u64> { v.iter().map(|x| x.to_bits()).collect() };
            match (
                &*dataset_a.indicator(sma_key),
                &*dataset_b.indicator(sma_key),
            ) {
                (IndicatorSeries::Single(x), IndicatorSeries::Single(y)) => assert_eq!(
                    bits(&x[..visible]),
                    bits(&y[..visible]),
                    "{tf:?} t={t}: sma must not depend on future bars"
                ),
                _ => panic!("sma must be Single"),
            }
            match (&*dataset_a.indicator(bb_key), &*dataset_b.indicator(bb_key)) {
                (
                    IndicatorSeries::Band {
                        lower: l1,
                        middle: m1,
                        upper: u1,
                    },
                    IndicatorSeries::Band {
                        lower: l2,
                        middle: m2,
                        upper: u2,
                    },
                ) => {
                    for (name, x, y) in [("lower", l1, l2), ("middle", m1, m2), ("upper", u1, u2)] {
                        assert_eq!(
                            bits(&x[..visible]),
                            bits(&y[..visible]),
                            "{tf:?} t={t}: bb.{name} must not depend on future bars"
                        );
                    }
                }
                _ => panic!("bb must be Band"),
            }
        }
    }

    for tf in ALL_TFS {
        assert!(
            nonempty_seen[tf.idx()],
            "{tf:?}: no probe t had a completed bar; the comparison would be vacuous"
        );
    }
}

#[test]
fn leading_partial_bucket_completes_by_end_time_alone() {
    let bars = vec![bar_at_secs(5 * MIN, 150.00), bar_at_secs(10 * MIN, 150.01)];
    let to = 15 * MIN;
    let dataset = Dataset::new(bars, 0, to, 0, 64).unwrap();
    assert_eq!(dataset.completed(Tf::M15, 0), 0);
    assert_eq!(dataset.completed(Tf::M15, 1), 1);
}

#[test]
fn all_tfs_order_matches_idx_and_tf_count() {
    assert_eq!(ALL_TFS.len(), TF_COUNT);
    for (i, &tf) in ALL_TFS.iter().enumerate() {
        assert_eq!(tf.idx(), i, "{tf:?}: idx() must match its ALL_TFS position");
    }
}

fn make_bars(n: usize, start_secs: i64) -> Vec<Bar> {
    (0..n)
        .map(|i| bar_at_secs(start_secs + i as i64 * 5 * MIN, 150.0 + i as f64 * 0.001))
        .collect()
}

#[test]
fn new_succeeds_when_warmup_count_matches_exactly() {
    let warmup_bars = 5;
    let eval_count = 3;
    let all_bars = make_bars(warmup_bars + eval_count, 0);
    let from = all_bars[warmup_bars].open_time;
    let to = all_bars[warmup_bars + eval_count - 1].open_time + M5_SECS;

    let dataset =
        Dataset::new(all_bars, from, to, warmup_bars, 64).expect("exact warmup count must succeed");
    assert_eq!(dataset.eval_start(), warmup_bars);
    assert_eq!(dataset.bars().len(), warmup_bars + eval_count);
}

fn utc_secs(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, 0)
        .unwrap()
        .and_utc()
        .timestamp()
}

#[test]
fn earliest_from_is_next_midnight_when_both_boundary_bars_are_mid_day() {
    let bars = make_bars(60, utc_secs(2024, 1, 3, 22, 0));
    assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 4, 0, 0)));
}

#[test]
fn earliest_from_is_that_midnight_when_the_first_eval_bar_is_exactly_midnight() {
    let bars = make_bars(20, utc_secs(2024, 1, 3, 23, 35));
    assert_eq!(bars[5].open_time, utc_secs(2024, 1, 4, 0, 0));
    assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 4, 0, 0)));
}

#[test]
fn earliest_from_skips_to_saturday_midnight_across_a_weekend_gap() {
    let friday = utc_secs(2024, 1, 5, 21, 35);
    let mut bars = make_bars(5, friday);
    assert_eq!(bars[4].open_time, utc_secs(2024, 1, 5, 21, 55));
    bars.extend(make_bars(10, utc_secs(2024, 1, 7, 22, 0)));
    assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 6, 0, 0)));
}

#[test]
fn earliest_from_edge_cases() {
    let bars = make_bars(5, utc_secs(2024, 1, 3, 22, 0));
    assert_eq!(
        earliest_from(&bars, 0),
        Some(utc_secs(2024, 1, 3, 0, 0)),
        "warmup 0 → date of the first bar"
    );
    assert_eq!(
        earliest_from(&bars, 5),
        Some(utc_secs(2024, 1, 4, 0, 0)),
        "exactly warmup_bars bars → next midnight after the last of them"
    );
    assert_eq!(earliest_from(&bars, 6), None, "fewer than warmup_bars bars");
    assert_eq!(earliest_from(&[], 0), None);
}

#[test]
fn new_accepts_earliest_from_and_rejects_one_day_earlier_naming_that_date() {
    let warmup_bars = 5;
    let all_bars = make_bars(60, utc_secs(2024, 1, 3, 22, 0));
    let earliest = earliest_from(&all_bars, warmup_bars).expect("enough bars");
    let to = all_bars.last().unwrap().open_time + M5_SECS;

    Dataset::new(all_bars.clone(), earliest, to, warmup_bars, 64)
        .expect("earliest_from itself must be accepted");

    let err = Dataset::new(all_bars, earliest - SECS_PER_DAY, to, warmup_bars, 64)
        .expect_err("one day earlier has too few warmup bars");
    assert!(matches!(err, SimError::Args(_)));
    let message = err.to_string();
    assert!(
        message.contains("2024-01-04"),
        "message should name the earliest `from` date (2024-01-04): {message}"
    );
}

#[test]
fn new_rejects_exactly_one_warmup_bar_short_and_accepts_exactly_enough() {
    let warmup_bars = 5;
    let mut all_bars = make_bars(4, utc_secs(2024, 1, 3, 23, 40));
    all_bars.push(bar_at_secs(utc_secs(2024, 1, 4, 12, 0), 150.0));
    all_bars.extend(make_bars(10, utc_secs(2024, 1, 5, 0, 0)));
    let to = all_bars.last().unwrap().open_time + M5_SECS;
    let earliest = earliest_from(&all_bars, warmup_bars).expect("enough bars in total");
    assert_eq!(earliest, utc_secs(2024, 1, 5, 0, 0));

    let one_short_from = utc_secs(2024, 1, 4, 0, 0);
    let err = Dataset::new(all_bars.clone(), one_short_from, to, warmup_bars, 64)
        .expect_err("4 bars before `from` is one short of warmup_bars = 5");
    assert!(matches!(err, SimError::Args(_)));
    let message = err.to_string();
    assert!(
        message.contains("2024-01-05"),
        "message should name the earliest `from` date (2024-01-05): {message}"
    );

    let dataset = Dataset::new(all_bars, earliest, to, warmup_bars, 64)
        .expect("exactly warmup_bars bars before `from` must succeed");
    assert_eq!(dataset.eval_start(), warmup_bars);
}

#[test]
fn new_fails_when_evaluation_period_has_no_bars() {
    let warmup_bars = 2;
    let all_bars = make_bars(warmup_bars + 3, 0);
    let last_open = all_bars.last().unwrap().open_time;
    let from = last_open + 10 * M5_SECS;
    let to = from + M5_SECS;

    let err = Dataset::new(all_bars, from, to, warmup_bars, 64)
        .expect_err("empty evaluation period must be rejected");
    assert!(matches!(err, SimError::Args(_)));
}

#[test]
fn missing_weekdays_reports_only_weekdays_without_bars() {
    let monday = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();
    let warmup_bars = 1;
    let all_bars = vec![
        bar_at_secs(monday - 5 * MIN, 149.0),
        bar_at_secs(monday, 150.0),
    ];
    let from = monday;
    let to = monday + 7 * 24 * 3600;

    let dataset = Dataset::new(all_bars, from, to, warmup_bars, 64).unwrap();
    let missing = dataset.missing_weekdays();
    let expected: Vec<NaiveDate> = vec![
        NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(),
        NaiveDate::from_ymd_opt(2024, 1, 3).unwrap(),
        NaiveDate::from_ymd_opt(2024, 1, 4).unwrap(),
        NaiveDate::from_ymd_opt(2024, 1, 5).unwrap(),
    ];
    assert_eq!(missing, expected);
}

#[test]
fn indicator_delegates_to_the_series_for_the_requested_timeframe() {
    let warmup_bars = 5;
    let all_bars = make_bars(warmup_bars + 10, 0);
    let from = all_bars[warmup_bars].open_time;
    let to = all_bars.last().unwrap().open_time + M5_SECS;
    let dataset = Dataset::new(all_bars, from, to, warmup_bars, 64).unwrap();

    let key = IndicatorKey {
        tf: Tf::M5,
        kind: IndicatorKind::Sma,
        period: 3,
        mult_x100: 0,
    };
    let result = dataset.indicator(key);
    match &*result {
        IndicatorSeries::Single(values) => {
            assert_eq!(values.len(), dataset.series(Tf::M5).close.len());
        }
        _ => panic!("sma must be a Single series"),
    }
}

#[test]
fn tf_parse_roundtrips_known_strings_and_rejects_unknown() {
    assert_eq!(Tf::parse("M5"), Some(Tf::M5));
    assert_eq!(Tf::parse("M15"), Some(Tf::M15));
    assert_eq!(Tf::parse("H1"), Some(Tf::H1));
    assert_eq!(Tf::parse("H4"), Some(Tf::H4));
    assert_eq!(Tf::parse("D1"), None);
    assert_eq!(Tf::parse(""), None);
}

#[test]
fn tf_secs_match_spec() {
    assert_eq!(Tf::M5.secs(), 300);
    assert_eq!(Tf::M15.secs(), 900);
    assert_eq!(Tf::H1.secs(), 3600);
    assert_eq!(Tf::H4.secs(), 14400);
}
