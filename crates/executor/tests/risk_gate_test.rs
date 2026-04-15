use auto_trader_executor::risk_gate::{GateDecision, RejectReason, eval_price_freshness};

#[test]
fn rejects_when_price_tick_is_stale() {
    let decision = eval_price_freshness(60, 90);
    match decision {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs: a }) => assert_eq!(a, 90),
        other => panic!("expected PriceTickStale, got {:?}", other),
    }
}

#[test]
fn passes_when_price_tick_is_fresh() {
    assert!(matches!(eval_price_freshness(60, 10), GateDecision::Pass));
}

#[test]
fn rejects_on_exact_limit_breach() {
    // age == freshness_secs is still Pass (> not >=)
    assert!(matches!(eval_price_freshness(60, 60), GateDecision::Pass));
}

#[test]
fn rejects_when_age_is_one_over_limit() {
    assert!(matches!(
        eval_price_freshness(60, 61),
        GateDecision::Reject(RejectReason::PriceTickStale { .. })
    ));
}
