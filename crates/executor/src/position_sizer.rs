use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Position sizer with **pure capacity-based** sizing.
///
/// The sizer's only job is "given an account balance and the current
/// price, how many units can we hold?" — it does NOT look at any chart
/// information (stop-loss distance, ATR, indicator state, …). That
/// separation matches the conceptual layering of the system:
///
/// - **Signal layer (chart-only)**: a strategy decides "buy / sell" and
///   what price levels constitute SL / TP.
/// - **Execution layer (balance-only)**: the sizer decides how many
///   units to actually buy.
///
/// Each strategy declares an `allocation_pct` on the Signal it emits
/// to express how aggressively it wants to commit capital per trade
/// (e.g. 0.30 for the conservative strategy, 0.90 for the aggressive
/// one). The sizer multiplies that fraction with the account's capacity:
///
/// ```text
/// quantity = floor(
///     (balance × leverage × allocation_pct / entry_price) / min_lot
/// ) × min_lot
/// ```
///
/// `allocation_pct = 1.0` means "use the entire account's leveraged
/// notional", which is what crypto traders sometimes call full kelly.
/// `allocation_pct < 1.0` always leaves headroom for adverse price moves
/// and for subsequent trades.
pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>) -> Self {
        Self { min_order_sizes }
    }

    /// Compute the trade quantity. Returns None when the result would
    /// be below the per-pair `min_order_size` — the account is
    /// structurally too small to express even one minimum lot at this
    /// price under the requested allocation.
    ///
    /// `allocation_pct` must be in (0, 1]. Values outside that range
    /// are treated as bugs and rejected (returns None) — the sizer
    /// does not silently clamp.
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        leverage: Decimal,
        allocation_pct: Decimal,
    ) -> Option<Decimal> {
        if balance <= Decimal::ZERO
            || entry_price <= Decimal::ZERO
            || leverage <= Decimal::ZERO
            || allocation_pct <= Decimal::ZERO
            || allocation_pct > Decimal::ONE
        {
            return None;
        }

        // Pure capacity formula. No SL distance, no risk_rate, no chart
        // information of any kind. The strategy's chart-derived stop
        // levels live on the Signal and are honored by the position
        // monitor; they are not an input to sizing.
        let raw_qty = balance * leverage * allocation_pct / entry_price;

        let min_size = self
            .min_order_sizes
            .get(pair)
            .copied()
            .unwrap_or(Decimal::ZERO);

        if min_size > Decimal::ZERO {
            // Truncate to a multiple of min_lot so the broker accepts
            // the size. Anything below min_lot means the account is
            // too small to participate at all.
            let truncated = (raw_qty / min_size).floor() * min_size;
            if truncated < min_size {
                return None;
            }
            Some(truncated)
        } else if raw_qty > Decimal::ZERO {
            Some(raw_qty)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Pair;
    use rust_decimal_macros::dec;

    fn btc_sizer() -> PositionSizer {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        PositionSizer::new(min_sizes)
    }

    #[test]
    fn full_allocation_uses_full_leveraged_capacity() {
        // 100k balance × 2x lev × 1.0 allocation / 10M price = 0.02 BTC
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(10000000),
            dec!(2),
            dec!(1.0),
        );
        assert_eq!(qty, Some(dec!(0.02)));
    }

    #[test]
    fn half_allocation_uses_half_capacity() {
        // 100k × 2x × 0.5 / 10M = 0.01 BTC
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(10000000),
            dec!(2),
            dec!(0.5),
        );
        assert_eq!(qty, Some(dec!(0.01)));
    }

    #[test]
    fn truncates_to_min_lot_multiple() {
        // 30k × 2x × 0.9 / 11M ≈ 0.004909 → truncated to 0.004 BTC
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(11000000),
            dec!(2),
            dec!(0.9),
        );
        assert_eq!(qty, Some(dec!(0.004)));
    }

    #[test]
    fn rejects_when_account_too_small_for_one_min_lot() {
        // 5k × 2x × 0.9 / 11M ≈ 0.000818 → below 0.001 min lot → reject
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(5000),
            dec!(11000000),
            dec!(2),
            dec!(0.9),
        );
        assert_eq!(qty, None);
    }

    #[test]
    fn rejects_zero_or_negative_inputs() {
        let s = btc_sizer();
        let p = Pair::new("FX_BTC_JPY");
        // zero balance
        assert_eq!(
            s.calculate_quantity(&p, dec!(0), dec!(10000000), dec!(2), dec!(0.5)),
            None
        );
        // zero price
        assert_eq!(
            s.calculate_quantity(&p, dec!(100000), dec!(0), dec!(2), dec!(0.5)),
            None
        );
        // zero leverage
        assert_eq!(
            s.calculate_quantity(&p, dec!(100000), dec!(10000000), dec!(0), dec!(0.5)),
            None
        );
        // zero allocation
        assert_eq!(
            s.calculate_quantity(&p, dec!(100000), dec!(10000000), dec!(2), dec!(0)),
            None
        );
        // > 100% allocation rejected (it's a bug — caller should clamp)
        assert_eq!(
            s.calculate_quantity(&p, dec!(100000), dec!(10000000), dec!(2), dec!(1.5)),
            None
        );
    }

    #[test]
    fn the_30k_donchian_case_now_succeeds() {
        // The original bug: 30k account, BTC ~11M, donchian fires.
        // With 100% allocation (the current 標準 profile), we get:
        //   30000 × 2 × 1.0 / 11042347 ≈ 0.005434
        //   truncated to 0.005 BTC
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(11042347),
            dec!(2),
            dec!(1.0),
        );
        assert_eq!(qty, Some(dec!(0.005)));
    }
}
