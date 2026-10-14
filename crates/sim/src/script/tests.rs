use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::*;
use crate::config::SimConfig;
use crate::error::SimError;
use crate::indicators::{IndicatorKey, IndicatorKind, IndicatorSeries};
use crate::series::{Dataset, Tf};
use crate::types::{Bar, M5_SECS, milli_to_pips};

fn wrap_on_bar(body: &str) -> String {
    format!("fn params() {{ #{{}} }}\nfn on_bar(ctx, p) {{\n{body}\n}}\n")
}

fn empty_params() -> ParamSet {
    ParamSet::new()
}

fn state(t: usize) -> BarState {
    BarState {
        t,
        position: 0,
        entry_price_milli: 0,
        bars_held: 0,
    }
}

fn run_single(
    dataset: &Arc<Dataset>,
    cfg: &SimConfig,
    source: &str,
    params: &ParamSet,
    bar_state: BarState,
) -> Result<i8, String> {
    let host = ScriptHost::new(cfg);
    let script = host.compile(source).expect("test script must compile");
    let mut run = ScriptRun::new(&host, &script, params, dataset.clone(), cfg);
    run.on_bar(bar_state).map_err(|e| format!("{e:?}"))
}

fn deterministic_bars(n: usize) -> Vec<Bar> {
    const SPREAD_MILLI: i64 = 10;
    let mid_closes: Vec<f64> = (0..n)
        .map(|i| 150.0 + 0.3 * (i as f64 * 0.07).sin() + 0.001 * i as f64)
        .collect();
    let close_milli: Vec<i64> = mid_closes
        .iter()
        .map(|&v| (v * 1000.0).round() as i64)
        .collect();
    (0..n)
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
        .collect()
}

fn deterministic_dataset(n: usize) -> Arc<Dataset> {
    let bars = deterministic_bars(n);
    let from = bars[0].open_time;
    let to = bars[n - 1].open_time + M5_SECS;
    Arc::new(Dataset::new(bars, from, to, 0, 64).expect("deterministic dataset must build"))
}

fn script_shape_violation_cases() -> Vec<(&'static str, String)> {
    let oversized_padding = "x".repeat(40_000);
    vec![
        (
            "syntax error",
            "fn params() { #{} }\nfn on_bar(ctx, p) { ctx.position".to_string(),
        ),
        (
            "source exceeds 32 KiB",
            format!("// {oversized_padding}\nfn params() {{ #{{}} }}\nfn on_bar(ctx, p) {{ 0 }}\n"),
        ),
        ("params() undefined", "fn on_bar(ctx, p) { 0 }".to_string()),
        ("on_bar undefined", "fn params() { #{} }".to_string()),
        (
            "params() has 1 argument",
            "fn params(x) { #{} }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "on_bar has 1 argument",
            "fn params() { #{} }\nfn on_bar(ctx) { 0 }".to_string(),
        ),
        (
            "params() returns a non-map",
            "fn params() { 42 }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
    ]
}

fn param_spec_violation_cases() -> Vec<(&'static str, String)> {
    let thirteen_params = (0..13)
        .map(|i| format!("p{i}: #{{ min: 0, max: 10, step: 1, \"default\": 0 }}"))
        .collect::<Vec<_>>()
        .join(", ");

    vec![
        (
            "a param spec is not a map",
            "fn params() { #{ entry: 5 } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "a param spec is missing 'min'",
            "fn params() { #{ entry: #{ max: 60, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "a param spec mixes int and float",
            "fn params() { #{ entry: #{ min: 10, max: 60.0, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "min > default",
            "fn params() { #{ entry: #{ min: 30, max: 60, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "default > max",
            "fn params() { #{ entry: #{ min: 10, max: 15, step: 2, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "step == 0",
            "fn params() { #{ entry: #{ min: 10, max: 60, step: 0, \"default\": 20 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "13 parameters",
            format!("fn params() {{ #{{ {thirteen_params} }} }}\nfn on_bar(ctx, p) {{ 0 }}\n"),
        ),
        (
            "default not on the step grid",
            "fn params() { #{ entry: #{ min: 10, max: 60, step: 2, \"default\": 15 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "float step is NaN",
            "fn params() { #{ x: #{ min: 0.0, max: 1.0, step: 0.0/0.0, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "float max is infinite",
            "fn params() { #{ x: #{ min: 0.0, max: 1.0/0.0, step: 0.5, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "float min is -infinite",
            "fn params() { #{ x: #{ min: -1.0/0.0, max: 1.0, step: 0.5, \"default\": 0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "float default is NaN",
            "fn params() { #{ x: #{ min: 0.0, max: 1.0, step: 0.5, \"default\": 0.0/0.0 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "int default off the step grid even with the full i64 range",
            "fn params() { #{ x: #{ min: -9223372036854775807 - 1, max: 9223372036854775807, step: 2, \"default\": 9223372036854775807 } } }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
        (
            "params() infinite loop",
            "fn params() { loop {} }\nfn on_bar(ctx, p) { 0 }".to_string(),
        ),
    ]
}

fn assert_cases_rejected_as_invalid_script(host: &ScriptHost, cases: Vec<(&str, String)>) {
    for (name, source) in cases {
        match host.compile(&source) {
            Err(SimError::InvalidScript(_)) => {}
            Err(other) => {
                panic!("case '{name}' should be InvalidScript, got a different SimError: {other}")
            }
            Ok(_) => panic!(
                "case '{name}' should be InvalidScript, but the script compiled successfully"
            ),
        }
    }
}

#[test]
fn registration_rejects_each_8_1_violation() {
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);

    assert_cases_rejected_as_invalid_script(&host, script_shape_violation_cases());
    assert_cases_rejected_as_invalid_script(&host, param_spec_violation_cases());
}

#[test]
fn full_i64_range_int_param_does_not_overflow_when_checking_the_step_grid() {
    let host = ScriptHost::new(&SimConfig::default());
    let source = "fn params() { #{ a: #{ min: -9223372036854775807 - 1, max: 9223372036854775807, step: 1, \"default\": 9223372036854775807 } } }\nfn on_bar(ctx, p) { 0 }";
    assert!(host.compile(source).is_ok());
}

#[test]
fn donchian_sar_script_registers_with_a_single_entry_param() {
    let host = ScriptHost::new(&SimConfig::default());
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/scripts/donchian_sar.rhai"
    ))
    .expect("donchian_sar.rhai must exist");

    let script = host
        .compile(&source)
        .expect("spec 8.1 script must register");

    assert_eq!(
        script.params,
        vec![ParamSpec {
            name: "entry".to_string(),
            kind: ParamKind::Int,
            min: 10.0,
            max: 60.0,
            step: 2.0,
            default: 20.0,
        }]
    );
}

