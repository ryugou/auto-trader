use rust_decimal_macros::dec;
use serde_json::json;

use super::*;

#[test]
fn regime_from_indicators_returns_expected_regime_per_case() {
    let cases = [
        (
            "regime_string_value",
            Some(json!({"regime": "trend"})),
            "trend",
        ),
        ("key_missing", Some(json!({"other": "value"})), "unknown"),
        ("indicators_absent", None, "unknown"),
        (
            "value_is_not_a_string",
            Some(json!({"regime": 42})),
            "unknown",
        ),
    ];

    for (name, indicators, expected) in cases {
        let regime = regime_from_indicators(indicators.as_ref());

        assert_eq!(regime, expected, "case {name}");
    }
}

#[test]
fn feedback_comment_formats_pnl_and_regime() {
    let comment = feedback_comment(dec!(123.45), "trend");

    assert_eq!(comment, "PnL: 123.45, regime: trend");
}
