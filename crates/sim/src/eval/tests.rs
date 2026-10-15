use super::*;
use crate::engine::RunStatus;
use crate::types::M5_SECS;

fn bar_mid2_close(idx: usize, mid2_close: i64) -> Bar {
    let bid = mid2_close / 2;
    let ask = mid2_close - bid;
    Bar {
        open_time: idx as i64 * M5_SECS,
        bid_open: bid,
        bid_high: bid,
        bid_low: bid,
        bid_close: bid,
        ask_open: ask,
        ask_high: ask,
        ask_low: ask,
        ask_close: ask,
    }
}

fn bars(n: usize) -> Vec<Bar> {
    (0..n).map(|i| bar_mid2_close(i, 300_000)).collect()
}

fn trade(pnl_pips: f64, exit_idx: usize, reason: ExitReason) -> SimTrade {
    SimTrade {
        direction: 1,
        entry_idx: exit_idx,
        exit_idx,
        entry_milli: 0,
        exit_milli: (pnl_pips * 10.0).round() as i64,
        pnl_milli: (pnl_pips * 10.0).round() as i64,
        reason,
    }
}

fn outcome(trades: Vec<SimTrade>, positions: Vec<i8>) -> SimOutcome {
    SimOutcome {
        status: RunStatus::Ok,
        trades,
        positions,
    }
}

fn empty_benchmark(theta_pips: i64, n: usize) -> Benchmark {
    Benchmark {
        theta_pips,
        legs: vec![],
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    }
}