#[test]
fn sha256_matches_a_direct_sha2_computation() {
    let host = ScriptHost::new(&SimConfig::default());
    let source = wrap_on_bar("0");
    let script = host.compile(&source).expect("valid script");

    let expected = {
        let digest = Sha256::digest(source.as_bytes());
        hex::encode(digest)
    };
    assert_eq!(script.sha256, expected);
}

#[test]
fn a_script_with_no_parameters_registers_with_an_empty_params_list() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = host
        .compile(&wrap_on_bar("0"))
        .expect("fn params() { #{} } must register");
    assert!(script.params.is_empty());
}

#[test]
fn a_top_level_let_statement_registers_but_is_never_evaluated() {
    let host = ScriptHost::new(&SimConfig::default());
    let source = "let x = 1;\nfn params() { #{} }\nfn on_bar(ctx, p) { x }\n";
    let script = host
        .compile(source)
        .expect("a top-level statement must not block registration");

    let dataset = deterministic_dataset(10);
    let mut run = ScriptRun::new(
        &host,
        &script,
        &empty_params(),
        dataset,
        &SimConfig::default(),
    );
    let result = run.on_bar(state(5));
    assert!(
        result.is_err(),
        "on_bar referencing a top-level `let` must fail at runtime (the statement was never run), got {result:?}"
    );
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

fn float_mult_script(host: &ScriptHost) -> CompiledScript {
    host.compile(
        "fn params() { #{ mult: #{ min: 0.5, max: 5.0, step: 0.5, \"default\": 2.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
    )
    .expect("valid script")
}

#[test]
fn resolve_params_uses_default_when_nothing_is_given() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = donchian_sar_script(&host);
    let given = serde_json::Map::new();
    let resolved = script
        .resolve_params(&given)
        .expect("empty params must resolve");
    assert_eq!(resolved.get("entry"), Some(&ParamValue::Int(20)));
}

#[test]
fn resolve_params_fills_unspecified_params_with_default() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = host
        .compile(
            "fn params() { #{ a: #{ min: 0, max: 10, step: 1, \"default\": 3 }, b: #{ min: 0, max: 10, step: 1, \"default\": 7 } } }\nfn on_bar(ctx, p) { 0 }\n",
        )
        .expect("valid script");
    let mut given = serde_json::Map::new();
    given.insert("a".to_string(), serde_json::json!(5));
    let resolved = script
        .resolve_params(&given)
        .expect("partial params must resolve");
    assert_eq!(resolved.get("a"), Some(&ParamValue::Int(5)));
    assert_eq!(resolved.get("b"), Some(&ParamValue::Int(7)));
}

