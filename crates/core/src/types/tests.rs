use rust_decimal_macros::dec;

use super::*;

#[test]
fn pair_display_format() {
    let pair = Pair::new("USD_JPY");
    assert_eq!(pair.to_string(), "USD_JPY");
}

#[test]
fn direction_serializes_snake_case() {
    let json = serde_json::to_string(&Direction::Long).unwrap();
    assert_eq!(json, r#""long""#);
}

#[test]
fn exchange_serializes_snake_case() {
    let json = serde_json::to_string(&Exchange::BitflyerCfd).unwrap();
    assert_eq!(json, r#""bitflyer_cfd""#);
    let json = serde_json::to_string(&Exchange::Oanda).unwrap();
    assert_eq!(json, r#""oanda""#);
    let json = serde_json::to_string(&Exchange::GmoFx).unwrap();
    assert_eq!(json, r#""gmo_fx""#);
}

#[test]
fn exchange_display() {
    assert_eq!(Exchange::Oanda.as_str(), "oanda");
    assert_eq!(Exchange::BitflyerCfd.as_str(), "bitflyer_cfd");
    assert_eq!(Exchange::GmoFx.as_str(), "gmo_fx");
}

#[test]
fn signal_roundtrip() {
    let signal = Signal {
        strategy_name: "test".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        stop_loss_pct: dec!(0.005),
        take_profit_pct: Some(dec!(0.01)),
        confidence: 0.8,
        timestamp: Utc::now(),
        allocation_pct: dec!(0.5),
        max_hold_until: None,
    };
    let json = serde_json::to_string(&signal).unwrap();
    let back: Signal = serde_json::from_str(&json).unwrap();
    assert_eq!(back.pair, signal.pair);
    assert_eq!(back.direction, Direction::Long);
}

#[test]
fn trade_status_roundtrip() {
    for status in [TradeStatus::Open, TradeStatus::Closing, TradeStatus::Closed] {
        let json = serde_json::to_string(&status).unwrap();
        let back: TradeStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }
}

#[test]
fn signal_deserialize_without_allocation_pct_falls_back_to_default() {
    let legacy_json = r#"{
        "strategy_name": "legacy",
        "pair": "USD_JPY",
        "direction": "long",
        "stop_loss_pct": "0.005",
        "confidence": 0.8,
        "timestamp": "2024-01-01T00:00:00Z"
    }"#;
    let signal: Signal = serde_json::from_str(legacy_json).expect("Signal must deserialize");
    assert_eq!(signal.allocation_pct, dec!(0.5));
    assert!(signal.max_hold_until.is_none());
    assert!(signal.take_profit_pct.is_none());
}

#[test]
fn signal_with_take_profit_pct_roundtrip() {
    let signal = Signal {
        strategy_name: "s".to_string(),
        pair: Pair::new("USD_JPY"),
        direction: Direction::Short,
        stop_loss_pct: dec!(0.005),
        take_profit_pct: Some(dec!(0.015)),
        confidence: 0.75,
        timestamp: Utc::now(),
        allocation_pct: dec!(0.3),
        max_hold_until: None,
    };
    let json = serde_json::to_string(&signal).unwrap();
    let back: Signal = serde_json::from_str(&json).unwrap();
    assert_eq!(back.take_profit_pct, Some(dec!(0.015)));
    assert_eq!(back.stop_loss_pct, dec!(0.005));
}

#[test]
fn trade_status_as_str() {
    assert_eq!(TradeStatus::Open.as_str(), "open");
    assert_eq!(TradeStatus::Closing.as_str(), "closing");
    assert_eq!(TradeStatus::Closed.as_str(), "closed");
}

#[test]
fn exit_reason_as_str_values_are_stable_across_versions() {
    let cases = [
        (ExitReason::TpHit, "tp_hit"),
        (ExitReason::SlHit, "sl_hit"),
        (ExitReason::Manual, "manual"),
        (ExitReason::SignalReverse, "signal_reverse"),
        (ExitReason::StrategyMeanReached, "strategy_mean_reached"),
        (
            ExitReason::StrategyTrailingChannel,
            "strategy_trailing_channel",
        ),
        (ExitReason::StrategyTrailingMa, "strategy_trailing_ma"),
        (
            ExitReason::StrategyIndicatorReversal,
            "strategy_indicator_reversal",
        ),
        (ExitReason::StrategyTimeLimit, "strategy_time_limit"),
        (ExitReason::Reconciled, "reconciled"),
        (ExitReason::Liquidation, "liquidation"),
    ];
    for (reason, tag) in cases {
        assert_eq!(reason.as_str(), tag);
        let parsed: ExitReason = tag.parse().unwrap();
        assert_eq!(parsed.as_str(), tag);
    }
}

#[test]
fn exit_reason_from_str_rejects_unsupported_value() {
    let err = "not_a_reason".parse::<ExitReason>().unwrap_err();
    assert!(err.to_string().contains("unsupported exit_reason"));
}

#[test]
fn direction_from_str_rejects_unsupported_value() {
    let err = "sideways".parse::<Direction>().unwrap_err();
    assert!(err.to_string().contains("unsupported direction"));
}

#[test]
fn trade_status_from_str_rejects_unsupported_value() {
    let err = "pending".parse::<TradeStatus>().unwrap_err();
    assert!(err.to_string().contains("unsupported trade_status"));
}

#[test]
fn exchange_from_str_rejects_unsupported_value() {
    let err = "unknown".parse::<Exchange>().unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("unsupported exchange: unknown"),
        "{message}"
    );
}
