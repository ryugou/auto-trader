use auto_trader_market::indicators as market;
use rust_decimal::Decimal;
use rust_decimal::prelude::FromPrimitive;

use super::*;
use crate::series::TfSeries;

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

fn to_decimal(v: &[f64]) -> Vec<Decimal> {
    v.iter()
        .map(|&x| Decimal::from_f64(x).expect("finite test price"))
        .collect()
}

const MARKET_DECIMAL_TOLERANCE: f64 = 1e-6;

fn assert_close(actual: f64, expected: Option<Decimal>, label: &str, i: usize) {
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
        let expected = market::atr(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
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
        let expected = market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
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
        let expected = market::adx(&highs_dec[..=i], &lows_dec[..=i], &closes_dec[..=i], period);
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
        let expected = market::donchian_channel(&highs_dec[..=i], &lows_dec[..=i], period, true);
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

fn single_series(v: Vec<f64>) -> IndicatorSeries {
    IndicatorSeries::Single(v)
}

#[test]
fn get_or_compute_returns_the_same_arc_for_the_same_key() {
    let cache = IndicatorCache::new(512);
    let key = IndicatorKey {
        tf: Tf::M5,
        kind: IndicatorKind::Sma,
        period: 10,
        mult_x100: 0,
    };
    let first = cache.get_or_compute(key, || single_series(vec![1.0, 2.0, 3.0]));
    let second = cache.get_or_compute(key, || single_series(vec![9.0, 9.0, 9.0]));
    assert!(
        Arc::ptr_eq(&first, &second),
        "second call must return the cached Arc, not recompute"
    );
}

#[test]
fn concurrent_requests_for_the_same_key_compute_only_once() {
    let cache = Arc::new(IndicatorCache::new(512));
    let key = IndicatorKey {
        tf: Tf::M5,
        kind: IndicatorKind::Sma,
        period: 10,
        mult_x100: 0,
    };
    let compute_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let cache = cache.clone();
            let compute_count = compute_count.clone();
            std::thread::spawn(move || {
                cache.get_or_compute(key, || {
                    compute_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    single_series(vec![42.0])
                })
            })
        })
        .collect();
    for h in handles {
        h.join().expect("worker thread should not panic");
    }

    assert_eq!(
        compute_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the same key must be computed exactly once across concurrent requests"
    );
}

#[test]
fn eviction_drops_the_least_recently_used_key_and_recompute_matches_original() {
    let cache = IndicatorCache::new(1);
    let big = vec![1.23456_f64; 50_000];
    let key_of = |period: u32| IndicatorKey {
        tf: Tf::M5,
        kind: IndicatorKind::Sma,
        period,
        mult_x100: 0,
    };

    let first_value = cache.get_or_compute(key_of(1), || single_series(big.clone()));
    for period in 2..=6 {
        cache.get_or_compute(key_of(period), || single_series(big.clone()));
    }

    let recomputed = cache.get_or_compute(key_of(1), || single_series(big.clone()));
    match (&*first_value, &*recomputed) {
        (IndicatorSeries::Single(a), IndicatorSeries::Single(b)) => assert_eq!(a, b),
        _ => panic!("unexpected indicator series variant"),
    }
    assert!(
        !Arc::ptr_eq(&first_value, &recomputed),
        "key_of(1) should have been evicted and recomputed, not reused from cache"
    );
}

fn sma_key(period: u32) -> IndicatorKey {
    IndicatorKey {
        tf: Tf::M5,
        kind: IndicatorKind::Sma,
        period,
        mult_x100: 0,
    }
}

fn assert_used_bytes_matches_entries(cache: &IndicatorCache, context: &str) {
    let guard = cache.inner.lock().expect("test cache mutex");
    let sum: usize = guard.map.values().map(|e| e.bytes).sum();
    assert_eq!(
        guard.used_bytes, sum,
        "used_bytes must equal the sum of entry bytes ({context})"
    );
}

const CONCURRENCY_TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn assert_other_work_completes_while_key_a_is_computing(
    cache: Arc<IndicatorCache>,
    other: impl FnOnce(&IndicatorCache) + Send + 'static,
) {
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let slow = {
        let cache = cache.clone();
        std::thread::spawn(move || {
            cache.get_or_compute(sma_key(1), || {
                started_tx.send(()).expect("main thread is waiting");
                let _ = release_rx.recv_timeout(CONCURRENCY_TEST_TIMEOUT);
                single_series(vec![2.0])
            })
        })
    };
    started_rx
        .recv_timeout(CONCURRENCY_TEST_TIMEOUT)
        .expect("key A's computation did not start within the timeout");

    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let other_thread = {
        let cache = cache.clone();
        std::thread::spawn(move || {
            other(&cache);
            let _ = done_tx.send(());
        })
    };
    let outcome = done_rx.recv_timeout(CONCURRENCY_TEST_TIMEOUT);

    release_tx.send(()).expect("key A's computation is waiting");
    slow.join().expect("slow worker should not panic");
    other_thread.join().expect("other worker should not panic");
    outcome.expect(
        "other work did not finish while key A was still computing: \
         the cache is blocking on key A's computation",
    );
}