#[test]
fn resolve_params_rejects_unknown_name() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = donchian_sar_script(&host);
    let mut given = serde_json::Map::new();
    given.insert("bogus".to_string(), serde_json::json!(1));
    assert!(matches!(
        script.resolve_params(&given),
        Err(SimError::Args(_))
    ));
}

#[test]
fn resolve_params_rejects_out_of_range_value() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = donchian_sar_script(&host);
    let mut given = serde_json::Map::new();
    given.insert("entry".to_string(), serde_json::json!(1000));
    assert!(matches!(
        script.resolve_params(&given),
        Err(SimError::Args(_))
    ));
}

#[test]
fn resolve_params_rejects_value_off_the_step_grid() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = donchian_sar_script(&host);
    let mut given = serde_json::Map::new();
    let value_off_the_step_grid = 31;
    given.insert(
        "entry".to_string(),
        serde_json::json!(value_off_the_step_grid),
    );
    assert!(matches!(
        script.resolve_params(&given),
        Err(SimError::Args(_))
    ));
}

#[test]
fn resolve_params_rejects_a_float_given_to_an_int_param() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = donchian_sar_script(&host);
    let mut given = serde_json::Map::new();
    given.insert("entry".to_string(), serde_json::json!(30.0));
    assert!(matches!(
        script.resolve_params(&given),
        Err(SimError::Args(_))
    ));
}

#[test]
fn resolve_params_accepts_a_json_integer_for_a_float_param() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = float_mult_script(&host);
    let mut given = serde_json::Map::new();
    given.insert("mult".to_string(), serde_json::json!(3));
    let resolved = script
        .resolve_params(&given)
        .expect("int-as-float must be accepted");
    assert_eq!(resolved.get("mult"), Some(&ParamValue::Float(3.0)));
}

#[test]
fn resolve_params_rejects_each_invalid_float_value() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = float_mult_script(&host);

    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "a string given to a float param",
            serde_json::json!("not a number"),
        ),
        ("an out-of-range float value", serde_json::json!(100.0)),
    ];

    for (name, value) in cases {
        let mut given = serde_json::Map::new();
        given.insert("mult".to_string(), value);
        assert!(
            matches!(script.resolve_params(&given), Err(SimError::Args(_))),
            "case '{name}' should be SimError::Args"
        );
    }
}

#[test]
fn compile_rejects_float_default_whose_value_gap_to_grid_exceeds_tolerance() {
    let host = ScriptHost::new(&SimConfig::default());
    let result = host.compile(
        "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.00000001 } } }\nfn on_bar(ctx, p) { 0 }\n",
    );
    assert!(matches!(result, Err(SimError::InvalidScript(_))));
}

#[test]
fn compile_accepts_float_default_with_only_rounding_error() {
    let host = ScriptHost::new(&SimConfig::default());
    host.compile(
        "fn params() { #{ x: #{ min: 0.1, max: 1.0, step: 0.1, \"default\": 0.3 } } }\nfn on_bar(ctx, p) { 0 }\n",
    )
    .expect("0.3 differs from 0.1 + 2*0.1 only by float rounding");
}

#[test]
fn resolve_params_rejects_float_whose_value_gap_to_grid_exceeds_tolerance() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = host
        .compile(
            "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
        )
        .expect("valid script");
    let mut given = serde_json::Map::new();
    given.insert("x".to_string(), serde_json::json!(100.00000001));
    assert!(matches!(
        script.resolve_params(&given),
        Err(SimError::Args(_))
    ));
}

#[test]
fn resolve_params_accepts_float_on_the_grid() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = host
        .compile(
            "fn params() { #{ x: #{ min: 0.0, max: 1000.0, step: 100.0, \"default\": 100.0 } } }\nfn on_bar(ctx, p) { 0 }\n",
        )
        .expect("valid script");
    let mut given = serde_json::Map::new();
    given.insert("x".to_string(), serde_json::json!(300.0));
    let resolved = script.resolve_params(&given).expect("300.0 is on the grid");
    assert_eq!(resolved.get("x"), Some(&ParamValue::Float(300.0)));
}

const T_MID: usize = 300;

