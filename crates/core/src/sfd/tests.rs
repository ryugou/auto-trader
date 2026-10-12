use super::*;

#[test]
fn estimate_all_exchanges_currently_zero() {
    for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
        assert_eq!(estimate(ex, dec!(150), dec!(1)), Decimal::ZERO);
    }
}

#[test]
fn sfd_daily_rate_below_5pct_is_zero() {
    assert_eq!(sfd_daily_rate(dec!(0)), Decimal::ZERO);
    assert_eq!(sfd_daily_rate(dec!(0.04999)), Decimal::ZERO);
}

#[test]
fn sfd_daily_rate_5_to_10pct_is_0_25pct() {
    assert_eq!(sfd_daily_rate(dec!(0.05)), dec!(0.0025));
    assert_eq!(sfd_daily_rate(dec!(0.09999)), dec!(0.0025));
}

#[test]
fn sfd_daily_rate_10_to_15pct_is_0_5pct() {
    assert_eq!(sfd_daily_rate(dec!(0.10)), dec!(0.005));
    assert_eq!(sfd_daily_rate(dec!(0.14999)), dec!(0.005));
}

#[test]
fn sfd_daily_rate_15_to_20pct_is_1pct() {
    assert_eq!(sfd_daily_rate(dec!(0.15)), dec!(0.01));
    assert_eq!(sfd_daily_rate(dec!(0.19999)), dec!(0.01));
}

#[test]
fn sfd_daily_rate_20pct_or_more_is_3pct() {
    assert_eq!(sfd_daily_rate(dec!(0.20)), dec!(0.03));
    assert_eq!(sfd_daily_rate(dec!(0.50)), dec!(0.03));
}

#[test]
fn sfd_daily_rate_normalizes_negative_input_via_abs() {
    assert_eq!(sfd_daily_rate(dec!(-0.10)), dec!(0.005));
    assert_eq!(sfd_daily_rate(dec!(-0.07)), dec!(0.0025));
    assert_eq!(sfd_daily_rate(dec!(-0.04)), Decimal::ZERO);
    assert_eq!(sfd_daily_rate(dec!(-0.25)), dec!(0.03));
}

fn ctx(fx: Decimal, spot: Decimal, notional: Decimal, dir: Direction) -> SfdContext {
    SfdContext {
        fx_price: fx,
        spot_price: spot,
        position_notional: notional,
        direction: dir,
    }
}

#[test]
fn compute_hourly_sfd_zero_when_divergence_below_5pct() {
    let c = ctx(dec!(104), dec!(100), dec!(100_000), Direction::Long);
    assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
}

fn truncated_hourly(notional: Decimal, daily_rate: Decimal) -> Decimal {
    (notional * daily_rate / dec!(24)).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

#[test]
fn compute_hourly_sfd_long_pays_when_fx_above_spot() {
    let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Long);
    let fee = compute_hourly_sfd(c);
    assert!(fee > Decimal::ZERO);
    assert_eq!(fee, truncated_hourly(dec!(100_000), dec!(0.005)));
    assert_eq!(fee, dec!(20));
}

#[test]
fn compute_hourly_sfd_short_receives_when_fx_above_spot() {
    let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Short);
    let fee = compute_hourly_sfd(c);
    assert!(fee < Decimal::ZERO);
    assert_eq!(fee, -truncated_hourly(dec!(100_000), dec!(0.005)));
}

#[test]
fn compute_hourly_sfd_long_receives_when_fx_below_spot() {
    let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Long);
    let fee = compute_hourly_sfd(c);
    assert!(fee < Decimal::ZERO);
    assert_eq!(fee, -truncated_hourly(dec!(100_000), dec!(0.005)));
}

#[test]
fn compute_hourly_sfd_short_pays_when_fx_below_spot() {
    let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Short);
    let fee = compute_hourly_sfd(c);
    assert!(fee > Decimal::ZERO);
    assert_eq!(fee, truncated_hourly(dec!(100_000), dec!(0.005)));
}

#[test]
fn compute_hourly_sfd_truncates_fractional_yen_to_zero() {
    let c = ctx(dec!(110), dec!(100), dec!(100), Direction::Long);
    let fee = compute_hourly_sfd(c);
    assert_eq!(fee, Decimal::ZERO);
}

#[test]
fn compute_hourly_sfd_zero_spot_returns_zero_no_panic() {
    let c = ctx(dec!(100), Decimal::ZERO, dec!(100_000), Direction::Long);
    assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
}
