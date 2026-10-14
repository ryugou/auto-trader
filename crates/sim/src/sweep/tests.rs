use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::types::{Bar, M5_SECS};

const RECORD_HELD_BY_MAIN_BEFORE_HANDOFF: usize = 1;

fn int_spec(name: &str, min: f64, max: f64, step: f64, default: f64) -> ParamSpec {
    ParamSpec {
        name: name.to_string(),
        kind: ParamKind::Int,
        min,
        max,
        step,
        default,
    }
}

fn float_spec(name: &str, min: f64, max: f64, step: f64, default: f64) -> ParamSpec {
    ParamSpec {
        name: name.to_string(),
        kind: ParamKind::Float,
        min,
        max,
        step,
        default,
    }
}

#[test]
fn candidate_count_for_an_integer_grid() {
    let spec = int_spec("entry", 10.0, 60.0, 2.0, 20.0);
    assert_eq!(candidate_count(&spec), 26);
}

#[test]
fn candidate_count_for_a_float_grid_and_the_last_candidate_equals_max() {
    let spec = float_spec("mult", 0.5, 2.0, 0.5, 2.0);
    assert_eq!(candidate_count(&spec), 4);

    let last = combination_at(std::slice::from_ref(&spec), 3);
    match last.get("mult") {
        Some(ParamValue::Float(v)) => assert!((v - 2.0).abs() < FLOAT_GRID_TOLERANCE),
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn candidate_count_for_a_float_grid_includes_max_despite_accumulated_error() {
    let spec = float_spec("x", 0.1, 0.3, 0.1, 0.1);
    assert_eq!(spec.max - spec.min, 0.19999999999999998);
    assert_eq!(candidate_count(&spec), 3);

    let last = combination_at(std::slice::from_ref(&spec), 2);
    match last.get("x") {
        Some(ParamValue::Float(v)) => assert!((v - 0.3).abs() < FLOAT_GRID_TOLERANCE),
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn float_candidate_count_does_not_include_a_candidate_beyond_max_plus_tolerance() {
    let spec = float_spec("x", 0.0, 299.99999999, 100.0, 0.0);
    assert_eq!(candidate_count(&spec), 3);
}

#[test]
fn float_candidate_count_includes_a_candidate_within_max_plus_tolerance() {
    let spec = float_spec("x", 0.0, 0.0029999995, 0.001, 0.0);
    assert_eq!(candidate_count(&spec), 4);
}

#[test]
fn zero_parameters_has_exactly_one_combination_which_is_empty() {
    assert_eq!(total_combinations(&[]).unwrap(), 1);
    assert_eq!(combination_at(&[], 0), ParamSet::new());
}

#[test]
fn two_parameters_use_mixed_radix_with_the_last_as_least_significant_digit() {
    let specs = vec![
        int_spec("a", 0.0, 2.0, 1.0, 0.0),
        int_spec("b", 0.0, 3.0, 1.0, 0.0),
    ];
    assert_eq!(total_combinations(&specs).unwrap(), 12);

    let at0 = combination_at(&specs, 0);
    assert_eq!(at0["a"], ParamValue::Int(0));
    assert_eq!(at0["b"], ParamValue::Int(0));

    let at1 = combination_at(&specs, 1);
    assert_eq!(at1["a"], ParamValue::Int(0), "index 1: only b advances");
    assert_eq!(at1["b"], ParamValue::Int(1));

    let at4 = combination_at(&specs, 4);
    assert_eq!(at4["a"], ParamValue::Int(1), "index 4: a advances by one");
    assert_eq!(at4["b"], ParamValue::Int(0));
}

#[test]
fn default_index_points_at_the_default_combination() {
    let specs = vec![
        int_spec("a", 0.0, 2.0, 1.0, 1.0),
        int_spec("b", 0.0, 3.0, 1.0, 2.0),
    ];
    let idx = default_index(&specs).unwrap();
    let combo = combination_at(&specs, idx);
    assert_eq!(combo["a"], ParamValue::Int(1));
    assert_eq!(combo["b"], ParamValue::Int(2));
}

#[test]
fn default_index_matches_compiled_script_default_params() {
    let host = ScriptHost::new(&SimConfig::default());
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/donchian_sar.rhai"
    ))
    .expect("donchian_sar.rhai must exist");
    let script = host
        .compile(&source)
        .expect("spec 8.1 script must register");

    let idx = default_index(&script.params).unwrap();
    assert_eq!(combination_at(&script.params, idx), script.default_params());
}

#[test]
fn total_combinations_rejects_a_script_whose_combination_count_exceeds_two_pow_63() {
    let specs: Vec<ParamSpec> = (0..12)
        .map(|i| int_spec(&format!("p{i}"), 0.0, 1000.0, 1.0, 0.0))
        .collect();

    match total_combinations(&specs) {
        Err(SimError::Args(message)) => {
            assert!(
                message.contains("narrow") && message.contains("coarser"),
                "message must guide the operator to narrow ranges or use a coarser step, got {message:?}"
            );
        }
        other => panic!("expected Err(SimError::Args(_)), got {other:?}"),
    }
    assert!(
        matches!(default_index(&specs), Err(SimError::Args(_))),
        "default_index must reject the same way as total_combinations"
    );
    assert!(
        matches!(select_indices(&specs, 10, 42), Err(SimError::Args(_))),
        "select_indices must reject the same way as total_combinations"
    );
}

#[test]
fn total_combinations_rejects_a_script_whose_combination_count_overflows_u128_without_panicking() {
    let specs: Vec<ParamSpec> = (0..12)
        .map(|i| float_spec(&format!("p{i}"), 0.0, 10000.0, 1.0, 0.0))
        .collect();

    assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
    assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
    assert!(matches!(
        select_indices(&specs, 10, 42),
        Err(SimError::Args(_))
    ));
}

#[test]
fn total_combinations_succeeds_when_the_combination_count_is_exactly_two_pow_63() {
    let specs: Vec<ParamSpec> = (0..9)
        .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
        .collect();

    assert_eq!(total_combinations(&specs).unwrap(), 1u128 << 63);
    assert_eq!(
        default_index(&specs).unwrap(),
        0,
        "every spec's default is k=0, so the mixed-radix index is 0"
    );
    let indices =
        select_indices(&specs, 5, 42).expect("exactly 2^63 combinations must not be rejected");
    assert_eq!(indices.len(), 5);
    assert_eq!(indices[0], 0, "default index must come first");
}

#[test]
fn total_combinations_rejects_when_one_more_parameter_pushes_the_count_to_two_pow_64() {
    let mut specs: Vec<ParamSpec> = (0..9)
        .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
        .collect();
    specs.push(int_spec("extra", 0.0, 1.0, 1.0, 0.0));

    assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
    assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
    assert!(matches!(
        select_indices(&specs, 5, 42),
        Err(SimError::Args(_))
    ));
}

#[test]
fn total_combinations_rejects_a_count_just_above_two_pow_63_but_below_two_pow_64() {
    let mut specs: Vec<ParamSpec> = (0..8)
        .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 0.0))
        .collect();
    specs.push(int_spec("p8", 0.0, 128.0, 1.0, 0.0));

    assert!(matches!(total_combinations(&specs), Err(SimError::Args(_))));
    assert!(matches!(default_index(&specs), Err(SimError::Args(_))));
    assert!(matches!(
        select_indices(&specs, 5, 42),
        Err(SimError::Args(_))
    ));
}

