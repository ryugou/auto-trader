//! Test helpers for the broker-aware sizing invariants introduced by PR #80.
//!
//! Centralises the formula `max_alloc = 1 / (Y + leverage × stop_loss_pct)`
//! so flow tests can assert numeric expectations without re-deriving.

use auto_trader_core::types::{Direction, Trade};
use rust_decimal::{Decimal, RoundingStrategy};

/// `max_alloc = 1 / (Y + L × s)`
pub fn compute_max_alloc(
    leverage: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
) -> Decimal {
    Decimal::ONE / (liquidation_margin_level + leverage * stop_loss_pct)
}

/// Mirror of `PositionSizer::calculate_quantity` (without min_lot truncation).
/// Returns the raw quantity before truncation.
pub fn compute_raw_quantity(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
) -> Decimal {
    let max_alloc = compute_max_alloc(leverage, stop_loss_pct, liquidation_margin_level);
    let risk_alloc = max_alloc.min(allocation_pct);
    balance * leverage * risk_alloc / entry_price
}

/// Apply min_lot truncation (floor to multiple of `min_lot`).
pub fn truncate_to_min_lot(raw_qty: Decimal, min_lot: Decimal) -> Decimal {
    if min_lot <= Decimal::ZERO {
        return raw_qty;
    }
    (raw_qty / min_lot).floor() * min_lot
}