#[test]
fn ctx_scalar_properties_match_bar_state_and_bar_values() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();
    let bar = dataset.bars()[T_MID];

    let fifty_pips_milli = 500;
    let entry_price_milli = bar.bid_close - fifty_pips_milli;
    let bar_state = BarState {
        t: T_MID,
        position: 1,
        entry_price_milli,
        bars_held: 7,
    };
    let expected_entry_price = entry_price_milli as f64 / 1000.0;
    let mut long_params = ParamSet::new();
    long_params.insert(
        "entry_price_x1e6".to_string(),
        ParamValue::Int((expected_entry_price * 1_000_000.0).round() as i64),
    );
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "if ctx.position != 1 { return -1; }\n",
            "if ((ctx.entry_price * 1000000.0).round() - p.entry_price_x1e6).abs() > 1 { return -1; }\n",
            "if ctx.bars_held != 7 { return -1; }\n",
            "if (ctx.unrealized_pips - 50.0).abs() > 1e-6 { return -1; }\n",
            "1"
        )),
        &long_params,
        bar_state,
    );
    assert_eq!(
        result,
        Ok(1),
        "position/entry_price/bars_held/unrealized_pips (long)"
    );

    let thirty_pips_milli = 300;
    let short_state = BarState {
        t: T_MID,
        position: -1,
        entry_price_milli: bar.ask_close - thirty_pips_milli,
        bars_held: 2,
    };
    let short_result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("if (ctx.unrealized_pips - (-30.0)).abs() > 1e-6 { -1 } else { 1 }"),
        &empty_params(),
        short_state,
    );
    assert_eq!(short_result, Ok(1), "unrealized_pips (short)");

    let flat_result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(
            "if ctx.entry_price == 0.0 && ctx.unrealized_pips == 0.0 && ctx.bars_held == 0 { 1 } else { -1 }",
        ),
        &empty_params(),
        state(T_MID),
    );
    assert_eq!(flat_result, Ok(1), "flat position defaults");
}

#[test]
fn ctx_time_hour_weekday_and_spread_match_the_bar() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();
    let bar = dataset.bars()[T_MID];
    let end_time = bar.open_time + M5_SECS;
    let expected = ctx::end_time_to_utc(end_time).expect("end_time must be a valid UTC timestamp");
    let expected_hour = {
        use chrono::Timelike;
        expected.hour() as i64
    };
    let expected_weekday = {
        use chrono::Datelike;
        expected.weekday().num_days_from_monday() as i64
    };
    let expected_spread_pips = milli_to_pips(bar.ask_close - bar.bid_close);

    let mut params = ParamSet::new();
    params.insert("time".to_string(), ParamValue::Int(end_time));
    params.insert("hour".to_string(), ParamValue::Int(expected_hour));
    params.insert("weekday".to_string(), ParamValue::Int(expected_weekday));
    params.insert(
        "spread_x1000".to_string(),
        ParamValue::Int((expected_spread_pips * 1000.0).round() as i64),
    );

    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "if ctx.time != p.time { return -1; }\n",
            "if ctx.hour != p.hour { return -1; }\n",
            "if ctx.weekday != p.weekday { return -1; }\n",
            "if ((ctx.spread * 1000.0).round() - p.spread_x1000).abs() > 1 { return -1; }\n",
            "1"
        )),
        &params,
        state(T_MID),
    );
    assert_eq!(result, Ok(1));
}

#[test]
fn ctx_close_returns_the_latest_completed_m15_and_shift_1_returns_the_previous_one() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();
    let completed = dataset.completed(Tf::M15, T_MID);
    assert!(completed >= 2, "test needs at least 2 completed M15 bars");
    let series = dataset.series(Tf::M15);
    let latest = series.close[completed - 1];
    let previous = series.close[completed - 2];

    let mut params = ParamSet::new();
    params.insert(
        "latest_x1e6".to_string(),
        ParamValue::Int((latest * 1_000_000.0).round() as i64),
    );
    params.insert(
        "previous_x1e6".to_string(),
        ParamValue::Int((previous * 1_000_000.0).round() as i64),
    );

    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "let c0 = ctx.close(\"M15\", 0);\n",
            "let c1 = ctx.close(\"M15\", 1);\n",
            "if ((c0 * 1000000.0).round() - p.latest_x1e6).abs() > 1 { return -1; }\n",
            "if ((c1 * 1000000.0).round() - p.previous_x1e6).abs() > 1 { return -1; }\n",
            "1"
        )),
        &params,
        state(T_MID),
    );
    assert_eq!(result, Ok(1));
}

fn assert_ctx_single_indicator_matches_dataset(
    dataset: &Arc<Dataset>,
    kind: IndicatorKind,
    fn_name: &str,
) {
    let cfg = SimConfig::default();
    let tf = Tf::M15;
    let period = 5u32;
    let shift = 0usize;
    let completed = dataset.completed(tf, T_MID);
    let idx = completed - 1 - shift;
    let key = IndicatorKey {
        tf,
        kind,
        period,
        mult_x100: 0,
    };
    let series = dataset.indicator(key).unwrap();
    let IndicatorSeries::Single(values) = &*series else {
        panic!("expected Single series for {fn_name}")
    };
    let expected = values[idx];
    assert!(
        !expected.is_nan(),
        "{fn_name}: test setup must have enough bars"
    );

    let mut params = ParamSet::new();
    params.insert(
        "expected_x1e6".to_string(),
        ParamValue::Int((expected * 1_000_000.0).round() as i64),
    );
    let body = format!(
        "let v = ctx.{fn_name}(\"M15\", {period}, {shift});\nif ((v * 1000000.0).round() - p.expected_x1e6).abs() > 1 {{ -1 }} else {{ 1 }}"
    );
    let result = run_single(dataset, &cfg, &wrap_on_bar(&body), &params, state(T_MID));
    assert_eq!(result, Ok(1), "{fn_name} did not match Dataset::indicator");
}