#[test]
fn three_trades_produce_total_pips_trade_count_win_rate_and_drawdown() {
    let trades = vec![
        trade(30.0, 0, ExitReason::Signal),
        trade(-10.0, 1, ExitReason::Signal),
        trade(20.0, 2, ExitReason::Signal),
    ];
    let metrics = evaluate(&bars(3), &outcome(trades, vec![1, 1, 1]), &[]);

    assert_eq!(metrics.total_pips, 40.0);
    assert_eq!(metrics.trade_count, 3);
    assert!((metrics.win_rate - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(metrics.max_drawdown_pips, 10.0);
}

#[test]
fn drawdown_from_initial_zero_peak_when_first_trade_is_a_loss() {
    let trades = vec![trade(-15.0, 0, ExitReason::Signal)];
    let metrics = evaluate(&bars(1), &outcome(trades, vec![1]), &[]);

    assert_eq!(metrics.max_drawdown_pips, 15.0);
}

#[test]
fn zero_trades_yield_zero_total_pips_win_rate_and_drawdown() {
    let metrics = evaluate(&bars(2), &outcome(vec![], vec![0, 0]), &[]);

    assert_eq!(metrics.total_pips, 0.0);
    assert_eq!(metrics.trade_count, 0);
    assert_eq!(metrics.win_rate, 0.0);
    assert_eq!(metrics.max_drawdown_pips, 0.0);
}

#[test]
fn time_in_market_is_the_fraction_of_bars_with_a_nonzero_position() {
    let positions = vec![0, 1, 1, 0, -1];
    let metrics = evaluate(&bars(5), &outcome(vec![], positions), &[]);

    assert_eq!(metrics.time_in_market, 0.6);
}

#[test]
fn protective_stop_count_counts_only_protective_stop_exits() {
    let trades = vec![
        trade(10.0, 0, ExitReason::Signal),
        trade(-5.0, 1, ExitReason::ProtectiveStop),
        trade(3.0, 2, ExitReason::EndOfData),
        trade(-2.0, 3, ExitReason::ProtectiveStop),
    ];
    let metrics = evaluate(&bars(4), &outcome(trades, vec![1, 1, 1, 1]), &[]);

    assert_eq!(metrics.protective_stop_count, 2);
}

#[test]
fn segments_split_evaluation_period_into_six_parts_with_remainder_in_the_last() {
    let trades = vec![
        trade(1.0, 1, ExitReason::Signal),
        trade(2.0, 3, ExitReason::Signal),
        trade(3.0, 5, ExitReason::Signal),
        trade(4.0, 7, ExitReason::Signal),
        trade(5.0, 9, ExitReason::Signal),
        trade(6.0, 12, ExitReason::Signal),
    ];
    let metrics = evaluate(&bars(13), &outcome(trades, vec![0; 13]), &[]);

    assert_eq!(
        metrics.segments,
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        "each trade must be attributed to the segment containing its exit_idx"
    );
}

#[test]
fn fewer_than_six_bars_put_every_trade_in_the_last_segment() {
    let trades = vec![
        trade(1.0, 0, ExitReason::Signal),
        trade(2.0, 4, ExitReason::Signal),
    ];
    let metrics = evaluate(&bars(5), &outcome(trades, vec![0; 5]), &[]);

    assert_eq!(metrics.segments, [0.0, 0.0, 0.0, 0.0, 0.0, 3.0]);
}

fn trade_milli(pnl_milli: i64, exit_idx: usize) -> SimTrade {
    SimTrade {
        direction: 1,
        entry_idx: exit_idx,
        exit_idx,
        entry_milli: 0,
        exit_milli: pnl_milli,
        pnl_milli,
        reason: ExitReason::Signal,
    }
}

#[test]
fn profits_are_summed_in_integer_milli_so_three_tenth_pips_total_exactly_0_3() {
    let trades = vec![trade_milli(1, 6), trade_milli(1, 7), trade_milli(1, 8)];
    let metrics = evaluate(&bars(18), &outcome(trades, vec![0; 18]), &[]);

    assert_eq!(metrics.total_pips, 0.3);
    assert_eq!(metrics.segments[2], 0.3);
}

#[test]
fn drawdown_is_accumulated_in_integer_milli_so_three_tenth_pip_losses_total_exactly_0_3() {
    let trades = vec![trade_milli(-1, 0), trade_milli(-1, 1), trade_milli(-1, 2)];
    let metrics = evaluate(&bars(3), &outcome(trades, vec![0; 3]), &[]);

    assert_eq!(metrics.max_drawdown_pips, 0.3);
}

#[test]
fn capture_rate_divides_total_pips_by_realizable_pips_when_positive() {
    let trades = vec![
        trade(30.0, 0, ExitReason::Signal),
        trade(10.0, 1, ExitReason::Signal),
    ];
    let bm = Benchmark {
        theta_pips: 20,
        legs: vec![],
        ideal_milli: 0,
        realizable_milli: 500,
        labels: vec![0; 2],
    };
    let metrics = evaluate(&bars(2), &outcome(trades, vec![0, 0]), &[bm]);

    let theta = &metrics.by_theta["20"];
    assert_eq!(theta.realizable_pips, 50.0);
    assert_eq!(theta.capture_rate, Some(40.0 / 50.0));
}

#[test]
fn capture_rate_is_none_when_realizable_pips_is_zero_or_negative() {
    let n = 2;
    for realizable_milli in [0i64, -100] {
        let bm = Benchmark {
            theta_pips: 20,
            legs: vec![],
            ideal_milli: 0,
            realizable_milli,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);
        assert_eq!(
            metrics.by_theta["20"].capture_rate, None,
            "realizable_milli={realizable_milli} must yield capture_rate = None"
        );
    }
}

#[test]
fn correct_side_ratio_counts_matches_among_labeled_bars_only() {
    let n = 10;
    let mut labels = vec![0i8; n];
    for l in labels.iter_mut().take(8).skip(2) {
        *l = 1;
    }
    let mut positions = vec![0i8; n];
    positions[2] = 1;
    positions[3] = 1;
    positions[4] = 1;
    positions[5] = 1;
    positions[6] = -1;
    positions[7] = 0;

    let bm = Benchmark {
        theta_pips: 20,
        legs: vec![],
        ideal_milli: 0,
        realizable_milli: 0,
        labels,
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], positions), &[bm]);

    assert_eq!(metrics.by_theta["20"].correct_side_ratio, Some(4.0 / 6.0));
}

#[test]
fn correct_side_ratio_is_none_when_no_bar_is_labeled() {
    let n = 5;
    let metrics = evaluate(
        &bars(n),
        &outcome(vec![], vec![0; n]),
        &[empty_benchmark(20, n)],
    );
    assert_eq!(metrics.by_theta["20"].correct_side_ratio, None);
}