#[test]
fn default_index_at_the_two_pow_63_boundary_is_the_last_index() {
    let specs: Vec<ParamSpec> = (0..9)
        .map(|i| int_spec(&format!("p{i}"), 0.0, 127.0, 1.0, 127.0))
        .collect();
    let last = (1u128 << 63) - 1;

    assert_eq!(default_index(&specs).unwrap(), last);
    let combo = combination_at(&specs, last);
    for spec in &specs {
        assert_eq!(combo[spec.name.as_str()], ParamValue::Int(127));
    }

    let indices =
        select_indices(&specs, 5, 42).expect("exactly 2^63 combinations must not be rejected");
    assert_eq!(indices.len(), 5);
    assert_eq!(indices[0], last, "default index must come first");
    assert!(indices.iter().all(|&i| i < (1u128 << 63)));
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 5, "no duplicates");
}

fn specs_with_total(n: u64) -> Vec<ParamSpec> {
    vec![int_spec("x", 0.0, (n - 1) as f64, 1.0, 0.0)]
}

#[test]
fn selects_every_index_once_with_default_first_when_total_is_at_most_max_runs() {
    let specs = specs_with_total(5);
    let indices = select_indices(&specs, 10, 42).unwrap();

    assert_eq!(indices.len(), 5, "exactly one entry per combination");
    assert_eq!(indices[0], default_index(&specs).unwrap());
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4], "all 5 indices, no duplicates");
}