#[test]
fn ctx_sma_matches_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Sma, "sma");
}

#[test]
fn ctx_ema_matches_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Ema, "ema");
}

#[test]
fn ctx_rsi_matches_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Rsi, "rsi");
}

#[test]
fn ctx_atr_matches_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Atr, "atr");
}

#[test]
fn ctx_adx_matches_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_single_indicator_matches_dataset(&dataset, IndicatorKind::Adx, "adx");
}

#[test]
fn ctx_donchian_has_upper_and_lower_keys_matching_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();
    let tf = Tf::M15;
    let period = 5u32;
    let completed = dataset.completed(tf, T_MID);
    let idx = completed - 1;
    let key = IndicatorKey {
        tf,
        kind: IndicatorKind::Donchian,
        period,
        mult_x100: 0,
    };
    let series = dataset.indicator(key).unwrap();
    let IndicatorSeries::Channel { lower, upper } = &*series else {
        panic!("expected Channel series for donchian")
    };
    let (expected_lower, expected_upper) = (lower[idx], upper[idx]);

    let mut params = ParamSet::new();
    params.insert(
        "lower_x1e6".to_string(),
        ParamValue::Int((expected_lower * 1_000_000.0).round() as i64),
    );
    params.insert(
        "upper_x1e6".to_string(),
        ParamValue::Int((expected_upper * 1_000_000.0).round() as i64),
    );
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "let ch = ctx.donchian(\"M15\", 5, 0);\n",
            "if ((ch.upper * 1000000.0).round() - p.upper_x1e6).abs() > 1 { return -1; }\n",
            "if ((ch.lower * 1000000.0).round() - p.lower_x1e6).abs() > 1 { return -1; }\n",
            "1"
        )),
        &params,
        state(T_MID),
    );
    assert_eq!(result, Ok(1));
}

fn assert_ctx_band_indicator_matches_dataset(
    dataset: &Arc<Dataset>,
    kind: IndicatorKind,
    fn_name: &str,
) {
    let cfg = SimConfig::default();
    let tf = Tf::M15;
    let period = 5u32;
    let completed = dataset.completed(tf, T_MID);
    let idx = completed - 1;
    let key = IndicatorKey {
        tf,
        kind,
        period,
        mult_x100: 200,
    };
    let series = dataset.indicator(key).unwrap();
    let IndicatorSeries::Band {
        lower,
        middle,
        upper,
    } = &*series
    else {
        panic!("expected Band series for {fn_name}")
    };
    let (expected_lower, expected_middle, expected_upper) = (lower[idx], middle[idx], upper[idx]);

    let mut params = ParamSet::new();
    params.insert(
        "lower_x1e6".to_string(),
        ParamValue::Int((expected_lower * 1_000_000.0).round() as i64),
    );
    params.insert(
        "middle_x1e6".to_string(),
        ParamValue::Int((expected_middle * 1_000_000.0).round() as i64),
    );
    params.insert(
        "upper_x1e6".to_string(),
        ParamValue::Int((expected_upper * 1_000_000.0).round() as i64),
    );
    let body = format!(
        concat!(
            "let b = ctx.{fn_name}(\"M15\", 5, 2, 0);\n",
            "if ((b.upper * 1000000.0).round() - p.upper_x1e6).abs() > 1 {{ return -1; }}\n",
            "if ((b.middle * 1000000.0).round() - p.middle_x1e6).abs() > 1 {{ return -1; }}\n",
            "if ((b.lower * 1000000.0).round() - p.lower_x1e6).abs() > 1 {{ return -1; }}\n",
            "1"
        ),
        fn_name = fn_name
    );
    let result = run_single(dataset, &cfg, &wrap_on_bar(&body), &params, state(T_MID));
    assert_eq!(result, Ok(1), "{fn_name} did not match Dataset::indicator");
}

#[test]
fn ctx_bb_has_upper_middle_lower_keys_matching_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_band_indicator_matches_dataset(&dataset, IndicatorKind::Bb, "bb");
}

#[test]
fn ctx_keltner_has_upper_middle_lower_keys_matching_dataset_indicator() {
    let dataset = deterministic_dataset(400);
    assert_ctx_band_indicator_matches_dataset(&dataset, IndicatorKind::Keltner, "keltner");
}

