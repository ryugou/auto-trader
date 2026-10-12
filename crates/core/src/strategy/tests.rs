use rust_decimal_macros::dec;

use super::*;

#[test]
fn as_tag_values_are_stable_across_versions() {
    let cases = [
        (StrategyExitReason::MeanReached, "strategy_mean_reached"),
        (
            StrategyExitReason::TrailingChannel,
            "strategy_trailing_channel",
        ),
        (StrategyExitReason::TrailingMa, "strategy_trailing_ma"),
        (
            StrategyExitReason::IndicatorReversal,
            "strategy_indicator_reversal",
        ),
        (StrategyExitReason::TimeLimit, "strategy_time_limit"),
        (StrategyExitReason::Custom("custom_tag"), "custom_tag"),
    ];
    for (reason, tag) in cases {
        assert_eq!(reason.as_tag(), tag);
    }
}

#[test]
fn custom_reason_maps_to_manual_exit_reason() {
    assert_eq!(
        StrategyExitReason::Custom("whatever").to_exit_reason(),
        ExitReason::Manual
    );
}

#[test]
fn non_custom_reasons_map_to_matching_exit_reason() {
    let cases = [
        (
            StrategyExitReason::MeanReached,
            ExitReason::StrategyMeanReached,
        ),
        (
            StrategyExitReason::TrailingChannel,
            ExitReason::StrategyTrailingChannel,
        ),
        (
            StrategyExitReason::TrailingMa,
            ExitReason::StrategyTrailingMa,
        ),
        (
            StrategyExitReason::IndicatorReversal,
            ExitReason::StrategyIndicatorReversal,
        ),
        (StrategyExitReason::TimeLimit, ExitReason::StrategyTimeLimit),
    ];
    for (reason, expected) in cases {
        assert_eq!(reason.to_exit_reason(), expected);
    }
}

#[test]
fn has_reached_one_r_defaults_to_true_for_non_positive_entry_or_stop_loss() {
    assert!(has_reached_one_r(
        &Direction::Long,
        Decimal::ZERO,
        dec!(100),
        dec!(50)
    ));
    assert!(has_reached_one_r(
        &Direction::Long,
        dec!(100),
        Decimal::ZERO,
        dec!(50)
    ));
}

#[test]
fn has_reached_one_r_defaults_to_true_when_stop_loss_is_on_wrong_side() {
    assert!(has_reached_one_r(
        &Direction::Long,
        dec!(100),
        dec!(110),
        dec!(105)
    ));
    assert!(has_reached_one_r(
        &Direction::Short,
        dec!(100),
        dec!(90),
        dec!(95)
    ));
}

#[test]
fn has_reached_one_r_defaults_to_true_when_stop_loss_equals_entry() {
    assert!(has_reached_one_r(
        &Direction::Long,
        dec!(100),
        dec!(100),
        dec!(90)
    ));
    assert!(has_reached_one_r(
        &Direction::Short,
        dec!(100),
        dec!(100),
        dec!(110)
    ));
}

#[test]
fn has_reached_one_r_true_when_profit_meets_or_exceeds_sl_distance() {
    assert!(has_reached_one_r(
        &Direction::Long,
        dec!(100),
        dec!(95),
        dec!(105)
    ));
    assert!(!has_reached_one_r(
        &Direction::Long,
        dec!(100),
        dec!(95),
        dec!(104)
    ));
}