#[test]
fn selects_every_index_once_with_a_non_zero_default_first_when_total_is_at_most_max_runs() {
    let specs = vec![int_spec("x", 0.0, 4.0, 1.0, 2.0)];
    let indices = select_indices(&specs, 10, 42).unwrap();

    assert_eq!(indices.len(), 5, "exactly one entry per combination");
    assert_eq!(indices[0], 2, "default (index 2) must come first");
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted, vec![0, 1, 2, 3, 4], "all 5 indices, no duplicates");
}

#[test]
fn sampling_with_a_middle_default_never_duplicates_or_leaves_the_index_range() {
    let specs = vec![int_spec("x", 0.0, 26.0, 1.0, 13.0)];
    for seed in 0..20u64 {
        let indices = select_indices(&specs, 26, seed).unwrap();

        assert_eq!(indices.len(), 26, "seed {seed}");
        assert_eq!(indices[0], 13, "seed {seed}: default first");
        assert!(indices.iter().all(|&i| i < 27), "seed {seed}: in range");
        let mut sorted = indices.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 26, "seed {seed}: no duplicates");
    }
}

#[test]
fn selects_max_runs_indices_with_default_first_when_total_is_one_more_than_max_runs() {
    let specs = specs_with_total(27);
    let indices = select_indices(&specs, 26, 42).unwrap();

    assert_eq!(indices.len(), 26);
    assert_eq!(indices[0], default_index(&specs).unwrap());
    let mut sorted = indices.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        26,
        "no duplicates among the 26 returned indices"
    );
}

#[test]
fn same_seed_reproduces_the_same_selection_and_a_different_seed_differs() {
    let specs = specs_with_total(1000);
    let a1 = select_indices(&specs, 20, 1).unwrap();
    let a2 = select_indices(&specs, 20, 1).unwrap();
    assert_eq!(a1, a2, "same seed must reproduce the same selection");

    let b = select_indices(&specs, 20, 2).unwrap();
    assert_ne!(a1, b, "a different seed must change the selection");
}

#[test]
fn max_runs_of_one_returns_only_the_default_index() {
    let specs = specs_with_total(1000);
    let indices = select_indices(&specs, 1, 42).unwrap();
    assert_eq!(indices, vec![default_index(&specs).unwrap()]);
}

#[test]
fn zero_parameters_returns_a_single_combination() {
    let indices = select_indices(&[], 26, 42).unwrap();
    assert_eq!(indices, vec![0]);
}

#[test]
fn select_indices_with_max_runs_zero_returns_no_indices_without_panicking() {
    let specs = specs_with_total(5);
    assert_eq!(select_indices(&specs, 0, 42).unwrap(), Vec::<u128>::new());
}

