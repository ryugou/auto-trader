use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

use crate::types::{Direction, Exchange};

pub fn estimate(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

pub fn sfd_daily_rate(divergence: Decimal) -> Decimal {
    let d = divergence.abs();
    if d < dec!(0.05) {
        Decimal::ZERO
    } else if d < dec!(0.10) {
        dec!(0.0025)
    } else if d < dec!(0.15) {
        dec!(0.005)
    } else if d < dec!(0.20) {
        dec!(0.01)
    } else {
        dec!(0.03)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SfdContext {
    pub fx_price: Decimal,
    pub spot_price: Decimal,
    pub position_notional: Decimal,
    pub direction: Direction,
}

pub fn compute_hourly_sfd(ctx: SfdContext) -> Decimal {
    if ctx.spot_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let divergence = (ctx.fx_price - ctx.spot_price) / ctx.spot_price;
    let rate = sfd_daily_rate(divergence);
    if rate.is_zero() {
        return Decimal::ZERO;
    }
    let magnitude = (ctx.position_notional * rate / Decimal::from(24))
        .round_dp_with_strategy(0, RoundingStrategy::ToZero);
    let sign_positive = match (divergence.is_sign_positive(), ctx.direction) {
        (true, Direction::Long) => true,
        (true, Direction::Short) => false,
        (false, Direction::Long) => false,
        (false, Direction::Short) => true,
    };
    if sign_positive { magnitude } else { -magnitude }
}

#[cfg(test)]
mod tests;
