use auto_trader_core::types::Direction;
use auto_trader_market::bitflyer_private::Side;

pub(super) fn opening_side(direction: Direction) -> Side {
    match direction {
        Direction::Long => Side::Buy,
        Direction::Short => Side::Sell,
    }
}

pub(super) fn closing_side(direction: Direction) -> Side {
    match direction {
        Direction::Long => Side::Sell,
        Direction::Short => Side::Buy,
    }
}
