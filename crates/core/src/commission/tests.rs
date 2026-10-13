use rust_decimal_macros::dec;

use super::*;

#[test]
fn estimate_all_exchanges_currently_zero_for_open() {
    for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
        assert_eq!(
            estimate(Action::Open, ex, dec!(150), dec!(1)),
            Decimal::ZERO
        );
    }
}

#[test]
fn estimate_all_exchanges_currently_zero_for_close() {
    for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
        assert_eq!(
            estimate(Action::Close, ex, dec!(150), dec!(1)),
            Decimal::ZERO
        );
    }
}
