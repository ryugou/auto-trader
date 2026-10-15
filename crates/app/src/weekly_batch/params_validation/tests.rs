use super::*;

#[test]
fn validate_params_donchian_normalizes_and_drops_extras() {
    let proposed = serde_json::json!({
        "entry_channel": 20,
        "exit_channel": 10,
        "atr_baseline_bars": 50,
        "sl_pct": 0.03,
    });
    let current = serde_json::json!({});
    let v = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap();
    assert_eq!(v["entry_channel"], 20);
    assert_eq!(v["exit_channel"], 10);
    assert_eq!(v["atr_baseline_bars"], 50);
    assert!(v.get("sl_pct").is_none(), "extra keys should be dropped");
}

#[test]
fn validate_params_donchian_rejects_out_of_range() {
    let proposed = serde_json::json!({
        "entry_channel": 100,
        "exit_channel": 10,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid entry_channel: out of range [10, 30]"
    );
}

#[test]
fn validate_params_donchian_rejects_exit_ge_entry() {
    let proposed = serde_json::json!({
        "entry_channel": 12,
        "exit_channel": 15,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid exit_channel: must be less than entry_channel"
    );
}

#[test]
fn validate_params_donchian_rejects_missing_key() {
    let proposed = serde_json::json!({
        "exit_channel": 10,
        "atr_baseline_bars": 50,
    });
    let current = serde_json::json!({});
    let err = validate_params("donchian_trend_evolve_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("entry_channel"),
        "error should name the missing key: {err}"
    );
}

#[test]
fn validate_params_permissive_accepts_matching_key_set() {
    let proposed = serde_json::json!({ "window": 20, "threshold": 1.5 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let v = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap();
    assert_eq!(v["window"], 20);
}

#[test]
fn validate_params_permissive_rejects_empty_object() {
    let proposed = serde_json::json!({});
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_non_object() {
    let proposed = serde_json::json!([1, 2, 3]);
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_null_values() {
    let proposed = serde_json::json!({"window": null});
    let current = serde_json::json!({ "window": 15 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_err());
}

#[test]
fn validate_params_permissive_rejects_added_key() {
    let proposed = serde_json::json!({ "window": 20, "threshold": 1.5, "rogue": 99 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("rogue"),
        "error should name the extra key: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_dropped_key() {
    let proposed = serde_json::json!({ "window": 20 });
    let current = serde_json::json!({ "window": 15, "threshold": 1.0 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("threshold"),
        "error should name the dropped key: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_type_swap() {
    let proposed = serde_json::json!({ "window": "20" });
    let current = serde_json::json!({ "window": 15 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("type mismatch"),
        "error should flag type mismatch: {err}"
    );
}

#[test]
fn validate_params_permissive_rejects_int_to_float_swap() {
    let proposed = serde_json::json!({ "window": 20.5 });
    let current = serde_json::json!({ "window": 20 });
    let err = validate_params("bb_mean_revert_v1", &proposed, &current).unwrap_err();
    assert!(
        format!("{err}").contains("type mismatch"),
        "int→float swap should be rejected: {err}"
    );
}

#[test]
fn validate_params_permissive_accepts_float_to_float() {
    let proposed = serde_json::json!({ "threshold": 1.6 });
    let current = serde_json::json!({ "threshold": 1.2 });
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
}

#[test]
fn validate_params_permissive_relaxes_when_current_empty() {
    let proposed = serde_json::json!({ "anything": 1 });
    let current = serde_json::json!({});
    assert!(validate_params("bb_mean_revert_v1", &proposed, &current).is_ok());
}
