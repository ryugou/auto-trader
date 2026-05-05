use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Position sizer: converts Signal.allocation_pct → concrete quantity.
///
/// Sizing strategy: **invest the maximum amount that keeps the post-SL
/// margin level at or above the broker's liquidation threshold**.
///
///   max_alloc = 1 / (Y + leverage × stop_loss_pct)
///   risk_alloc = min(max_alloc, allocation_pct)
///
/// `Y` is the broker's liquidation margin level threshold supplied by the
/// caller (resolved per-exchange from `[exchange_margin.<name>]` config).
/// At `risk_alloc = max_alloc`, an idealised fill at the SL price closes the
/// position with margin level exactly Y. Real-world slippage, weekend gaps,
/// or SL-trigger latency can push the realised loss past the SL price and
/// drop margin level below Y; this sizer does not include a buffer for those
/// cases. If post-SL slippage tolerance is required, callers should size
/// against a tighter Y (or a separate buffered threshold).
///
/// Example: bitflyer_cfd (Y=0.5, lev=2), SL=2%
///   max_alloc = 1 / (0.5 + 0.04) = 1.85 → capped at allocation_pct (1.0)
///
/// Example: gmo_fx (Y=1.0, lev=10), SL=2%
///   max_alloc = 1 / (1.0 + 0.2) = 0.833 → 83.3% of balance as margin
pub struct PositionSizer {
    min_order_sizes: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(min_order_sizes: HashMap<Pair, Decimal>) -> Self {
        Self { min_order_sizes }
    }

