use super::*;

#[test]
fn rejects_non_finite_pip_offsets() {
    assert!(!pip_offsets_are_tradable(f64::NAN, 100.0));
    assert!(!pip_offsets_are_tradable(100.0, f64::INFINITY));
}

#[test]
fn rejects_non_positive_pip_offsets() {
    assert!(!pip_offsets_are_tradable(0.0, 100.0));
    assert!(!pip_offsets_are_tradable(100.0, -1.0));
}

#[test]
fn rejects_pip_offsets_exceeding_max() {
    assert!(!pip_offsets_are_tradable(10_000.1, 100.0));
    assert!(!pip_offsets_are_tradable(100.0, 10_000.1));
}

#[test]
fn accepts_pip_offsets_within_bounds() {
    assert!(pip_offsets_are_tradable(100.0, 200.0));
    assert!(pip_offsets_are_tradable(10_000.0, 10_000.0));
}

#[test]
fn rejects_non_positive_entry_price() {
    assert!(!entry_price_is_tradable(Decimal::ZERO));
    assert!(!entry_price_is_tradable(dec!(-1)));
}

#[test]
fn accepts_positive_entry_price() {
    assert!(entry_price_is_tradable(dec!(150.25)));
}

#[test]
fn strips_markdown_json_fence_and_trims_whitespace() {
    assert_eq!(
        strip_markdown_code_fence("```json\n{\"a\":1}\n```"),
        "{\"a\":1}"
    );
    assert_eq!(strip_markdown_code_fence("  {\"a\":1}  "), "{\"a\":1}");
}
