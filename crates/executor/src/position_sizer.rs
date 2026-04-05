use auto_trader_core::types::Pair;
use rust_decimal::Decimal;
use std::collections::HashMap;

pub struct PositionSizer {
    risk_rate: Decimal,
    min_order_sizes: HashMap<Pair, Decimal>,
}

impl PositionSizer {
    pub fn new(risk_rate: Decimal, min_order_sizes: HashMap<Pair, Decimal>) -> Self {
        Self {
            risk_rate,
            min_order_sizes,
        }
    }

    /// Returns the position quantity, or None if the trade should be skipped
    /// (below min order size or exceeds margin).
    pub fn calculate_quantity(
        &self,
        pair: &Pair,
        balance: Decimal,
        entry_price: Decimal,
        stop_loss: Decimal,
        leverage: Decimal,
    ) -> Option<Decimal> {
        let max_loss = balance * self.risk_rate;
        let sl_distance = (entry_price - stop_loss).abs();
        if sl_distance == Decimal::ZERO {
            return None;
        }

        let quantity = max_loss / sl_distance;

        // Check minimum order size
        let min_size = self
            .min_order_sizes
            .get(pair)
            .copied()
            .unwrap_or(Decimal::ZERO);
        if quantity < min_size {
            return None;
        }

        // Check margin requirement
        let margin_required = quantity * entry_price / leverage;
        if margin_required > balance {
            return None;
        }

        // Truncate to min_size precision
        if min_size > Decimal::ZERO {
            let truncated = (quantity / min_size).floor() * min_size;
            if truncated < min_size {
                return None;
            }
            Some(truncated)
        } else {
            Some(quantity)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Pair;
    use rust_decimal_macros::dec;

    #[test]
    fn calculates_quantity_from_risk() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.02), min_sizes);

        // balance=100000, risk_rate=2% → max_loss=2000
        // SL距離=200000円 → quantity = 2000/200000 = 0.01 BTC
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(100000),
            dec!(15000000),
            dec!(14800000),
            dec!(2),
        );
        assert_eq!(qty, Some(dec!(0.01)));
    }

    #[test]
    fn rejects_below_min_order_size() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.02), min_sizes);

        // balance=1000, risk_rate=2% → max_loss=20
        // SL距離=200000 → quantity = 20/200000 = 0.0001 < min 0.001
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(1000),
            dec!(15000000),
            dec!(14800000),
            dec!(2),
        );
        assert_eq!(qty, None);
    }

    #[test]
    fn rejects_exceeds_margin() {
        let mut min_sizes = HashMap::new();
        min_sizes.insert(Pair::new("FX_BTC_JPY"), dec!(0.001));
        let sizer = PositionSizer::new(dec!(0.50), min_sizes); // 50% risk

        // balance=5233, risk_rate=50% → max_loss=2616.5
        // SL距離=100000 → quantity = 2616.5/100000 = 0.026165
        // margin_required = 0.026165 * 15000000 / 2 = 196237.5 > 5233 → reject
        let qty = sizer.calculate_quantity(
            &Pair::new("FX_BTC_JPY"),
            dec!(5233),
            dec!(15000000),
            dec!(14900000),
            dec!(2),
        );
        assert_eq!(qty, None);
    }
}