    /// Compute the trade quantity. Returns None when the result would
    /// be below the per-pair `min_order_size`, when any input is non-positive,
    /// or when `liquidation_margin_level` is non-positive (configuration bug).
    ///
    /// `allocation_pct` must be in (0, 1]. Values outside that range
    /// are treated as bugs and rejected (returns None) — the sizer
    /// does not silently clamp.
    ///
    /// `allocation_pct` is the **fraction of leveraged capacity** to deploy,
    /// i.e. (margin used / equity), so the resulting trade notional is
    /// `balance × leverage × allocation_pct`. Setting `allocation_pct = 1.0`
    /// requests "use my full balance as margin" (subject to the post-SL
    /// liquidation cap below).
    ///
    /// `stop_loss_pct` is the stop-loss distance as a fraction of fill price
    /// (e.g., 0.005 = 0.5% distance).
    ///
    /// `liquidation_margin_level` is the broker's margin-call threshold as
    /// a decimal (e.g., 0.50 for bitFlyer Crypto CFD's 50%, 1.00 for GMO
    /// 外国為替FX's 100%).
    #[allow(clippy::too_many_arguments)]
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        leverage: Decimal,
        allocation_pct: Decimal,
        stop_loss_pct: Decimal,
        liquidation_margin_level: Decimal,
    ) -> Option<Decimal> {
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

        // SL ヒット時の維持率 = (1 - L × a × s) / a ≥ Y を解いて
        //   a ≤ 1 / (Y + L × s)
        let max_alloc = Decimal::ONE / (liquidation_margin_level + leverage * stop_loss_pct);
        let risk_alloc = max_alloc.min(allocation_pct);

        // Mechanical sizing: apply leverage and risk-adjusted allocation, divide by price.
        let raw_qty = balance * leverage * risk_alloc / entry_price;

        let min_size = self
            .min_order_sizes
            .get(pair)
            .copied()
            .unwrap_or(Decimal::ZERO);

        if min_size > Decimal::ZERO {
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

    fn fx_sizer() -> PositionSizer {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("USD_JPY"), dec!(1));
        PositionSizer::new(min_sizes)
    }

    /// gmo_fx (Y=1.0) lev=10, SL=2%, balance=30,000円: max_alloc = 1/(1.0+0.2) = 0.8333...
    /// position_value = 30,000 × 10 / 1.2 / 157 = 250,000 / 157 = 1592.356... → 1592 (min_lot=1).
    #[test]
    fn gmo_fx_loose_sl_caps_at_alloc_below_one() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.02),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(1592)));
    }

    /// gmo_fx (Y=1.0) lev=10, SL=0.5%: max_alloc = 1/(1.0+0.05) ≈ 0.952.
    /// 30,000 × 10 × 0.952 / 157 = 1819 USD → 1819.
    #[test]
    fn gmo_fx_tight_sl_higher_allocation() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.005),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(1819)));
    }

    /// bitflyer_cfd (Y=0.5) lev=2, SL=2%: max_alloc = 1/(0.5+0.04) ≈ 1.85 → cap at allocation_pct=1.0.
    /// 30,000 × 2 × 1.0 / 12,500,000 = 0.0048 → truncated to 0.004 (multiple of min_lot=0.001).
    #[test]
    fn bitflyer_cfd_typical_sl_uses_full_allocation() {
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(12500000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, Some(dec!(0.004)));
    }

    /// At lev=10, SL=10%, Y=1.0: max_alloc = 1/(1.0+1.0) = 0.5 → forces under-allocation.
    /// 30,000 × 10 × 0.5 / 157 = 955 USD.
    #[test]
    fn lc_constraint_binds_at_high_leverage_and_wide_sl() {
        let qty = fx_sizer().calculate_quantity(
            &Pair::new("USD_JPY"),
            dec!(30000),
            dec!(157),
            dec!(10),
            dec!(1.0),
            dec!(0.10),
            dec!(1.00),
        );
        assert_eq!(qty, Some(dec!(955)));
    }

    /// Caller-supplied allocation_pct dominates when smaller than the LC cap.
    /// bitflyer_cfd, alloc=0.5 → 30,000 × 2 × 0.5 / 12,500,000 = 0.0024 → 0.002.
    #[test]
    fn allocation_pct_dominates_when_smaller_than_max_alloc() {
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(12500000),
            dec!(2),
            dec!(0.5),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, Some(dec!(0.002)));
    }

    /// liquidation_margin_level <= 0 is treated as a configuration bug.
    #[test]
    fn rejects_zero_or_negative_liquidation_margin_level() {
        let s = fx_sizer();
        let p = Pair::new("USD_JPY");
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(0)
            ),
            None
        );
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(-0.5)
            ),
            None
        );
    }

    /// Existing input validations preserved.
    #[test]
    fn rejects_zero_or_negative_inputs() {
        let s = fx_sizer();
        let p = Pair::new("USD_JPY");
        // zero balance
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(0),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00)
            ),
            None
        );
        // zero price
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(0),
                dec!(10),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00)
            ),
            None
        );
        // zero leverage
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(0),
                dec!(1.0),
                dec!(0.02),
                dec!(1.00)
            ),
            None
        );
        // zero allocation
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(0),
                dec!(0.02),
                dec!(1.00)
            ),
            None
        );
        // > 100% allocation rejected
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.5),
                dec!(0.02),
                dec!(1.00)
            ),
            None
        );
        // zero stop loss
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(30000),
                dec!(157),
                dec!(10),
                dec!(1.0),
                dec!(0),
                dec!(1.00)
            ),
            None
        );
    }

    /// Truncation to min_lot still rejects when result < one lot.
    #[test]
    fn rejects_when_account_too_small_for_one_min_lot() {
        // bitflyer_cfd, balance=5,000円 with BTC ~12.5M and lev=2: full alloc
        // qty = 5000 × 2 × 1.0 / 12,500,000 = 0.0008 → below 0.001 min lot
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(5000),
            dec!(12500000),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
            dec!(0.50),
        );
        assert_eq!(qty, None);
    }

    /// Property: applying max_alloc places the post-SL margin level at exactly Y
    /// when LC is the binding constraint, or strictly above Y when the
    /// allocation_pct cap (a = 1.0) is binding (so liquidation is not at risk).
    ///   margin_level = (1 - L × a × s) / a; with a = 1 / (Y + L × s), this equals Y.
    ///   With a = 1.0 instead, margin_level = 1 - L × s, which must be ≥ Y.
    #[test]
    fn post_sl_margin_level_equals_threshold_invariant() {
        let cases = [
            // LC-binding cases: a_unbounded < 1
            (dec!(10), dec!(0.005), dec!(1.00)),
            (dec!(10), dec!(0.02), dec!(1.00)),
            (dec!(10), dec!(0.10), dec!(1.00)),
            (dec!(2), dec!(0.05), dec!(0.50)),
            // Cap-binding case: a_unbounded ≥ 1, so the caller-side cap of 1.0 binds.
            // bitflyer_cfd lev=2, sl=2%, y=0.5 → 1/(0.5+0.04)=1.85 ≥ 1 → cap binds.
            (dec!(2), dec!(0.02), dec!(0.50)),
        ];
        let mut saw_lc_binding = false;
        let mut saw_cap_binding = false;
        for (lev, sl, y) in cases {
            // a = 1 / (Y + L × s), bounded by 1.0 caller-side.
            let a_unbounded = Decimal::ONE / (y + lev * sl);
            if a_unbounded < Decimal::ONE {
                saw_lc_binding = true;
                let ml = (Decimal::ONE - lev * a_unbounded * sl) / a_unbounded;
                let diff = (ml - y).abs();
                assert!(
                    diff < dec!(0.0001),
                    "LC-binding: lev={lev}, sl={sl}, y={y}: margin level {ml} != threshold (diff={diff})"
                );
            } else {
                // Cap-binding branch: a clamps to 1.0; verify post-SL margin
                // level lies at or above Y (with 1 bp slack for Decimal
                // rounding) — i.e. liquidation is comfortably avoided.
                saw_cap_binding = true;
                let a = Decimal::ONE;
                let ml = (Decimal::ONE - lev * a * sl) / a;
                assert!(
                    ml + dec!(0.0001) >= y,
                    "cap-binding: lev={lev}, sl={sl}, y={y}: margin level {ml} < threshold (slack 1bp)"
                );
            }
        }
        assert!(saw_lc_binding, "test must cover the LC-binding branch");
        assert!(saw_cap_binding, "test must cover the cap-binding branch");
    }
}