#[test]
fn missed_legs_includes_only_legs_with_match_ratio_below_half_and_excludes_exactly_half() {
    let n = 13;
    let mut positions = vec![0i8; n];
    positions[1] = 1;
    positions[2] = -1;
    positions[3] = 0;
    positions[4] = 0;
    positions[5] = 1;
    positions[6] = 1;
    positions[7] = 0;
    positions[8] = -1;
    positions[9] = 1;
    positions[10] = 1;
    positions[11] = 1;
    positions[12] = 0;

    let legs = vec![
        Leg {
            a: 0,
            b: 4,
            direction: 1,
            ideal_milli: 100,
            realizable_milli: 100,
        },
        Leg {
            a: 4,
            b: 8,
            direction: 1,
            ideal_milli: 100,
            realizable_milli: 100,
        },
        Leg {
            a: 8,
            b: 12,
            direction: 1,
            ideal_milli: 100,
            realizable_milli: 100,
        },
    ];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 300,
        realizable_milli: 300,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], positions), &[bm]);

    let missed = &metrics.by_theta["20"].missed_legs;
    assert_eq!(
        missed.len(),
        1,
        "only the 1/4 leg is below the 0.5 threshold"
    );
    assert_eq!(missed[0].flat_bars, 2);
    assert_eq!(missed[0].opposite_bars, 1);
}

#[test]
fn missed_legs_report_start_time_at_a_plus_1_and_end_time_at_b() {
    let n = 6;
    let legs = vec![Leg {
        a: 1,
        b: 4,
        direction: 1,
        ideal_milli: 100,
        realizable_milli: 100,
    }];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 100,
        realizable_milli: 100,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

    let missed = &metrics.by_theta["20"].missed_legs;
    assert_eq!(missed.len(), 1);
    assert_eq!(missed[0].start_time, "1970-01-01T00:10:00Z");
    assert_eq!(missed[0].end_time, "1970-01-01T00:20:00Z");
    assert_eq!(missed[0].direction, 1);
}

#[test]
fn missed_legs_sorted_by_realizable_pips_descending_and_capped_at_20() {
    let leg_count = 21;
    let n = leg_count * 2 + 1;
    let legs: Vec<Leg> = (0..leg_count)
        .map(|i| Leg {
            a: i * 2,
            b: i * 2 + 2,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: (i as i64 + 1) * 10,
        })
        .collect();
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

    let missed = &metrics.by_theta["20"].missed_legs;
    assert_eq!(missed.len(), 20, "21 candidates must be capped at 20");
    assert_eq!(missed[0].realizable_pips, milli_to_pips(210));
    assert_eq!(
        missed[19].realizable_pips,
        milli_to_pips(20),
        "the smallest candidate (realizable_milli=10) must be dropped"
    );
}

#[test]
fn missed_legs_tie_break_by_start_time_ascending() {
    let n = 6;
    let legs = vec![
        Leg {
            a: 2,
            b: 4,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 100,
        },
        Leg {
            a: 0,
            b: 2,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 100,
        },
    ];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

    let missed = &metrics.by_theta["20"].missed_legs;
    assert_eq!(missed.len(), 2);
    assert_eq!(
        missed[0].start_time,
        rfc3339(M5_SECS),
        "the earlier-starting leg (a=0) must come first despite equal realizable_pips"
    );
}

#[test]
fn mean_lag_averages_over_legs_with_at_least_one_matching_bar() {
    let n = 35;
    let bars_vec: Vec<Bar> = (0..n)
        .map(|i| bar_mid2_close(i, 300_000 + i as i64 * 20))
        .collect();

    let mut positions = vec![0i8; n];
    positions[13] = 1;
    positions[21] = 1;

    let legs = vec![
        Leg {
            a: 10,
            b: 15,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 10,
        },
        Leg {
            a: 20,
            b: 24,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 10,
        },
        Leg {
            a: 30,
            b: 34,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 10,
        },
    ];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars_vec, &outcome(vec![], positions), &[bm]);

    let theta = &metrics.by_theta["20"];
    assert_eq!(theta.mean_lag_bars, Some((2.0 + 0.0) / 2.0));
    assert_eq!(theta.mean_lag_pips, Some((3.0 + 1.0) / 2.0));
}