fn deterministic_dataset(n: usize) -> Arc<Dataset> {
    const SPREAD_MILLI: i64 = 10;
    let mid_closes: Vec<f64> = (0..n)
        .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
        .collect();
    let close_milli: Vec<i64> = mid_closes
        .iter()
        .map(|&v| (v * 1000.0).round() as i64)
        .collect();
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let close = close_milli[i];
            let open = if i == 0 {
                close_milli[0]
            } else {
                close_milli[i - 1]
            };
            let half = SPREAD_MILLI / 2;
            let hi = open.max(close);
            let lo = open.min(close);
            Bar {
                open_time: i as i64 * M5_SECS,
                bid_open: open - half,
                bid_high: hi - half,
                bid_low: lo - half,
                bid_close: close - half,
                ask_open: open + half,
                ask_high: hi + half,
                ask_low: lo + half,
                ask_close: close + half,
            }
        })
        .collect();
    let from = bars[0].open_time;
    let to = bars[n - 1].open_time + M5_SECS;
    Arc::new(Dataset::new(bars, from, to, 0, 64).expect("deterministic dataset must build"))
}

fn donchian_sar_script(host: &ScriptHost) -> CompiledScript {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/donchian_sar.rhai"
    ))
    .expect("donchian_sar.rhai must exist");
    host.compile(&source)
        .expect("spec 8.1 script must register")
}

#[test]
fn jobs_1_and_jobs_4_produce_the_same_set_of_run_records() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = donchian_sar_script(&host);
    let dataset = deterministic_dataset(400);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let indices = select_indices(&script.params, 10, 42).unwrap();
    assert_eq!(indices.len(), 10);

    let run_with = |jobs: usize| -> Vec<RunRecord> {
        let mut records = Vec::new();
        run_sweep(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            jobs,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        )
        .expect("run_sweep must succeed");
        records.sort_by_key(|r| format!("{:?}", r.params));
        records
    };

    assert_eq!(run_with(1), run_with(4));
}

#[test]
fn on_result_error_stops_further_on_result_calls_and_is_returned() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = donchian_sar_script(&host);
    let dataset = deterministic_dataset(400);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let indices = select_indices(&script.params, 8, 42).unwrap();
    assert_eq!(indices.len(), 8);

    let mut calls = 0usize;
    let result = run_sweep(
        &dataset,
        &host,
        &script,
        &benchmarks,
        &cfg,
        &indices,
        1,
        &mut |_record| {
            calls += 1;
            if calls == 3 {
                Err(SimError::Args("stop after 3".to_string()))
            } else {
                Ok(())
            }
        },
    );

    match result {
        Err(SimError::Args(message)) => assert_eq!(message, "stop after 3"),
        other => panic!("expected Err(SimError::Args(\"stop after 3\")), got {other:?}"),
    }
    assert_eq!(
        calls, 3,
        "on_result must not be called again after it returns Err"
    );
    assert!(calls < indices.len(), "not every index should be observed");
}

#[test]
fn a_script_without_parameters_runs_exactly_once_with_an_empty_param_set() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = host
        .compile("fn params() { #{} }\nfn on_bar(ctx, p) { 0 }\n")
        .expect("valid script");
    let dataset = deterministic_dataset(10);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let indices = select_indices(&script.params, 26, 42).unwrap();

    let mut records = Vec::new();
    run_sweep(
        &dataset,
        &host,
        &script,
        &benchmarks,
        &cfg,
        &indices,
        1,
        &mut |record| {
            records.push(record);
            Ok(())
        },
    )
    .expect("run_sweep must succeed");

    assert_eq!(records.len(), 1, "on_result must be called exactly once");
    assert_eq!(records[0].params, ParamSet::new());
}