#[test]
fn insufficient_bars_return_unit_and_using_unit_arithmetically_is_an_error() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();

    let returns_unit = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("let v = ctx.sma(\"H4\", 1000, 0); if v == () { 1 } else { -1 }"),
        &empty_params(),
        state(5),
    );
    assert_eq!(returns_unit, Ok(1), "insufficient data must yield ()");

    let arithmetic_on_unit = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("let v = ctx.sma(\"H4\", 1000, 0); v + 1"),
        &empty_params(),
        state(5),
    );
    assert!(
        arithmetic_on_unit.is_err(),
        "using () in arithmetic must be a runtime error, got {arithmetic_on_unit:?}"
    );
}

#[test]
fn band_indicators_return_unit_when_bars_are_insufficient() {
    let bars_with_only_two_completed_m15_bars_by_t5 = 30;
    let dataset = deterministic_dataset(bars_with_only_two_completed_m15_bars_by_t5);
    let cfg = SimConfig::default();
    let t_with_two_completed_m15_bars = 5;
    let period_exceeding_the_completed_m15_bars = 5;

    for fn_name in ["bb", "keltner"] {
        let body = format!(
            "let v = ctx.{fn_name}(\"M15\", {period_exceeding_the_completed_m15_bars}, 2, 0); if v == () {{ 1 }} else {{ -1 }}"
        );
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(&body),
            &empty_params(),
            state(t_with_two_completed_m15_bars),
        );
        assert_eq!(result, Ok(1), "{fn_name}: insufficient data must yield ()");
    }
}

#[test]
fn exceeding_a_sandbox_limit_in_on_bar_is_a_runtime_error() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();

    let cases: Vec<(&str, String)> = vec![
        (
            "deep recursion trips the sandbox's call-depth guard",
            "fn params() { #{} }\nfn rec(n) { if n <= 0 { 0 } else { rec(n - 1) } }\nfn on_bar(ctx, p) { rec(200) }".to_string(),
        ),
        (
            "string length beyond MAX_STRING_SIZE",
            wrap_on_bar("let s = \"\"; for i in 0..500 { s += \"0123456789\"; } 0"),
        ),
        (
            "array length beyond MAX_ARRAY_SIZE",
            wrap_on_bar("let a = []; for i in 0..2000 { a.push(i); } 0"),
        ),
    ];

    for (name, source) in cases {
        let result = run_single(&dataset, &cfg, &source, &empty_params(), state(0));
        assert!(
            result.is_err(),
            "case '{name}' should exceed the sandbox limit, got {result:?}"
        );
    }
}

#[test]
fn compile_rejects_an_object_map_literal_exceeding_max_map_size() {
    let host = ScriptHost::new(&SimConfig::default());
    let fields: Vec<String> = (0..300).map(|i| format!("f{i}: {i}")).collect();
    let source = format!(
        "fn params() {{ #{{}} }}\nfn on_bar(ctx, p) {{ let m = #{{ {} }}; 0 }}",
        fields.join(", ")
    );
    assert!(
        host.compile(&source).is_err(),
        "a 300-field object map literal exceeds MAX_MAP_SIZE and must be rejected"
    );
}

#[test]
fn mult_accepts_both_int_and_float_and_rounds_nearby_values_to_the_same_key() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();

    let int_vs_float = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "let a = ctx.bb(\"M15\", 5, 2, 0);\n",
            "let b = ctx.bb(\"M15\", 5, 2.0, 0);\n",
            "if a.upper == b.upper && a.middle == b.middle && a.lower == b.lower { 1 } else { -1 }"
        )),
        &empty_params(),
        state(T_MID),
    );
    assert_eq!(
        int_vs_float,
        Ok(1),
        "mult=2 (int) and mult=2.0 (float) must agree"
    );

    let rounded_same_key = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar(concat!(
            "let a = ctx.bb(\"M15\", 5, 2.004, 0);\n",
            "let b = ctx.bb(\"M15\", 5, 2.0, 0);\n",
            "if a.upper == b.upper && a.middle == b.middle && a.lower == b.lower { 1 } else { -1 }"
        )),
        &empty_params(),
        state(T_MID),
    );
    assert_eq!(
        rounded_same_key,
        Ok(1),
        "mult=2.004 and mult=2.0 must round to the same key"
    );
}

#[test]
fn invalid_ctx_arguments_are_runtime_errors() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();

    let cases: &[(&str, &str)] = &[
        ("unknown tf", "ctx.close(\"D1\", 0)"),
        ("period = 0", "ctx.sma(\"M15\", 0, 0)"),
        ("period = 1001", "ctx.sma(\"M15\", 1001, 0)"),
        ("shift = -1", "ctx.close(\"M15\", -1)"),
        ("shift = 1001", "ctx.close(\"M15\", 1001)"),
        ("mult = 0.01", "ctx.bb(\"M15\", 5, 0.01, 0)"),
        ("mult = 11", "ctx.bb(\"M15\", 5, 11, 0)"),
        ("period as float", "ctx.sma(\"M15\", 5.0, 0)"),
    ];

    for (name, expr) in cases {
        let body = format!("let v = {expr};\n0");
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(&body),
            &empty_params(),
            state(T_MID),
        );
        assert!(
            result.is_err(),
            "case '{name}' ({expr}) should be a runtime error, got {result:?}"
        );
    }
}

