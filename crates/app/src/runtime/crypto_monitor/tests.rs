use auto_trader_core::types::ExitReason;
use chrono::Duration;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn detect_exit_reason_returns_expected_reason_per_case() {
    let now = Utc::now();
    let cases = [
        (
            "long_sl_hit_when_price_at_or_below_stop_loss",
            Direction::Long,
            dec!(100),
            dec!(100),
            Some(dec!(120)),
            None,
            Some(ExitReason::SlHit),
        ),
        (
            "long_tp_hit_when_price_at_or_above_take_profit",
            Direction::Long,
            dec!(120),
            dec!(90),
            Some(dec!(120)),
            None,
            Some(ExitReason::TpHit),
        ),
        (
            "long_time_limit_hit_when_neither_sl_nor_tp_reached",
            Direction::Long,
            dec!(105),
            dec!(90),
            Some(dec!(120)),
            Some(now - Duration::hours(1)),
            Some(ExitReason::StrategyTimeLimit),
        ),
        (
            "long_none_when_nothing_hit",
            Direction::Long,
            dec!(105),
            dec!(90),
            Some(dec!(120)),
            Some(now + Duration::hours(1)),
            None,
        ),
        (
            "short_sl_hit_when_price_at_or_above_stop_loss",
            Direction::Short,
            dec!(100),
            dec!(100),
            Some(dec!(80)),
            None,
            Some(ExitReason::SlHit),
        ),
        (
            "short_tp_hit_when_price_at_or_below_take_profit",
            Direction::Short,
            dec!(80),
            dec!(100),
            Some(dec!(80)),
            None,
            Some(ExitReason::TpHit),
        ),
        (
            "sl_takes_priority_over_time_limit_when_both_hit",
            Direction::Long,
            dec!(100),
            dec!(100),
            Some(dec!(120)),
            Some(now - Duration::hours(1)),
            Some(ExitReason::SlHit),
        ),
    ];
    for (name, direction, price, stop_loss, take_profit, time_limit, expected) in cases {
        let reason = detect_exit_reason(direction, price, stop_loss, take_profit, time_limit, now);
        assert_eq!(reason, expected, "case: {name}");
    }
}