#[test]
fn mean_lag_pips_is_an_absolute_value_for_a_falling_leg() {
    let n = 10;
    let bars_vec: Vec<Bar> = (0..n)
        .map(|i| bar_mid2_close(i, 300_000 - i as i64 * 20))
        .collect();

    let mut positions = vec![0i8; n];
    positions[4] = -1;

    let legs = vec![Leg {
        a: 2,
        b: 6,
        direction: -1,
        ideal_milli: 0,
        realizable_milli: 10,
    }];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars_vec, &outcome(vec![], positions), &[bm]);

    let theta = &metrics.by_theta["20"];
    assert_eq!(theta.mean_lag_bars, Some(1.0));
    assert_eq!(
        theta.mean_lag_pips,
        Some(2.0),
        "mean_lag_pips must use the absolute price difference, not the signed one"
    );
}

#[test]
fn mean_lag_is_none_when_no_leg_has_a_matching_bar() {
    let n = 6;
    let legs = vec![Leg {
        a: 0,
        b: 4,
        direction: 1,
        ideal_milli: 0,
        realizable_milli: 10,
    }];
    let bm = Benchmark {
        theta_pips: 20,
        legs,
        ideal_milli: 0,
        realizable_milli: 0,
        labels: vec![0; n],
    };
    let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

    let theta = &metrics.by_theta["20"];
    assert_eq!(theta.mean_lag_bars, None);
    assert_eq!(theta.mean_lag_pips, None);
}

#[test]
fn metrics_json_matches_spec_shape_with_null_for_none_and_theta_pips_string_keys() {
    let mut by_theta = BTreeMap::new();
    by_theta.insert(
        "20".to_string(),
        ThetaMetrics {
            leg_count: 0,
            ideal_pips: 0.0,
            realizable_pips: 0.0,
            capture_rate: None,
            correct_side_ratio: None,
            mean_lag_bars: None,
            mean_lag_pips: None,
            missed_legs: vec![MissedLeg {
                start_time: "2024-01-02T03:05:00Z".to_string(),
                end_time: "2024-01-02T07:40:00Z".to_string(),
                direction: 1,
                realizable_pips: 0.0,
                flat_bars: 0,
                opposite_bars: 0,
            }],
        },
    );
    let metrics = Metrics {
        total_pips: 0.0,
        trade_count: 0,
        win_rate: 0.0,
        max_drawdown_pips: 0.0,
        time_in_market: 0.0,
        protective_stop_count: 0,
        segments: [0.0; 6],
        by_theta,
    };

    let json = metrics.metrics_json();
    let expected = serde_json::json!({
        "segments": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "by_theta": {
            "20": {
                "leg_count": 0,
                "ideal_pips": 0.0,
                "realizable_pips": 0.0,
                "capture_rate": null,
                "correct_side_ratio": null,
                "mean_lag_bars": null,
                "mean_lag_pips": null,
                "missed_legs": [
                    {
                        "start_time": "2024-01-02T03:05:00Z",
                        "end_time": "2024-01-02T07:40:00Z",
                        "direction": 1,
                        "realizable_pips": 0.0,
                        "flat_bars": 0,
                        "opposite_bars": 0
                    }
                ]
            }
        }
    });
    assert_eq!(json, expected);
}

#[test]
fn zero_leg_benchmark_does_not_panic_and_yields_none_options() {
    let n = 5;
    let metrics = evaluate(
        &bars(n),
        &outcome(vec![], vec![0; n]),
        &[empty_benchmark(20, n)],
    );

    let theta = &metrics.by_theta["20"];
    assert_eq!(theta.leg_count, 0);
    assert_eq!(theta.ideal_pips, 0.0);
    assert_eq!(theta.realizable_pips, 0.0);
    assert_eq!(theta.capture_rate, None);
    assert_eq!(theta.correct_side_ratio, None);
    assert_eq!(theta.mean_lag_bars, None);
    assert_eq!(theta.mean_lag_pips, None);
    assert!(theta.missed_legs.is_empty());
}
