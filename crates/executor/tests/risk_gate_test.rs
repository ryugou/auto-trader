use auto_trader_executor::risk_gate::{GateDecision, RejectReason, RiskGate, RiskGateConfig};
use rust_decimal_macros::dec;

fn sample_config() -> RiskGateConfig {
    RiskGateConfig {
        daily_loss_limit_pct: dec!(0.05),
        price_freshness_secs: 60,
    }
}

#[test]
fn rejects_when_price_tick_is_stale() {
    let cfg = sample_config();
    let decision = RiskGate::eval_price_freshness(&cfg, 90);
    match decision {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs: a }) => assert_eq!(a, 90),
        other => panic!("expected PriceTickStale, got {:?}", other),
    }
}

#[test]
fn passes_when_price_tick_is_fresh() {
    let cfg = sample_config();
    assert!(matches!(RiskGate::eval_price_freshness(&cfg, 10), GateDecision::Pass));
}

#[test]
fn rejects_when_daily_loss_exceeds_limit() {
    let cfg = sample_config();
    let decision = RiskGate::eval_kill_switch(&cfg, dec!(30000), dec!(-1400), dec!(-200));
    match decision {
        GateDecision::Reject(RejectReason::DailyLossLimitExceeded { loss, limit }) => {
            assert_eq!(loss, dec!(-1600));
            assert_eq!(limit, dec!(-1500));
        }
        other => panic!("expected DailyLossLimitExceeded, got {:?}", other),
    }
}

#[test]
fn passes_when_daily_loss_within_limit() {
    let cfg = sample_config();
    assert!(matches!(
        RiskGate::eval_kill_switch(&cfg, dec!(30000), dec!(-500), dec!(-200)),
        GateDecision::Pass
    ));
}

#[test]
fn rejects_on_exact_limit_breach() {
    let cfg = sample_config();
    assert!(matches!(
        RiskGate::eval_kill_switch(&cfg, dec!(30000), dec!(-1500), dec!(0)),
        GateDecision::Reject(RejectReason::DailyLossLimitExceeded { .. })
    ));
}
