use std::collections::HashMap;

use auto_trader_core::config::StrategyConfig;

use super::*;

fn strategy(name: &str, enabled: bool) -> StrategyConfig {
    StrategyConfig {
        name: name.to_string(),
        enabled,
        mode: "paper".to_string(),
        pairs: vec![],
        params: HashMap::new(),
    }
}

#[test]
fn has_enabled_fx_strategy_is_true_only_for_enabled_swing_llm_strategy() {
    let cases = [
        (
            "enabled_swing_llm_strategy_is_fx_strategy",
            vec![strategy("swing_llm_usdjpy", true)],
            true,
        ),
        (
            "disabled_swing_llm_strategy_is_not_fx_strategy",
            vec![strategy("swing_llm_usdjpy", false)],
            false,
        ),
        (
            "enabled_non_swing_llm_strategy_is_not_fx_strategy",
            vec![strategy("donchian_trend", true)],
            false,
        ),
        ("empty_strategy_list_is_not_fx_strategy", vec![], false),
    ];

    for (name, strategies, expected) in cases {
        let result = has_enabled_fx_strategy(&strategies);

        assert_eq!(result, expected, "case {name}");
    }
}
