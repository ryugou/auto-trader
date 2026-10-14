mod oracle {
    use auto_trader_market::indicators as market;
    use rust_decimal::Decimal;
    use rust_decimal::prelude::FromPrimitive;

    use super::super::*;
    use crate::series::{Tf, TfSeries};

    fn deterministic_series(n: usize) -> TfSeries {
        let close: Vec<f64> = (0..n)
            .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
            .collect();
        let high: Vec<f64> = close.iter().map(|c| c + 0.05).collect();
        let low: Vec<f64> = close.iter().map(|c| c - 0.05).collect();
        let open: Vec<f64> = (0..n)
            .map(|i| if i == 0 { close[0] } else { close[i - 1] })
            .collect();
        let end_time: Vec<i64> = (0..n).map(|i| (i as i64 + 1) * 900).collect();
        TfSeries {
            open,
            high,
            low,
            close,
            end_time,
        }
    }

    pub(super) fn to_decimal(v: &[f64]) -> Vec<Decimal> {
        v.iter()
            .map(|&x| Decimal::from_f64(x).expect("finite test price"))
            .collect()
    }

    const MARKET_DECIMAL_TOLERANCE: f64 = 1e-6;

    pub(super) fn assert_close(actual: f64, expected: Option<Decimal>, label: &str, i: usize) {
        match expected {
            None => assert!(
                actual.is_nan(),
                "{label} at {i}: expected NaN (market returned None), got {actual}"
            ),
            Some(dec) => {
                let expected_f64 = rust_decimal::prelude::ToPrimitive::to_f64(&dec)
                    .expect("decimal result fits in f64");
                assert!(
                    (actual - expected_f64).abs() <= MARKET_DECIMAL_TOLERANCE,
                    "{label} at {i}: expected {expected_f64}, got {actual} (diff {})",
                    (actual - expected_f64).abs()
                );
            }
        }
    }

    const PERIODS: [usize; 3] = [2, 14, 50];

    fn check_indices(n: usize, periods: &[usize]) -> Vec<(usize, usize)> {
        periods
            .iter()
            .flat_map(|&period| (0..n).map(move |i| (period, i)))
            .collect()
    }

    #[test]
    fn sma_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Sma,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("sma must produce Single");
            };
            let expected = market::sma(&closes_dec[..=i], period);
            assert_close(values[i], expected, "sma", i);
        }
    }

    #[test]
    fn ema_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Ema,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("ema must produce Single");
            };
            let expected = market::ema(&closes_dec[..=i], period);
            assert_close(values[i], expected, "ema", i);
        }
    }

    #[test]
    fn rsi_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Rsi,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("rsi must produce Single");
            };
            let expected = market::rsi(&closes_dec[..=i], period);
            assert_close(values[i], expected, "rsi", i);
        }
    }

    #[test]
    fn atr_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Atr,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("atr must produce Single");
            };
            let expected =
                market::atr(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "atr", i);
        }
    }

    #[test]
    fn adx_matches_market_within_tolerance() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Adx,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Single(values) = result else {
                panic!("adx must produce Single");
            };
            let expected =
                market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "adx", i);
        }
    }

    #[test]
    fn adx_matches_market_when_a_fully_flat_stretch_precedes_the_movement() {
        let flat_bars = 80;
        let n = 400;
        let moving = deterministic_series(n - flat_bars);
        let mut series = TfSeries {
            open: vec![150.0; flat_bars],
            high: vec![150.0; flat_bars],
            low: vec![150.0; flat_bars],
            close: vec![150.0; flat_bars],
            end_time: (0..flat_bars).map(|i| (i as i64 + 1) * 900).collect(),
        };
        series.open.extend(&moving.open);
        series.high.extend(&moving.high);
        series.low.extend(&moving.low);
        series.close.extend(&moving.close);
        series
            .end_time
            .extend((flat_bars..n).map(|i| (i as i64 + 1) * 900));
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Adx,
                period: period as u32,
                mult_x100: 0,
            };
            let IndicatorSeries::Single(values) = compute(&series, key) else {
                panic!("adx must produce Single");
            };
            let expected =
                market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
            assert_close(values[i], expected, "adx(flat prefix)", i);
        }
    }

    #[test]
    fn bb_matches_market_within_tolerance_and_keys_map_to_spec_names() {
        let n = 400;
        let series = deterministic_series(n);
        let closes_dec = to_decimal(&series.close);
        let mult_dec = Decimal::from_f64(2.0).unwrap();
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Bb,
                period: period as u32,
                mult_x100: 200,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Band {
                lower,
                middle,
                upper,
            } = result
            else {
                panic!("bb must produce Band");
            };
            let expected = market::bollinger_bands(&closes_dec[..=i], period, mult_dec);
            assert_close(lower[i], expected.map(|(l, _, _)| l), "bb.lower", i);
            assert_close(middle[i], expected.map(|(_, m, _)| m), "bb.middle", i);
            assert_close(upper[i], expected.map(|(_, _, u)| u), "bb.upper", i);
        }
    }

    fn random_walk_closes_long_enough_to_surface_the_cancellation_error_a_400_bar_deterministic_series_would_miss(
        n: usize,
    ) -> Vec<f64> {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut mid2: i64 = 300_000;
        let mut closes = Vec::with_capacity(n);
        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let r = (state >> 33) as i64;
            let step = if r % 3 == 0 { 0 } else { (r / 3) % 7 - 3 };
            mid2 += step;
            closes.push(mid2 as f64 / 2000.0);
        }
        closes
    }

    #[test]
    fn bb_matches_market_on_long_random_walk_without_cancellation_error() {
        let n = 100_000;
        let tail = 2_000;
        let closes =
            random_walk_closes_long_enough_to_surface_the_cancellation_error_a_400_bar_deterministic_series_would_miss(
                n,
            );
        let series = TfSeries {
            open: closes.clone(),
            high: closes.clone(),
            low: closes.clone(),
            close: closes.clone(),
            end_time: (0..n).map(|i| (i as i64 + 1) * 900).collect(),
        };
        let closes_dec = to_decimal(&closes);
        for period in [2usize, 14, 50] {
            for mult_x100 in [200u32, 1000] {
                let key = IndicatorKey {
                    tf: Tf::M15,
                    kind: IndicatorKind::Bb,
                    period: period as u32,
                    mult_x100,
                };
                let IndicatorSeries::Band {
                    lower,
                    middle,
                    upper,
                } = compute(&series, key)
                else {
                    panic!("bb must produce Band");
                };
                let mult_dec = Decimal::from(mult_x100) / Decimal::from(100);
                for i in (n - tail)..n {
                    let expected = market::bollinger_bands(&closes_dec[..=i], period, mult_dec);
                    let label = format!("bb(period={period}, mult_x100={mult_x100})");
                    assert_close(
                        lower[i],
                        expected.map(|(l, _, _)| l),
                        &format!("{label}.lower"),
                        i,
                    );
                    assert_close(
                        middle[i],
                        expected.map(|(_, m, _)| m),
                        &format!("{label}.middle"),
                        i,
                    );
                    assert_close(
                        upper[i],
                        expected.map(|(_, _, u)| u),
                        &format!("{label}.upper"),
                        i,
                    );
                }
            }
        }
    }

    #[test]
    fn donchian_matches_market_include_current_true() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Donchian,
                period: period as u32,
                mult_x100: 0,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Channel { lower, upper } = result else {
                panic!("donchian must produce Channel");
            };
            let expected =
                market::donchian_channel(&highs_dec[..=i], &lows_dec[..=i], period, true);
            assert_close(lower[i], expected.map(|(l, _)| l), "donchian.lower", i);
            assert_close(upper[i], expected.map(|(_, u)| u), "donchian.upper", i);
        }
    }

    #[test]
    fn keltner_matches_market_within_tolerance_and_keys_map_to_spec_names() {
        let n = 400;
        let series = deterministic_series(n);
        let highs_dec = to_decimal(&series.high);
        let lows_dec = to_decimal(&series.low);
        let closes_dec = to_decimal(&series.close);
        let mult_dec = Decimal::from_f64(2.0).unwrap();
        for (period, i) in check_indices(n, &PERIODS) {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind: IndicatorKind::Keltner,
                period: period as u32,
                mult_x100: 200,
            };
            let result = compute(&series, key);
            let IndicatorSeries::Band {
                lower,
                middle,
                upper,
            } = result
            else {
                panic!("keltner must produce Band");
            };
            let expected = market::keltner_channels(
                &highs_dec[..=i],
                &lows_dec[..=i],
                &closes_dec[..=i],
                period,
                mult_dec,
            );
            assert_close(lower[i], expected.map(|(l, _, _)| l), "keltner.lower", i);
            assert_close(middle[i], expected.map(|(_, m, _)| m), "keltner.middle", i);
            assert_close(upper[i], expected.map(|(_, _, u)| u), "keltner.upper", i);
        }
    }

    #[test]
    fn compute_with_period_zero_yields_an_all_nan_series_for_every_kind() {
        let n = 10;
        let series = deterministic_series(n);
        let kinds = [
            IndicatorKind::Sma,
            IndicatorKind::Ema,
            IndicatorKind::Rsi,
            IndicatorKind::Atr,
            IndicatorKind::Adx,
            IndicatorKind::Bb,
            IndicatorKind::Donchian,
            IndicatorKind::Keltner,
        ];
        for kind in kinds {
            let key = IndicatorKey {
                tf: Tf::M15,
                kind,
                period: 0,
                mult_x100: 200,
            };
            let result = compute(&series, key);
            let component_series: Vec<&[f64]> = match &result {
                IndicatorSeries::Single(v) => vec![v.as_slice()],
                IndicatorSeries::Band {
                    lower,
                    middle,
                    upper,
                } => vec![lower.as_slice(), middle.as_slice(), upper.as_slice()],
                IndicatorSeries::Channel { lower, upper } => {
                    vec![lower.as_slice(), upper.as_slice()]
                }
            };
            for values in component_series {
                assert!(
                    values.iter().all(|v| v.is_nan()),
                    "{kind:?} with period=0 must be all NaN, got {values:?}"
                );
            }
        }
    }
}