#[test]
fn requesting_more_than_64_distinct_indicator_keys_fails_on_the_65th_across_calls() {
    let dataset = deterministic_dataset(400);
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = host
        .compile(&wrap_on_bar(concat!(
            "if ctx.bars_held == 0 { for i in 1..61 { ctx.sma(\"M5\", i, 0); } return 0; }\n",
            "if ctx.bars_held == 1 { for i in 61..65 { ctx.sma(\"M5\", i, 0); } return 0; }\n",
            "ctx.sma(\"M5\", 65, 0);\n",
            "0"
        )))
        .expect("valid script");
    let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

    let call1 = run
        .on_bar(BarState {
            t: 0,
            position: 0,
            entry_price_milli: 0,
            bars_held: 0,
        })
        .map_err(|e| format!("{e:?}"));
    assert_eq!(
        call1,
        Ok(0),
        "first 60 distinct keys must succeed: {call1:?}"
    );

    let call2 = run
        .on_bar(BarState {
            t: 1,
            position: 0,
            entry_price_milli: 0,
            bars_held: 1,
        })
        .map_err(|e| format!("{e:?}"));
    assert_eq!(
        call2,
        Ok(0),
        "keys 61..=64 (total 64) must succeed: {call2:?}"
    );

    let call3 = run.on_bar(BarState {
        t: 2,
        position: 0,
        entry_price_milli: 0,
        bars_held: 2,
    });
    assert!(call3.is_err(), "the 65th distinct key must fail: {call3:?}");
}

#[test]
fn this_persists_across_on_bar_calls() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();
    let host = ScriptHost::new(&cfg);
    let script = host
        .compile(&wrap_on_bar(
            "if this.seen == 1 { return 1; } this.seen = 1; 0",
        ))
        .expect("valid script");
    let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

    let first = run.on_bar(state(0)).map_err(|e| format!("{e:?}"));
    assert_eq!(first, Ok(0), "first call: `this.seen` is not set yet");
    let second = run.on_bar(state(1)).map_err(|e| format!("{e:?}"));
    assert_eq!(
        second,
        Ok(1),
        "second call must observe `this.seen` written by the first call"
    );
}

#[test]
fn on_bar_returning_a_value_other_than_1_minus_1_or_0_is_an_error() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();

    for (name, body) in [
        ("int 2", "2"),
        ("float 1.0", "1.0"),
        ("string", "\"x\""),
        ("unit", "()"),
    ] {
        let result = run_single(
            &dataset,
            &cfg,
            &wrap_on_bar(body),
            &empty_params(),
            state(0),
        );
        assert!(
            result.is_err(),
            "on_bar returning {name} must be an error, got {result:?}"
        );
    }
}

#[test]
fn an_infinite_loop_in_on_bar_is_an_error_under_a_small_max_operations_per_bar() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig {
        max_operations_per_bar: 10_000,
        ..SimConfig::default()
    };

    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("loop {}"),
        &empty_params(),
        state(0),
    );
    assert!(result.is_err());
}

#[test]
fn repeated_calls_eventually_fail_once_max_operations_per_run_is_exhausted() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig {
        max_operations_per_run: 50,
        ..SimConfig::default()
    };
    let host = ScriptHost::new(&cfg);
    let script = host
        .compile(&wrap_on_bar("let i = 0; while i < 5 { i += 1; } 0"))
        .expect("valid script");
    let mut run = ScriptRun::new(&host, &script, &empty_params(), dataset, &cfg);

    let results: Vec<Result<i8, DeciderError>> = (0..20).map(|t| run.on_bar(state(t))).collect();
    assert!(
        results[0].is_ok(),
        "first call must succeed: {:?}",
        results[0]
    );
    assert!(
        results.iter().any(|r| r.is_err()),
        "the run must eventually fail once max_operations_per_run is exhausted: {results:?}"
    );
    let first_err = results.iter().position(|r| r.is_err()).unwrap();
    assert!(
        results[first_err..].iter().all(|r| r.is_err()),
        "once the run budget is exhausted, it must not recover: {results:?}"
    );
}

#[test]
fn import_statement_fails_at_execution_via_the_dummy_module_resolver() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("import \"x\" as y;\n0"),
        &empty_params(),
        state(0),
    );
    assert!(
        result.is_err(),
        "import must fail at execution, got {result:?}"
    );
}

