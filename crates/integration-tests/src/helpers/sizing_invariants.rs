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

/// `max_alloc = 1 / (Y + L × s)`. Returns `None` for non-positive inputs
/// (matching `PositionSizer::calculate_quantity`'s validation).
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

/// Mirror of `PositionSizer::calculate_quantity` (without min_lot truncation).
/// Returns `None` when any input is non-positive or `allocation_pct > 1`,
/// matching production validation. Otherwise returns the raw quantity.
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

/// Apply min_lot truncation (floor to multiple of `min_lot`).
pub fn truncate_to_min_lot(raw_qty: Decimal, min_lot: Decimal) -> Decimal {
    if min_lot <= Decimal::ZERO {
        return raw_qty;
    }
    (raw_qty / min_lot).floor() * min_lot
}

/// Full quantity computation matching `PositionSizer::calculate_quantity`.
///
/// Returns `None` when the truncated quantity would be below `min_lot` —
/// matching production behavior where the sizer refuses to produce a
/// sub-lot order. Tests that previously got a 0-or-below value silently
/// will now see `None`, forcing them to handle the unsizeable case
/// explicitly (or unwrap if they expect a successful sizing).
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

/// `SL = entry × (1 - sl_pct)` for Long, `entry × (1 + sl_pct)` for Short.
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

/// `TP = entry × (1 + tp_pct)` for Long, `entry × (1 - tp_pct)` for Short.
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

/// `PnL = (exit - entry) × qty` for Long, `(entry - exit) × qty` for Short.
/// Note: leverage is already baked into `qty` by the sizer.
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

/// `margin = qty × entry / leverage`, truncated toward zero to whole yen
/// (matching `Trader::execute`'s `truncate_yen` helper).
pub fn expected_margin_lock(quantity: Decimal, entry_price: Decimal, leverage: Decimal) -> Decimal {
    (quantity * entry_price / leverage).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

/// Assert that the post-SL margin level is at or above broker threshold `Y`.
///
/// Computes:
///   `equity_at_sl     = balance + (sl_price - entry) × qty × direction_sign`
///   `margin_used      = qty × entry / leverage`
///   `margin_level     = equity_at_sl / margin_used`
///
/// Asserts `margin_level >= Y - epsilon` (epsilon for Decimal rounding).
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
