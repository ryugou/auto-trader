use super::*;

#[test]
fn parse_live_dry_run_override_accepts_bool_like_values_and_falls_back_otherwise() {
    let cases = [
        (
            "true_like_whitespace_padded_mixed_case",
            Some(" True\n"),
            false,
            true,
        ),
        ("true_like_1", Some("1"), false, true),
        ("true_like_yes", Some("yes"), false, true),
        ("true_like_on", Some("on"), false, true),
        ("false_like_mixed_case", Some("False"), true, false),
        ("false_like_0", Some("0"), true, false),
        ("false_like_no", Some("no"), true, false),
        ("false_like_off", Some("off"), true, false),
        ("absent_falls_back_to_true", None, true, true),
        ("absent_falls_back_to_false", None, false, false),
        ("unrecognized_falls_back_to_true", Some("maybe"), true, true),
        (
            "unrecognized_falls_back_to_false",
            Some("maybe"),
            false,
            false,
        ),
    ];

    for (name, raw, config_dry_run, expected) in cases {
        let actual = parse_live_dry_run_override(raw, config_dry_run);

        assert_eq!(actual, expected, "{name}");
    }
}