/// Full quantity computation matching `PositionSizer::calculate_quantity`.
#[allow(clippy::too_many_arguments)]
pub fn expected_quantity(
    balance: Decimal,
    leverage: Decimal,
    allocation_pct: Decimal,
    stop_loss_pct: Decimal,
    liquidation_margin_level: Decimal,
    entry_price: Decimal,
    min_lot: Decimal,
) -> Decimal {
    let raw = compute_raw_quantity(
        balance,
        leverage,
        allocation_pct,
        stop_loss_pct,
        liquidation_margin_level,
        entry_price,
    );
    truncate_to_min_lot(raw, min_lot)
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
pub fn expected_margin_lock(
    quantity: Decimal,
    entry_price: Decimal,
    leverage: Decimal,
) -> Decimal {
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
            "expected_margin_used must be positive, got {margin_used} \
             (qty={qty}, entry={entry}, leverage={leverage})"
        );
    }

    let margin_level = equity_at_sl / margin_used;
    let epsilon = rust_decimal_macros::dec!(0.001);
    assert!(
        margin_level >= liquidation_margin_level - epsilon,
        "post-SL margin level {margin_level} must be >= Y={liquidation_margin_level} \
         (trade {}, qty={qty}, entry={entry}, sl={sl_price}, equity_at_sl={equity_at_sl}, \
         margin_used={margin_used})",
        trade.id
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Exchange, Pair, TradeStatus};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    /// bitflyer_cfd: balance=30,000 JPY, lev=2, SL=2%, Y=0.5, BTC≈12.5M, min_lot=0.001.
    /// max_alloc = 1/(0.5+0.04)=1.85 → caps at allocation_pct=1.0.
    /// raw = 30,000 × 2 × 1.0 / 12,500,000 = 0.0048 → floor to 0.004.
    #[test]
    fn expected_quantity_matches_bitflyer_cfd_sizer_case() {
        let qty = expected_quantity(
            dec!(30000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.50),
            dec!(12500000),
            dec!(0.001),
        );
        assert_eq!(qty, dec!(0.004));
    }

    /// gmo_fx: balance=30,000 JPY, lev=10, SL=2%, Y=1.0, USD/JPY=157, min_lot=1.
    /// max_alloc = 1/(1.0+0.2) ≈ 0.8333 (caps below alloc=1.0).
    /// raw = 30,000 × 10 × 0.8333... / 157 = 1592.356... → floor to 1592.
    #[test]
    fn expected_quantity_matches_gmo_fx_sizer_case() {
        let qty = expected_quantity(
            dec!(30000),
            dec!(10),
            dec!(1.0),
            dec!(0.02),
            dec!(1.00),
            dec!(157),
            dec!(1),
        );
        assert_eq!(qty, dec!(1592));
    }

    #[test]
    fn expected_stop_loss_price_long_subtracts() {
        let sl = expected_stop_loss_price(dec!(100), Direction::Long, dec!(0.02));
        assert_eq!(sl, dec!(98.00));
    }

    #[test]
    fn expected_stop_loss_price_short_adds() {
        let sl = expected_stop_loss_price(dec!(100), Direction::Short, dec!(0.02));
        assert_eq!(sl, dec!(102.00));
    }

    #[test]
    fn expected_take_profit_price_long_adds() {
        let tp = expected_take_profit_price(dec!(100), Direction::Long, dec!(0.04));
        assert_eq!(tp, dec!(104.00));
    }

    #[test]
    fn expected_take_profit_price_short_subtracts() {
        let tp = expected_take_profit_price(dec!(100), Direction::Short, dec!(0.04));
        assert_eq!(tp, dec!(96.00));
    }

    #[test]
    fn expected_pnl_long_winning_is_positive() {
        let pnl = expected_pnl(dec!(100), dec!(110), dec!(2), Direction::Long);
        assert_eq!(pnl, dec!(20));
    }

    #[test]
    fn expected_pnl_long_losing_is_negative() {
        let pnl = expected_pnl(dec!(100), dec!(95), dec!(2), Direction::Long);
        assert_eq!(pnl, dec!(-10));
    }

    #[test]
    fn expected_pnl_short_winning_is_positive() {
        let pnl = expected_pnl(dec!(100), dec!(90), dec!(2), Direction::Short);
        assert_eq!(pnl, dec!(20));
    }

    #[test]
    fn expected_pnl_short_losing_is_negative() {
        let pnl = expected_pnl(dec!(100), dec!(105), dec!(2), Direction::Short);
        assert_eq!(pnl, dec!(-10));
    }

    fn make_trade(
        direction: Direction,
        entry: Decimal,
        sl: Decimal,
        qty: Decimal,
        leverage: Decimal,
    ) -> Trade {
        Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "t".to_string(),
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::GmoFx,
            direction,
            entry_price: entry,
            exit_price: None,
            stop_loss: sl,
            take_profit: None,
            quantity: qty,
            leverage,
            fees: Decimal::ZERO,
            entry_at: Utc::now(),
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
        }
    }

    /// At the exact-cap allocation, post-SL margin level lands at Y (within
    /// the 1bp epsilon allowance) — assertion must pass.
    ///
    /// gmo_fx (Y=1.0) lev=10, SL=2%, balance=30_000:
    ///   max_alloc=1/(1.0+0.2)=0.8333..., qty = 30_000×10×0.8333.../157 ≈ 1592.356
    ///   At sl_price=157×0.98=153.86: pnl=(153.86-157)×1592.356 = -5,000 (≈ exact)
    ///   margin_used = 1592.356×157/10 = 25,000  → margin_level = (30_000 - 5_000)/25_000 = 1.0.
    #[test]
    fn assert_post_sl_passes_at_exact_cap() {
        let balance = dec!(30000);
        let leverage = dec!(10);
        let sl_pct = dec!(0.02);
        let y = dec!(1.0);
        let entry = dec!(157);
        let raw_qty =
            compute_raw_quantity(balance, leverage, dec!(1.0), sl_pct, y, entry);
        let sl = expected_stop_loss_price(entry, Direction::Long, sl_pct);
        let trade = make_trade(Direction::Long, entry, sl, raw_qty, leverage);
        assert_post_sl_margin_level_at_least_y(&trade, balance, y);
    }

    /// Over-leveraged trade (qty constructed *outside* the broker cap) violates
    /// the invariant — assertion must panic.
    #[test]
    #[should_panic(expected = "post-SL margin level")]
    fn assert_post_sl_panics_on_over_alloc() {
        let balance = dec!(30000);
        let leverage = dec!(10);
        let entry = dec!(157);
        let sl = expected_stop_loss_price(entry, Direction::Long, dec!(0.05));
        // qty = 2000 USD ≫ raw_qty for 0.5% SL at this balance/leverage
        let trade = make_trade(Direction::Long, entry, sl, dec!(2000), leverage);
        // Y=1.0 cannot be satisfied with this qty — must panic.
        assert_post_sl_margin_level_at_least_y(&trade, balance, dec!(1.0));
    }
}