#[test]
fn a_slow_computation_for_one_key_does_not_block_a_cache_hit_for_another_key() {
    let cache = Arc::new(IndicatorCache::new(512));
    cache.get_or_compute(sma_key(2), || single_series(vec![1.0]));

    assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
        cache.get_or_compute(sma_key(2), || panic!("key B is cached, must not recompute"));
    });
}

#[test]
fn a_computation_for_one_key_does_not_block_a_computation_for_another_key() {
    let cache = Arc::new(IndicatorCache::new(512));

    assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
        cache.get_or_compute(sma_key(3), || single_series(vec![1.0]));
    });
}

#[test]
fn a_series_larger_than_the_budget_is_returned_but_not_retained() {
    let cache = IndicatorCache::new(1);
    let oversized = vec![1.0_f64; 200_000];
    let compute_count = std::cell::Cell::new(0_usize);

    let first = cache.get_or_compute(sma_key(1), || {
        compute_count.set(compute_count.get() + 1);
        single_series(oversized.clone())
    });
    match &*first {
        IndicatorSeries::Single(v) => assert_eq!(v.len(), 200_000),
        _ => panic!("unexpected indicator series variant"),
    }
    {
        let guard = cache.inner.lock().expect("test cache mutex");
        assert!(
            !guard.map.contains_key(&sma_key(1)),
            "an oversized series must not stay in the cache"
        );
        assert_eq!(guard.used_bytes, 0, "used_bytes must not exceed the budget");
    }
    assert_used_bytes_matches_entries(&cache, "after oversized request");

    cache.get_or_compute(sma_key(1), || {
        compute_count.set(compute_count.get() + 1);
        single_series(oversized.clone())
    });
    assert_eq!(
        compute_count.get(),
        2,
        "an oversized series is not cached, so the next request recomputes"
    );
}

#[test]
fn a_panicking_computation_can_be_retried_and_leaves_the_cache_consistent() {
    let cache = IndicatorCache::new(512);
    let compute_count = std::cell::Cell::new(0_usize);

    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cache.get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            panic!("intentional panic in compute_fn (expected by this test)")
        })
    }));
    assert!(first.is_err(), "the first request must propagate the panic");

    let second = cache.get_or_compute(sma_key(1), || {
        compute_count.set(compute_count.get() + 1);
        single_series(vec![7.0])
    });
    match &*second {
        IndicatorSeries::Single(v) => assert_eq!(v, &vec![7.0]),
        _ => panic!("unexpected indicator series variant"),
    }
    assert_eq!(compute_count.get(), 2, "the retry must recompute once");
    assert!(
        cache.inner.lock().is_ok(),
        "a panic in compute_fn must not poison the cache mutex"
    );
    assert_used_bytes_matches_entries(&cache, "after panic and retry");
}

#[test]
fn used_bytes_equals_the_sum_of_entry_bytes_after_every_operation() {
    let cache = IndicatorCache::new(1);
    let big = vec![1.0_f64; 50_000];
    assert_used_bytes_matches_entries(&cache, "empty cache");
    for period in 1..=8 {
        cache.get_or_compute(sma_key(period), || single_series(big.clone()));
        assert_used_bytes_matches_entries(&cache, &format!("after insert {period}"));
        cache.get_or_compute(sma_key(period), || single_series(big.clone()));
        assert_used_bytes_matches_entries(&cache, &format!("after hit {period}"));
    }
}

#[test]
fn a_cache_hit_protects_the_key_from_being_evicted_first() {
    let cache = IndicatorCache::new(1);
    let big = vec![1.0_f64; 50_000];
    let key1 = cache.get_or_compute(sma_key(1), || single_series(big.clone()));
    cache.get_or_compute(sma_key(2), || single_series(big.clone()));
    cache.get_or_compute(sma_key(1), || panic!("key 1 is cached, must not recompute"));
    cache.get_or_compute(sma_key(3), || single_series(big.clone()));

    let key1_again = cache.get_or_compute(sma_key(1), || {
        panic!("key 1 was used recently, must survive")
    });
    assert!(Arc::ptr_eq(&key1, &key1_again));
    let recomputed = std::cell::Cell::new(false);
    cache.get_or_compute(sma_key(2), || {
        recomputed.set(true);
        single_series(big.clone())
    });
    assert!(
        recomputed.get(),
        "key 2 was least recently used and must have been evicted"
    );
    assert_used_bytes_matches_entries(&cache, "after eviction");
}

#[test]
fn nothing_is_evicted_while_within_budget() {
    let cache = IndicatorCache::new(1);
    let big = vec![1.0_f64; 50_000];
    let first = cache.get_or_compute(sma_key(1), || single_series(big.clone()));
    let second = cache.get_or_compute(sma_key(2), || single_series(big.clone()));

    let first_again = cache.get_or_compute(sma_key(1), || panic!("must not be evicted"));
    let second_again = cache.get_or_compute(sma_key(2), || panic!("must not be evicted"));
    assert!(Arc::ptr_eq(&first, &first_again));
    assert!(Arc::ptr_eq(&second, &second_again));
    assert_eq!(cache.inner.lock().expect("test cache mutex").map.len(), 2);
}