#[test]
fn a_panicking_worker_is_reported_as_batch_failed_with_index_and_message() {
    for jobs in [1usize, 4] {
        let (done_tx, done_rx) = mpsc::channel::<Result<(), SimError>>();
        std::thread::spawn(move || {
            let cfg = SimConfig::default();
            let host = ScriptHost::new(&cfg);
            let mut script = host
                .compile("fn params() { #{} }\nfn on_bar(ctx, p) { 0 }\n")
                .expect("valid script");
            script.params.push(int_spec("bad", 0.0, 10.0, 0.0, 0.0));
            let dataset = deterministic_dataset(10);
            let benchmarks: Vec<Benchmark> = Vec::new();
            let indices = vec![7u128];

            let result = run_sweep(
                &dataset,
                &host,
                &script,
                &benchmarks,
                &cfg,
                &indices,
                jobs,
                &mut |_record| Ok(()),
            );
            let _ = done_tx.send(result);
        });

        match done_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Err(SimError::BatchFailed(message))) => {
                assert!(
                    message.contains("combination index 7"),
                    "jobs={jobs}: missing index in {message:?}"
                );
                assert!(
                    message.contains("divide by zero"),
                    "jobs={jobs}: missing panic message in {message:?}"
                );
            }
            Ok(other) => panic!("jobs={jobs}: expected Err(BatchFailed), got {other:?}"),
            Err(_) => panic!(
                "jobs={jobs}: run_sweep did not return within 10s; it likely deadlocked \
                 on the bounded channel after a worker panicked"
            ),
        }
    }
}

#[test]
fn completed_but_unhandled_results_stay_bounded_when_on_result_is_slow() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = donchian_sar_script(&host);
    let dataset = deterministic_dataset(400);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let jobs = 2usize;
    let indices = select_indices(&script.params, 20, 42).unwrap();
    assert_eq!(indices.len(), 20);

    let completed = AtomicUsize::new(0);
    let handed = AtomicUsize::new(0);
    let max_gap = AtomicUsize::new(0);
    let monitor_done = AtomicBool::new(false);
    let on_worker_complete = || {
        completed.fetch_add(1, Ordering::SeqCst);
    };

    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !monitor_done.load(Ordering::SeqCst) {
                let gap = completed
                    .load(Ordering::SeqCst)
                    .saturating_sub(handed.load(Ordering::SeqCst));
                max_gap.fetch_max(gap, Ordering::SeqCst);
                std::thread::sleep(Duration::from_micros(200));
            }
        });

        let result = run_sweep_with_hook(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            jobs,
            &mut |_record| {
                handed.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(30));
                Ok(())
            },
            &on_worker_complete,
        );
        monitor_done.store(true, Ordering::SeqCst);
        result
    });

    result.expect("run_sweep_with_hook must succeed");
    assert_eq!(completed.load(Ordering::SeqCst), 20);
    assert_eq!(handed.load(Ordering::SeqCst), 20);

    let capacity = channel_capacity(jobs);
    let observed_max_gap = max_gap.load(Ordering::SeqCst);
    let bound = capacity + jobs + RECORD_HELD_BY_MAIN_BEFORE_HANDOFF;
    assert!(
        observed_max_gap <= bound,
        "backlog of completed-but-unhandled results grew to {observed_max_gap}, which \
         exceeds capacity({capacity}) + jobs({jobs}) + 1; unprocessed RunRecords are \
         accumulating in memory faster than the slow consumer can drain them"
    );
}

#[test]
fn run_sweep_returns_the_on_result_error_without_hanging_when_the_consumer_is_slow() {
    let (done_tx, done_rx) = mpsc::channel::<(Result<(), SimError>, usize)>();
    std::thread::spawn(move || {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = donchian_sar_script(&host);
        let dataset = deterministic_dataset(400);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = select_indices(&script.params, 20, 42).unwrap();
        assert_eq!(indices.len(), 20);

        let mut calls = 0usize;
        let result = run_sweep(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            2,
            &mut |_record| {
                calls += 1;
                std::thread::sleep(Duration::from_millis(20));
                if calls == 3 {
                    Err(SimError::Args("stop after 3".to_string()))
                } else {
                    Ok(())
                }
            },
        );
        let _ = done_tx.send((result, calls));
    });

    match done_rx.recv_timeout(Duration::from_secs(10)) {
        Ok((Err(SimError::Args(message)), calls)) => {
            assert_eq!(message, "stop after 3");
            assert_eq!(
                calls, 3,
                "on_result must not be called at all once it has returned Err, \
                 got {calls} calls"
            );
        }
        Ok((other, calls)) => panic!(
            "expected Err(SimError::Args(\"stop after 3\")), got {other:?} after {calls} calls"
        ),
        Err(_) => panic!(
            "run_sweep did not return within 10s; it likely deadlocked on the bounded \
             channel after on_result returned Err"
        ),
    }
}

