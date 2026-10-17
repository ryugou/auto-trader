use rust_decimal::Decimal;

use crate::types::Exchange;

/// Kind of order whose commission is estimated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The order opens a position.
    Open,
    /// The order closes a position.
    Close,
}

/// Returns the estimated commission of a fill: zero for every exchange.
///
/// The action, the fill price and the quantity do not change the result.
pub fn estimate(
    _action: Action,
    exchange: Exchange,
    _fill_price: Decimal,
    _qty: Decimal,
) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

#[cfg(test)]
mod tests;
