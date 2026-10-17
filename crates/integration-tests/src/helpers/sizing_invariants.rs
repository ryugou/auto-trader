use auto_trader_core::types::{Direction, Trade};
use rust_decimal::{Decimal, RoundingStrategy};

fn validate_inputs_like_position_sizer(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
) -> Option<()> {
    if balance <= Decimal::ZERO
        || entry_price <= Decimal::ZERO
        || leverage <= Decimal::ZERO
        || allocation_pct <= Decimal::ZERO
        || allocation_pct > Decimal::ONE
        || stop_loss_pct <= Decimal::ZERO
        || liquidation_margin_level <= Decimal::ZERO
    {
        return None;
    }
    Some(())
}

/// Returns the largest allocation, `1 / (Y + L × s)` for the liquidation margin level `Y`, the leverage `L` and the stop-loss percentage `s`, or `None` if any input is not positive.
pub fn compute_max_alloc(
    leverage: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
) -> Option<Decimal> {
    if leverage <= Decimal::ZERO
        || stop_loss_pct <= Decimal::ZERO
        || liquidation_margin_level <= Decimal::ZERO
    {
        return None;
    }
    Some(Decimal::ONE / (liquidation_margin_level + leverage * stop_loss_pct))
}

/// Returns the quantity that `PositionSizer::calculate_quantity` sizes before it truncates to the minimum lot, or `None` if any input is not positive or `allocation_pct` is above 1.
pub fn compute_raw_quantity(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
) -> Option<Decimal> {
    validate_inputs_like_position_sizer(
        balance,
        leverage,
        allocation_pct,
        stop_loss_pct,
        liquidation_margin_level,
        entry_price,
    )?;
    let max_alloc = compute_max_alloc(leverage, stop_loss_pct, liquidation_margin_level)?;
    let risk_alloc = max_alloc.min(allocation_pct);
    Some(balance * leverage * risk_alloc / entry_price)
}

/// Returns `raw_qty` rounded down to a multiple of `min_lot`, or `raw_qty` itself if `min_lot` is not positive.
pub fn truncate_to_min_lot(raw_qty: Decimal, min_lot: Decimal) -> Decimal {
    if min_lot <= Decimal::ZERO {
        return raw_qty;
    }
    (raw_qty / min_lot).floor() * min_lot
}

/// Returns the quantity that `PositionSizer::calculate_quantity` returns, or `None` if the position cannot be sized.
///
/// The position cannot be sized when an input is invalid (see `compute_raw_quantity`), when the quantity truncated to `min_lot` is below `min_lot`, or when `min_lot` is not positive and the raw quantity is not positive.
#[allow(clippy::too_many_arguments)]
pub fn expected_quantity(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
    min_lot: Decimal,
) -> Option<Decimal> {
    let raw = compute_raw_quantity(
        balance,
        leverage,
        allocation_pct,
        stop_loss_pct,
        liquidation_margin_level,
        entry_price,
    )?;
    if min_lot > Decimal::ZERO {
        let truncated = truncate_to_min_lot(raw, min_lot);
        if truncated < min_lot {
            return None;
        }
        Some(truncated)
    } else if raw > Decimal::ZERO {
        Some(raw)
    } else {
        None
    }
}

/// Returns the stop-loss price: `entry × (1 - sl_pct)` for a long position and `entry × (1 + sl_pct)` for a short position.
pub fn expected_stop_loss_price(
    entry_price: Decimal,
    direction: Direction,
    stop_loss_pct: Decimal,
) -> Decimal {
    match direction {
        Direction::Long => entry_price * (Decimal::ONE - stop_loss_pct),
        Direction::Short => entry_price * (Decimal::ONE + stop_loss_pct),
    }
}

/// Returns the take-profit price: `entry × (1 + tp_pct)` for a long position and `entry × (1 - tp_pct)` for a short position.
pub fn expected_take_profit_price(
    entry_price: Decimal,
    direction: Direction,
    take_profit_pct: Decimal,
) -> Decimal {
    match direction {
        Direction::Long => entry_price * (Decimal::ONE + take_profit_pct),
        Direction::Short => entry_price * (Decimal::ONE - take_profit_pct),
    }
}

/// Returns the profit and loss: `(exit - entry) × qty` for a long position and `(entry - exit) × qty` for a short position.
///
/// The leverage is not applied, because the quantity already includes it.
pub fn expected_pnl(
    entry_price: Decimal,
    exit_price: Decimal,
    quantity: Decimal,
    direction: Direction,
) -> Decimal {
    let price_diff = match direction {
        Direction::Long => exit_price - entry_price,
        Direction::Short => entry_price - exit_price,
    };
    price_diff * quantity
}

/// Returns the margin to lock, `qty × entry / leverage` truncated toward zero to whole yen.
pub fn expected_margin_lock(quantity: Decimal, entry_price: Decimal, leverage: Decimal) -> Decimal {
    (quantity * entry_price / leverage).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

/// Asserts that the margin level of the trade after its stop loss is hit is at least `liquidation_margin_level` minus 0.001.
///
/// The margin level is the balance at open plus the profit and loss at the stop-loss price, divided by the margin used, which is `qty × entry / leverage`.
///
/// # Panics
///
/// - Panics if the margin used is not positive.
/// - Panics if the margin level is below `liquidation_margin_level` minus 0.001.
pub fn assert_post_sl_margin_level_at_least_y(
    trade: &Trade,
    balance_at_open: Decimal,
    liquidation_margin_level: Decimal,
) {
    let qty = trade.quantity;
    let entry = trade.entry_price;
    let sl_price = trade.stop_loss;
    let leverage = trade.leverage;

    let pnl_at_sl = match trade.direction {
        Direction::Long => (sl_price - entry) * qty,
        Direction::Short => (entry - sl_price) * qty,
    };
    let equity_at_sl = balance_at_open + pnl_at_sl;
    let margin_used = qty * entry / leverage;

    if margin_used <= Decimal::ZERO {
        panic!(
            "invalid margin used: must be positive, got {margin_used} \
             (qty={qty}, entry={entry}, leverage={leverage})"
        );
    }

    let margin_level = equity_at_sl / margin_used;
    let epsilon = rust_decimal_macros::dec!(0.001);
    assert!(
        margin_level >= liquidation_margin_level - epsilon,
        "invalid post-SL margin level: {margin_level} is below Y={liquidation_margin_level} \
         (trade {}, qty={qty}, entry={entry}, sl={sl_price}, equity_at_sl={equity_at_sl}, \
         margin_used={margin_used})",
        trade.id
    );
}

#[cfg(test)]
mod tests;