mod rsi_walk {
    use auto_trader_market::indicators as market;

    use super::super::*;
    use super::oracle::{assert_close, to_decimal};

    fn closes_from_mid2(mid2: &[i64]) -> Vec<f64> {
        mid2.iter().map(|&m| m as f64 / 2000.0).collect()
    }

    fn assert_rsi_matches_market_at_every_index(mid2: &[i64], label: &str) {
        let closes = closes_from_mid2(mid2);
        let closes_dec = to_decimal(&closes);
        for period in [2usize, 3, 14] {
            let values = rsi_series(&closes, period);
            for i in 0..closes.len() {
                let expected = market::rsi(&closes_dec[..=i], period);
                assert_close(
                    values[i],
                    expected,
                    &format!("rsi[{label}, period={period}]"),
                    i,
                );
            }
        }
    }

    fn walk(start: i64, steps: &[(usize, i64)]) -> Vec<i64> {
        let mut out = vec![start];
        let mut cur = start;
        for &(count, step) in steps {
            for _ in 0..count {
                cur += step;
                out.push(cur);
            }
        }
        out
    }

    #[test]
    fn rsi_is_100_after_declines_followed_by_a_long_run_of_gains() {
        let mid2 = walk(300_003, &[(2, -70_001), (10, -1), (40, 3)]);
        assert_rsi_matches_market_at_every_index(&mid2, "decline-then-gains");
    }

