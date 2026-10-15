use super::*;
use crate::indicators::types::IndicatorKind;

fn single_series(v: Vec<f64>) -> IndicatorSeries {
    IndicatorSeries::Single(v)
}

fn sma_key(period: u32) -> IndicatorKey {
    IndicatorKey {
        tf: crate::series::Tf::M5,
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
    slow.join().expect("slow worker should not panic").unwrap();
    other_thread.join().expect("other worker should not panic");
    outcome.expect(
        "other work did not finish while key A was still computing: \
         the cache is blocking on key A's computation",
    );
}

#[test]
fn get_or_compute_returns_the_same_arc_for_the_same_key() {
    let cache = IndicatorCache::new(512);
    let key = sma_key(10);
    let first = cache
        .get_or_compute(key, || single_series(vec![1.0, 2.0, 3.0]))
        .unwrap();
    let second = cache
        .get_or_compute(key, || single_series(vec![9.0, 9.0, 9.0]))
        .unwrap();
    assert!(
        Arc::ptr_eq(&first, &second),
        "second call must return the cached Arc, not recompute"
    );
}

#[test]
fn concurrent_requests_for_the_same_key_compute_only_once() {
    let cache = Arc::new(IndicatorCache::new(512));
    let key = sma_key(10);
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
        h.join().expect("worker thread should not panic").unwrap();
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

    let first_value = cache
        .get_or_compute(sma_key(1), || single_series(big.clone()))
        .unwrap();
    for period in 2..=6 {
        cache
            .get_or_compute(sma_key(period), || single_series(big.clone()))
            .unwrap();
    }

    let recomputed = cache
        .get_or_compute(sma_key(1), || single_series(big.clone()))
        .unwrap();
    match (&*first_value, &*recomputed) {
        (IndicatorSeries::Single(a), IndicatorSeries::Single(b)) => assert_eq!(a, b),
        _ => panic!("unexpected indicator series variant"),
    }
    assert!(
        !Arc::ptr_eq(&first_value, &recomputed),
        "key_of(1) should have been evicted and recomputed, not reused from cache"
    );
}

#[test]
fn a_slow_computation_for_one_key_does_not_block_a_cache_hit_for_another_key() {
    let cache = Arc::new(IndicatorCache::new(512));
    cache
        .get_or_compute(sma_key(2), || single_series(vec![1.0]))
        .unwrap();

    assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
        cache
            .get_or_compute(sma_key(2), || panic!("key B is cached, must not recompute"))
            .unwrap();
    });
}

#[test]
fn a_computation_for_one_key_does_not_block_a_computation_for_another_key() {
    let cache = Arc::new(IndicatorCache::new(512));

    assert_other_work_completes_while_key_a_is_computing(cache, |cache| {
        cache
            .get_or_compute(sma_key(3), || single_series(vec![1.0]))
            .unwrap();
    });
}

#[test]
fn a_series_larger_than_the_budget_is_returned_but_not_retained() {
    let cache = IndicatorCache::new(1);
    let oversized = vec![1.0_f64; 200_000];
    let compute_count = std::cell::Cell::new(0_usize);

    let first = cache
        .get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(oversized.clone())
        })
        .unwrap();
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

    cache
        .get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(oversized.clone())
        })
        .unwrap();
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

    let second = cache
        .get_or_compute(sma_key(1), || {
            compute_count.set(compute_count.get() + 1);
            single_series(vec![7.0])
        })
        .unwrap();
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
        cache
            .get_or_compute(sma_key(period), || single_series(big.clone()))
            .unwrap();
        assert_used_bytes_matches_entries(&cache, &format!("after insert {period}"));
        cache
            .get_or_compute(sma_key(period), || single_series(big.clone()))
            .unwrap();
        assert_used_bytes_matches_entries(&cache, &format!("after hit {period}"));
    }
}

#[test]
fn a_cache_hit_protects_the_key_from_being_evicted_first() {
    let cache = IndicatorCache::new(1);
    let big = vec![1.0_f64; 50_000];
    let key1 = cache
        .get_or_compute(sma_key(1), || single_series(big.clone()))
        .unwrap();
    cache
        .get_or_compute(sma_key(2), || single_series(big.clone()))
        .unwrap();
    cache
        .get_or_compute(sma_key(1), || panic!("key 1 is cached, must not recompute"))
        .unwrap();
    cache
        .get_or_compute(sma_key(3), || single_series(big.clone()))
        .unwrap();

    let key1_again = cache
        .get_or_compute(sma_key(1), || {
            panic!("key 1 was used recently, must survive")
        })
        .unwrap();
    assert!(Arc::ptr_eq(&key1, &key1_again));
    let recomputed = std::cell::Cell::new(false);
    cache
        .get_or_compute(sma_key(2), || {
            recomputed.set(true);
            single_series(big.clone())
        })
        .unwrap();
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
    let first = cache
        .get_or_compute(sma_key(1), || single_series(big.clone()))
        .unwrap();
    let second = cache
        .get_or_compute(sma_key(2), || single_series(big.clone()))
        .unwrap();

    let first_again = cache
        .get_or_compute(sma_key(1), || panic!("must not be evicted"))
        .unwrap();
    let second_again = cache
        .get_or_compute(sma_key(2), || panic!("must not be evicted"))
        .unwrap();
    assert!(Arc::ptr_eq(&first, &first_again));
    assert!(Arc::ptr_eq(&second, &second_again));
    assert_eq!(cache.inner.lock().expect("test cache mutex").map.len(), 2);
}