#[test]
fn sleep_fails_at_execution_because_lang_core_is_not_registered() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("sleep(1); 0"),
        &empty_params(),
        state(0),
    );
    assert!(
        result.is_err(),
        "sleep must fail at execution, got {result:?}"
    );
}

#[test]
fn timestamp_fails_at_execution_because_basic_time_is_not_registered() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("let t = timestamp(); 0"),
        &empty_params(),
        state(0),
    );
    assert!(
        result.is_err(),
        "timestamp must fail at execution, got {result:?}"
    );
}

#[test]
fn rand_fails_at_execution_because_no_package_registers_it() {
    let dataset = deterministic_dataset(10);
    let cfg = SimConfig::default();
    let result = run_single(
        &dataset,
        &cfg,
        &wrap_on_bar("let r = rand(); 0"),
        &empty_params(),
        state(0),
    );
    assert!(
        result.is_err(),
        "rand must fail at execution, got {result:?}"
    );
}

#[test]
fn eval_fails_registration_because_the_symbol_is_disabled() {
    let host = ScriptHost::new(&SimConfig::default());
    match host.compile(&wrap_on_bar("eval(\"1\")")) {
        Err(SimError::InvalidScript(_)) => {}
        Err(other) => {
            panic!("eval should be rejected as InvalidScript, got a different SimError: {other}")
        }
        Ok(_) => panic!(
            "eval should be rejected at registration (disabled symbol), but the script compiled successfully"
        ),
    }
}

#[test]
fn concurrent_script_runs_on_different_threads_do_not_mix_run_totals_or_this() {
    let host = ScriptHost::new(&SimConfig::default());
    let script = host
        .compile(&wrap_on_bar(concat!(
            "let i = 0; while i < 30 { i += 1; }\n",
            "if ctx.bars_held == 0 { this.tag = p.tag; return 0; }\n",
            "if this.tag == p.tag { 1 } else { -1 }"
        )))
        .expect("valid script");
    let dataset = deterministic_dataset(60);

    let outcome_a: Mutex<Option<Vec<Result<i8, DeciderError>>>> = Mutex::new(None);
    let outcome_b: Mutex<Option<Vec<Result<i8, DeciderError>>>> = Mutex::new(None);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            let cfg = SimConfig {
                max_operations_per_run: 50,
                ..SimConfig::default()
            };
            let mut params = ParamSet::new();
            params.insert("tag".to_string(), ParamValue::Int(1));
            let mut run = ScriptRun::new(&host, &script, &params, dataset.clone(), &cfg);
            let results: Vec<_> = (0..10)
                .map(|i| {
                    run.on_bar(BarState {
                        t: i,
                        position: 0,
                        entry_price_milli: 0,
                        bars_held: i as u32,
                    })
                })
                .collect();
            *outcome_a.lock().unwrap() = Some(results);
        });
        scope.spawn(|| {
            let cfg = SimConfig::default();
            let mut params = ParamSet::new();
            params.insert("tag".to_string(), ParamValue::Int(2));
            let mut run = ScriptRun::new(&host, &script, &params, dataset.clone(), &cfg);
            let results: Vec<_> = (0..10)
                .map(|i| {
                    run.on_bar(BarState {
                        t: i,
                        position: 0,
                        entry_price_milli: 0,
                        bars_held: i as u32,
                    })
                })
                .collect();
            *outcome_b.lock().unwrap() = Some(results);
        });
    });

    let results_a = outcome_a.into_inner().unwrap().unwrap();
    let results_b = outcome_b.into_inner().unwrap().unwrap();

    assert!(
        !results_a.iter().any(|r| r.as_ref().ok() == Some(&-1)),
        "thread A observed a `this` value that does not match its own tag: {results_a:?}"
    );
    assert!(
        !results_b.iter().any(|r| r.as_ref().ok() == Some(&-1)),
        "thread B observed a `this` value that does not match its own tag: {results_b:?}"
    );
    assert!(
        results_a.iter().any(|r| r.is_err()),
        "thread A (small run budget) should eventually fail: {results_a:?}"
    );
    assert!(
        results_b.iter().all(|r| r.is_ok()),
        "thread B (large run budget) must not be affected by thread A's small budget: {results_b:?}"
    );
}

#[test]
fn bar_end_time_adds_bar_length() {
    assert_eq!(ctx::bar_end_time(1_000).unwrap(), 1_000 + M5_SECS);
}

#[test]
fn bar_end_time_rejects_i64_overflow() {
    let err = ctx::bar_end_time(i64::MAX).unwrap_err().to_string();
    assert!(
        err.contains("invalid ctx.time: open_time plus the bar length overflows i64"),
        "{err}"
    );
}
