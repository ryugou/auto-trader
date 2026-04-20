use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Position sizer: converts Signal.allocation_pct → concrete quantity.
///
/// This sizer enforces a TARGET_RISK_PCT target (2% account risk per trade)
/// by computing risk-adjusted allocation based on stop-loss and leverage.
/// Strategies provide stop_loss_pct and an aggressiveness cap (allocation_pct),
/// and the sizer computes the actual allocation as:
///   risk_alloc = min(TARGET_RISK_PCT / (stop_loss_pct × leverage), allocation_pct)
///
/// This ensures that actual account risk = balance × leverage × risk_alloc × stop_loss_pct
/// is always ≤ balance × TARGET_RISK_PCT, regardless of leverage.
///
/// The sizer then applies leverage, divides by price, and rounds to min_lot.
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
    ///
    /// `stop_loss_pct` is the stop-loss distance as a fraction of fill price
    /// (e.g., 0.005 = 0.5% distance).
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        leverage: Decimal,
        allocation_pct: Decimal,
        stop_loss_pct: Decimal,
    ) -> Option<Decimal> {
        // 2% actual account risk per trade
        let target_risk_pct = Decimal::new(2, 2);

        if balance <= Decimal::ZERO
            || entry_price <= Decimal::ZERO
            || leverage <= Decimal::ZERO
            || allocation_pct <= Decimal::ZERO
            || allocation_pct > Decimal::ONE
            || stop_loss_pct <= Decimal::ZERO
        {
            return None;
        }

        // Risk-adjusted allocation: ensure actual risk ≤ target_risk_pct
        // Actual risk at SL hit = balance × leverage × risk_alloc × stop_loss_pct
        // We want: balance × leverage × risk_alloc × stop_loss_pct ≤ balance × target_risk_pct
        // So: risk_alloc ≤ target_risk_pct / (leverage × stop_loss_pct)
        // But also cap by the strategy's aggressiveness (allocation_pct)
        let risk_alloc = (target_risk_pct / (leverage * stop_loss_pct)).min(allocation_pct);

        // Mechanical sizing: apply leverage and risk-adjusted allocation, divide by price.
        let raw_qty = balance * leverage * risk_alloc / entry_price;

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
    fn full_allocation_with_risk_limiting() {
        // 100k balance × 2x lev × SL=1% → risk_alloc = min(2% / (2×1%), 1.0) = 1.0
        // 100k × 2 × 1.0 / 10M = 0.02 BTC
        // Actual risk = 100k × 2 × 1.0 × 1% = 2k (2% of balance)
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(10000000),
            dec!(2),
            dec!(1.0),
            dec!(0.01),
        );
        assert_eq!(qty, Some(dec!(0.02)));
    }

    #[test]
    fn risk_adjustment_caps_high_leverage() {
        // 100k balance × 10x lev × SL=2% → risk_alloc = min(2% / (10×2%), 1.0) = min(0.1, 1.0) = 0.1
        // 100k × 10 × 0.1 / 10M = 0.01 BTC
        // Actual risk = 100k × 10 × 0.1 × 2% = 2k (2% of balance)
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(10000000),
            dec!(10),
            dec!(1.0),
            dec!(0.02),
        );
        assert_eq!(qty, Some(dec!(0.01)));
    }

    #[test]
    fn half_allocation_with_moderate_leverage() {
        // 100k × 2x × SL=1% → risk_alloc = min(2% / (2×1%), 0.5) = min(1.0, 0.5) = 0.5
        // 100k × 2 × 0.5 / 10M = 0.01 BTC
        // Actual risk = 100k × 2 × 0.5 × 1% = 1k (1% of balance)
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(10000000),
            dec!(2),
            dec!(0.5),
            dec!(0.01),
        );
        assert_eq!(qty, Some(dec!(0.01)));
    }

    #[test]
    fn truncates_to_min_lot_multiple() {
        // 30k × 2x × SL=2% → risk_alloc = min(2% / (2×2%), 0.9) = min(0.5, 0.9) = 0.5
        // 30k × 2 × 0.5 / 11M ≈ 0.00272 → truncated to 0.002 BTC
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(11000000),
            dec!(2),
            dec!(0.9),
            dec!(0.02),
        );
        assert_eq!(qty, Some(dec!(0.002)));
    }

    #[test]
    fn rejects_when_account_too_small_for_one_min_lot() {
        // 5k × 2x × SL=2% → risk_alloc = min(2% / (2×2%), 0.9) = 0.5
        // 5k × 2 × 0.5 / 11M ≈ 0.000454 → below 0.001 min lot → reject
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(5000),
            dec!(11000000),
            dec!(2),
            dec!(0.9),
            dec!(0.02),
        );
        assert_eq!(qty, None);
    }

    #[test]
    fn rejects_zero_or_negative_inputs() {
        let s = btc_sizer();
        let p = Pair::new("FX_BTC_JPY");
        // zero balance
        assert_eq!(
            s.calculate_quantity(&p, dec!(0), dec!(10000000), dec!(2), dec!(0.5), dec!(0.01)),
            None
        );
        // zero price
        assert_eq!(
            s.calculate_quantity(&p, dec!(100000), dec!(0), dec!(2), dec!(0.5), dec!(0.01)),
            None
        );
        // zero leverage
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(100000),
                dec!(10000000),
                dec!(0),
                dec!(0.5),
                dec!(0.01)
            ),
            None
        );
        // zero allocation
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(100000),
                dec!(10000000),
                dec!(2),
                dec!(0),
                dec!(0.01)
            ),
            None
        );
        // > 100% allocation rejected (it's a bug — caller should clamp)
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(100000),
                dec!(10000000),
                dec!(2),
                dec!(1.5),
                dec!(0.01)
            ),
            None
        );
        // zero stop loss
        assert_eq!(
            s.calculate_quantity(
                &p,
                dec!(100000),
                dec!(10000000),
                dec!(2),
                dec!(0.5),
                dec!(0)
            ),
            None
        );
    }

    #[test]
    fn the_30k_donchian_case_with_proper_risk_limiting() {
        // The original bug: 30k account, BTC ~11M, donchian fires, SL=2%
        // With 100% allocation cap and 2x leverage:
        //   risk_alloc = min(2% / (2×2%), 1.0) = min(0.5, 1.0) = 0.5
        //   qty = 30000 × 2 × 0.5 / 11042347 ≈ 0.002718
        //   truncated to 0.002 BTC
        // Actual risk = 30k × 2 × 0.5 × 2% = 600 JPY (2% of balance)
        let qty = btc_sizer().calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(30000),
            dec!(11042347),
            dec!(2),
            dec!(1.0),
            dec!(0.02),
        );
        assert_eq!(qty, Some(dec!(0.002)));
    }
}
