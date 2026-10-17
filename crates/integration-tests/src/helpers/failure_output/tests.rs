use super::*;

#[test]
fn format_failure_contains_all_sections() {
    let logs = vec!["INFO starting".to_string(), "ERROR boom".to_string()];
    let report = format_failure(
        "my_test",
        "smoke.csv",
        "balance=100",
        "balance=90",
        &logs,
        "trading_accounts: 1 row",
    );

    assert!(report.contains("[FAIL] my_test"));
    assert!(report.contains("fixture: smoke.csv"));
    assert!(report.contains("expected: balance=100"));
    assert!(report.contains("actual: balance=90"));
    assert!(report.contains("=== application log ==="));
    assert!(report.contains("INFO starting"));
    assert!(report.contains("ERROR boom"));
    assert!(report.contains("=== db state ==="));
    assert!(report.contains("trading_accounts: 1 row"));
    assert!(report.contains("=== git diff (last 1 commit) ==="));
}