#[test]
fn a_panicking_on_result_propagates_without_hanging() {
    let jobs = 2usize;
    let capacity = channel_capacity(jobs);
    let threshold = RECORD_HELD_BY_MAIN_BEFORE_HANDOFF + capacity + jobs;

    let (done_tx, done_rx) = mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let cfg = SimConfig::default();
        let host = ScriptHost::new(&cfg);
        let script = donchian_sar_script(&host);
        let dataset = deterministic_dataset(400);
        let benchmarks: Vec<Benchmark> = Vec::new();
        let indices = select_indices(&script.params, 20, 42).unwrap();
        assert_eq!(indices.len(), 20);
        assert!(
            indices.len() > threshold,
            "need more jobs ({}) than threshold ({threshold}) to reach saturation",
            indices.len()
        );

        let completed = AtomicUsize::new(0);
        let on_worker_complete = || {
            completed.fetch_add(1, Ordering::SeqCst);
        };

        let _ = run_sweep_with_hook(
            &dataset,
            &host,
            &script,
            &benchmarks,
            &cfg,
            &indices,
            jobs,
            &mut |_record| {
                let deadline = Duration::from_secs(5);
                let start = std::time::Instant::now();
                while completed.load(Ordering::SeqCst) < threshold {
                    let elapsed = start.elapsed();
                    if elapsed >= deadline {
                        let got = completed.load(Ordering::SeqCst);
                        panic!(
                            "timed out after {elapsed:?} waiting for {threshold} \
                             completions (1 held + capacity={capacity} + jobs={jobs}) \
                             to fill the channel and block all workers on send; only \
                             {got} completed, so the full-buffer/all-workers-blocked \
                             state was never reached"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                panic!("on_result boom");
            },
            &on_worker_complete,
        );
        let _ = done_tx.send(());
    });

    match done_rx.recv_timeout(Duration::from_secs(10)) {
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "run_sweep did not return within 10s; it deadlocked after on_result panicked \
             (workers blocked on a full channel whose receiver was never dropped)"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let payload = handle
                .join()
                .expect_err("the sweep thread must have panicked");
            let message = panic_message(payload.as_ref());
            assert!(
                message.contains("on_result boom"),
                "unexpected panic payload: {message:?}"
            );
        }
        Ok(()) => panic!("the on_result panic was not propagated out of run_sweep"),
    }
}

#[test]
fn a_sweep_with_one_runtime_erroring_parameter_still_returns_ok_overall() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let source = "fn params() { #{ mode: #{ min: 0, max: 1, step: 1, \"default\": 0 } } }\n\
                   fn on_bar(ctx, p) {\n\
                   if p.mode == 1 { let x = ctx.sma(\"M5\", 2000, 0); }\n\
                   0\n\
                   }\n";
    let script = host.compile(source).expect("valid script");
    let dataset = deterministic_dataset(10);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let indices = select_indices(&script.params, 2, 42).unwrap();
    assert_eq!(indices.len(), 2, "mode has exactly 2 candidates (0 and 1)");

    let mut records = Vec::new();
    let result = run_sweep(
        &dataset,
        &host,
        &script,
        &benchmarks,
        &cfg,
        &indices,
        1,
        &mut |record| {
            records.push(record);
            Ok(())
        },
    );

    assert!(
        result.is_ok(),
        "run_sweep itself must return Ok even if one run is a script_error, got {result:?}"
    );
    assert_eq!(records.len(), 2);
    let ok_count = records
        .iter()
        .filter(|r| matches!(r.status, RunStatus::Ok))
        .count();
    let error_count = records
        .iter()
        .filter(|r| matches!(r.status, RunStatus::ScriptError { .. }))
        .count();
    assert_eq!(ok_count, 1, "mode=0 must simulate without error");
    assert_eq!(error_count, 1, "mode=1 must fail with a script_error");
    for record in &records {
        match record.status {
            RunStatus::Ok => assert!(record.metrics.is_some(), "an Ok run must carry metrics"),
            RunStatus::ScriptError { .. } => {
                assert!(
                    record.metrics.is_none(),
                    "a script_error run must not carry metrics"
                )
            }
        }
    }
}

