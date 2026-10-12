use rust_decimal::Decimal;

use crate::types::Exchange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Close,
}

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