    #[test]
    fn rsi_matches_market_across_fully_flat_stretches() {
        let mid2 = walk(
            300_003,
            &[
                (3, 70_001),
                (2, -50_001),
                (3, 1),
                (20, 0),
                (6, -5),
                (20, 0),
                (8, 7),
            ],
        );
        assert_rsi_matches_market_at_every_index(&mid2, "flat-stretches");
    }

    #[test]
    fn rsi_matches_market_on_blocks_of_flat_up_down_with_mixed_magnitude_steps() {
        let mut state: u64 = 0x1234_5678_9abc_def0;
        let mut mid2: i64 = 300_003;
        let mut series = Vec::new();
        for block in 0..400 {
            for _ in 0..30 {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let r = ((state >> 33) as i64).abs();
                let magnitude = if r % 5 == 0 { r % 100_000 } else { r % 3 };
                mid2 += match block % 4 {
                    0 => 0,
                    1 => -magnitude,
                    2 => magnitude,
                    _ => (r % 1301) - 600,
                };
                series.push(mid2);
            }
        }
        assert_rsi_matches_market_at_every_index(&series, "mixed-magnitude-blocks");
    }

    #[test]
    fn rsi_matches_market_for_windows_with_no_gains_only_declines_and_flats() {
        let mid2 = walk(
            300_003,
            &[(10, 3), (3, -70_001), (5, 0), (2, -11), (15, 0), (10, 5)],
        );
        assert_rsi_matches_market_at_every_index(&mid2, "no-gain-windows");
    }
}