#[test]
fn claim_next_unless_cancelled_hands_out_no_index_after_cancellation() {
    let mut open = WorkQueue {
        next: 0,
        cancelled: false,
    };
    assert_eq!(open.claim_next_unless_cancelled(2), Some(0));
    assert_eq!(open.claim_next_unless_cancelled(2), Some(1));
    assert_eq!(open.claim_next_unless_cancelled(2), None);

    let mut cancelled = WorkQueue {
        next: 0,
        cancelled: true,
    };
    assert_eq!(cancelled.claim_next_unless_cancelled(5), None);
}

#[test]
fn on_result_error_stops_workers_from_starting_unclaimed_runs() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = donchian_sar_script(&host);
    let dataset = deterministic_dataset(400);
    let benchmarks: Vec<Benchmark> = Vec::new();
    let jobs = 1usize;
    let indices = select_indices(&script.params, 20, 42).unwrap();
    assert_eq!(indices.len(), 20);

    let completed = AtomicUsize::new(0);
    let on_worker_complete = || {
        completed.fetch_add(1, Ordering::SeqCst);
    };

    let result = run_sweep_with_hook(
        &dataset,
        &host,
        &script,
        &benchmarks,
        &cfg,
        &indices,
        jobs,
        &mut |_record| Err(SimError::Config("stop after first result".to_string())),
        &on_worker_complete,
    );

    match result {
        Err(SimError::Config(message)) => assert_eq!(message, "stop after first result"),
        other => panic!("expected the on_result error to be returned, got {other:?}"),
    }
    let started = completed.load(Ordering::SeqCst);
    let bound = RECORD_HELD_BY_MAIN_BEFORE_HANDOFF + channel_capacity(jobs) + jobs;
    assert!(
        started <= bound && started < indices.len(),
        "{started} runs completed after on_result failed on the first record; at most {bound} \
         may already be in flight and none of the {} indices may be claimed afterwards",
        indices.len()
    );
}

#[test]
fn a_poisoned_indicator_cache_makes_run_sweep_return_err_and_skip_remaining_combinations() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let source = "fn params() { #{ mode: #{ min: 0, max: 4, step: 1, \"default\": 0 } } }\n\
                   fn on_bar(ctx, p) {\n\
                   let x = ctx.sma(\"M5\", 1, 0);\n\
                   0\n\
                   }\n";
    let script = host.compile(source).expect("valid script");
    let dataset = deterministic_dataset(400);
    dataset.poison_indicator_cache_for_test();
    let benchmarks: Vec<Benchmark> = Vec::new();
    let indices = select_indices(&script.params, 5, 42).unwrap();
    assert_eq!(indices.len(), 5);

    let mut records = Vec::new();
    let result = run_sweep(
        &dataset,
        &host,
        &script,
        &benchmarks,
        &cfg,
        &indices,
        1,
        &mut |record| {
            records.push(record);
            Ok(())
        },
    );

    assert!(
        result.is_err(),
        "a poisoned indicator cache is an internal fault, not a script error; run_sweep must \
         return Err, got {result:?}"
    );
    assert!(
        records.len() < indices.len(),
        "the fatal error must stop the sweep before every combination is attempted, got \
         {} of {} records",
        records.len(),
        indices.len()
    );
}
