use super::*;

#[test]
fn trimmed_non_empty_trims_whitespace_and_returns_none_for_blank_input() {
    let cases = [
        (
            "surrounded_by_whitespace_trims",
            "  abc  ",
            Some("abc".to_string()),
        ),
        ("only_whitespace_is_none", "   ", None),
        ("empty_string_is_none", "", None),
    ];

    for (name, input, expected) in cases {
        let actual = trimmed_non_empty(input.to_string());

        assert_eq!(actual, expected, "{name}");
    }
}
