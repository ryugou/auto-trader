use super::*;

fn catalog(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn find_missing_from_catalog_returns_registered_names_absent_from_catalog_in_registration_order() {
    let cases = [
        (
            "returns_only_registered_names_absent_from_catalog",
            vec!["strategy_a", "strategy_b", "strategy_c"],
            vec!["strategy_a", "strategy_c"],
            vec!["strategy_b"],
        ),
        (
            "returns_empty_when_all_registered_are_in_catalog",
            vec!["strategy_a", "strategy_b"],
            vec!["strategy_a", "strategy_b", "strategy_c"],
            vec![],
        ),
        (
            "returns_all_registered_in_order_when_catalog_is_empty",
            vec!["strategy_a", "strategy_b"],
            vec![],
            vec!["strategy_a", "strategy_b"],
        ),
        (
            "returns_empty_when_registered_is_empty",
            vec![],
            vec!["strategy_a"],
            vec![],
        ),
    ];

    for (name, registered, catalog_names, expected) in cases {
        let catalog = catalog(&catalog_names);

        let actual = find_missing_from_catalog(&registered, &catalog);

        assert_eq!(actual, expected, "{name}");
    }
}
