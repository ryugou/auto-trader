use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

use crate::types::{Direction, Exchange};

/// Returns the estimated SFD charge of a fill: zero for every exchange.
///
/// The fill price and the quantity do not change the result.
pub fn estimate(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

/// Returns the daily SFD rate for a divergence between the FX and spot prices.
///
/// The rate depends on the absolute divergence: 0 below 0.05, 0.0025 below 0.10, 0.005 below 0.15, 0.01 below 0.20 and 0.03 from 0.20 up.
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

/// Inputs of the hourly SFD computation.
#[derive(Debug, Clone, Copy)]
pub struct SfdContext {
    /// FX price.
    pub fx_price: Decimal,
    /// Spot price.
    pub spot_price: Decimal,
    /// Notional value of the position.
    pub position_notional: Decimal,
    /// Direction of the position.
    pub direction: Direction,
}

/// Returns the SFD charge of one hour, rounded toward zero to a whole number.
///
/// Returns zero when `spot_price` is not positive or the daily rate is zero. Otherwise the result is positive for a long position when `fx_price` is above `spot_price` and for a short position when it is below, and negative in the other cases.
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
