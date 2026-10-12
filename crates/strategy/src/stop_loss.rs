use rust_decimal::Decimal;

pub(crate) fn lacks_tradable_volatility(stop_loss_pct: Decimal) -> bool {
    stop_loss_pct <= Decimal::ZERO
}
