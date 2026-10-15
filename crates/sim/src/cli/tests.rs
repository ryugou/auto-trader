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
fn print_columns_and_kv_table_do_not_panic_on_empty_input() {
    let mut buf: Vec<u8> = Vec::new();
    print_kv_table(&mut buf, &[]).unwrap();
    print_columns(&mut buf, &["a", "b"], &[]).unwrap();
}

struct BrokenPipe;
impl std::io::Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn output_write_failure_is_returned_as_an_error_not_a_panic() {
    let err = print_kv_table(&mut BrokenPipe, &[("a".to_string(), "b".to_string())])
        .expect_err("a broken pipe must surface as Err");
    assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}

struct FlushFails;
impl std::io::Write for FlushFails {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::Other))
    }
}

#[test]
fn print_backfill_surfaces_a_flush_failure() {
    let report = fetch::BackfillReport {
        days: 1,
        saved: 1,
        one_sided: 0,
        invalid: 0,
        failed: Vec::new(),
    };
    for json in [false, true] {
        let err = print_backfill(&mut FlushFails, json, &report)
            .expect_err(&format!("flush failure must surface as Err (json={json})"));
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}

#[test]
fn print_benchmark_surfaces_a_flush_failure() {
    let benchmarks = vec![sample_benchmark(20, 1, 100, 90)];
    for json in [false, true] {
        let err = print_benchmark(&mut FlushFails, &benchmarks, json)
            .expect_err(&format!("flush failure must surface as Err (json={json})"));
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}

#[test]
fn print_run_surfaces_a_flush_failure() {
    let record = RunRecord {
        params: crate::script::ParamSet::new(),
        status: RunStatus::Ok,
        metrics: Some(sample_metrics()),
    };
    for json in [false, true] {
        let err = print_run(
            &mut FlushFails,
            &record,
            1.0,
            Uuid::nil(),
            Uuid::nil(),
            json,
        )
        .expect_err(&format!("flush failure must surface as Err (json={json})"));
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}

#[test]
fn print_sweep_surfaces_a_flush_failure() {
    let top: Vec<store::RunSummary> = Vec::new();
    for json in [false, true] {
        let err = print_sweep(
            &mut FlushFails,
            &top,
            1.0,
            Uuid::nil(),
            Uuid::nil(),
            0,
            json,
        )
        .expect_err(&format!("flush failure must surface as Err (json={json})"));
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }
}

fn dummy_leg() -> crate::benchmark::Leg {
    crate::benchmark::Leg {
        a: 0,
        b: 1,
        direction: 1,
        ideal_milli: 0,
        realizable_milli: 0,
    }
}

fn sample_benchmark(
    theta_pips: i64,
    leg_count: usize,
    ideal_milli: i64,
    realizable_milli: i64,
) -> Benchmark {
    Benchmark {
        theta_pips,
        legs: (0..leg_count).map(|_| dummy_leg()).collect(),
        ideal_milli,
        realizable_milli,
        labels: Vec::new(),
    }
}

#[test]
fn benchmark_json_includes_leg_count_ideal_and_realizable_pips_for_each_theta() {
    let cases = [(20i64, 3usize, 900i64, 850i64), (50, 1, 200, 150)];
    let benchmarks: Vec<Benchmark> = cases
        .iter()
        .map(|&(theta, legs, ideal, realizable)| sample_benchmark(theta, legs, ideal, realizable))
        .collect();

    let value = benchmark_json(&benchmarks);
    let by_theta = value
        .get("by_theta")
        .expect("benchmark_json must have a top-level by_theta key");
    for (theta, leg_count, ideal_milli, realizable_milli) in cases {
        let entry = by_theta
            .get(theta.to_string())
            .unwrap_or_else(|| panic!("by_theta must have an entry for theta {theta}"));
        assert_eq!(entry["leg_count"], serde_json::json!(leg_count));
        assert_eq!(
            entry["ideal_pips"],
            serde_json::json!(milli_to_pips(ideal_milli))
        );
        assert_eq!(
            entry["realizable_pips"],
            serde_json::json!(milli_to_pips(realizable_milli))
        );
    }
}

fn sample_metrics() -> crate::eval::Metrics {
    crate::eval::Metrics {
        total_pips: 12.5,
        trade_count: 3,
        win_rate: 0.6667,
        max_drawdown_pips: 4.0,
        time_in_market: 0.5,
        protective_stop_count: 1,
        segments: [0.0; 6],
        by_theta: std::collections::BTreeMap::new(),
    }
}

#[test]
fn run_json_ok_status_includes_metrics_and_omits_error() {
    let record = RunRecord {
        params: crate::script::ParamSet::new(),
        status: RunStatus::Ok,
        metrics: Some(sample_metrics()),
    };
    let value = run_json(&record, 1.23, Uuid::nil(), Uuid::nil());
    assert_eq!(value["status"], "ok");
    assert_eq!(value["elapsed_secs"], 1.23);
    assert!(
        value.get("error").is_none(),
        "ok status must not have an error key"
    );
    assert_eq!(value["metrics"]["total_pips"], 12.5);
    assert_eq!(value["metrics"]["trade_count"], 3);
}

#[test]
fn run_json_script_error_status_includes_error_and_omits_metrics() {
    let record = RunRecord {
        params: crate::script::ParamSet::new(),
        status: RunStatus::ScriptError {
            open_time: 0,
            message: "boom".to_string(),
        },
        metrics: None,
    };
    let value = run_json(&record, 0.5, Uuid::nil(), Uuid::nil());
    assert_eq!(value["status"], "script_error");
    assert!(
        value["error"].as_str().unwrap().contains("boom"),
        "error message must be present: {value}"
    );
    assert!(
        value.get("metrics").is_none(),
        "script_error status must not have a metrics key"
    );
}

#[test]
fn sweep_json_includes_elapsed_total_runs_and_top_run_fields() {
    let top = vec![store::RunSummary {
        params: serde_json::json!({"entry": 20}),
        total_pips: 42.0,
        trade_count: 5,
        metrics: serde_json::json!({"segments": vec![0.0; 6]}),
    }];
    let value = sweep_json(&top, 3.21, Uuid::nil(), Uuid::nil(), 7);
    assert_eq!(value["elapsed_secs"], 3.21);
    assert_eq!(value["total_runs"], 7);
    let top_runs = value["top_runs"]
        .as_array()
        .expect("top_runs must be an array");
    assert_eq!(top_runs.len(), 1);
    assert_eq!(top_runs[0]["total_pips"], 42.0);
    assert_eq!(top_runs[0]["trade_count"], 5);
    assert_eq!(top_runs[0]["params"]["entry"], 20);
}

#[test]
fn backfill_json_always_includes_every_count_and_the_failed_list() {
    let full = backfill_json(&fetch::BackfillReport {
        days: 2,
        saved: 100,
        one_sided: 1,
        invalid: 0,
        failed: Vec::new(),
    });
    assert_eq!(full["days"], 2);
    assert_eq!(full["saved"], 100);
    assert_eq!(full["one_sided"], 1);
    assert_eq!(full["invalid"], 0);
    assert_eq!(full["failed"], serde_json::json!([] as [String; 0]));

    let partial = backfill_json(&fetch::BackfillReport {
        days: 3,
        saved: 10,
        one_sided: 0,
        invalid: 2,
        failed: vec!["20240101 ASK".to_string()],
    });
    assert_eq!(partial["days"], 3);
    assert_eq!(partial["saved"], 10);
    assert_eq!(partial["invalid"], 2);
    assert_eq!(partial["failed"], serde_json::json!(["20240101 ASK"]));
}

#[derive(Clone, Default)]
struct CapturingWriter {
    buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl CapturingWriter {
    fn contents(&self) -> String {
        String::from_utf8(self.buf.lock().expect("lock poisoned").clone())
            .expect("log output must be UTF-8")
    }
}

impl std::io::Write for CapturingWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf
            .lock()
            .expect("lock poisoned")
            .extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn format_utc_rfc3339_logs_a_warning_when_falling_back_to_epoch_seconds() {
    let epoch_secs = i64::MIN;

    let writer = CapturingWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let formatted = {
        let _guard = tracing::subscriber::set_default(subscriber);
        format_utc_rfc3339(epoch_secs)
    };

    assert_eq!(
        formatted,
        epoch_secs.to_string(),
        "the fallback value itself must stay the epoch-seconds string"
    );
    let log = writer.contents();
    assert!(
        log.contains("WARN"),
        "falling back to the epoch-seconds representation must log a WARN: {log}"
    );
    assert!(
        log.contains(&epoch_secs.to_string()),
        "the WARN log must include the epoch seconds value that failed to convert: {log}"
    );
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
