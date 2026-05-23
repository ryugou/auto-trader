//! GMO FX paper account 用 daily swap point 計算 (pure 関数)。
//!
//! paper account は live exchange と違い bot が自分で swap を計上する必要が
//! ある。config の rate table (pair × direction の代表値) を使って 1 日分の
//! swap を計算し、`apply_swap_fee` 経由で `Trade.fees` に積算する。
//!
//! formula:
//!   per_lot = rate.long or rate.short (direction で分岐)
//!   lots    = quantity / 10_000  (GMO FX 標準 1 lot = 10,000 通貨単位)
//!   fee     = truncate_yen(per_lot × lots)
//!
//! sign 規約 (config rate と戻り値で同じ):
//!
//! - `> 0` → paper account 払い (fees 増、balance 減)
//! - `< 0` → paper account 受取 (fees 減、balance 増)
//!
//! 例: USD_JPY で Long が paper 払い (= interest pays the holder of the
//! lower-yielding currency)、Short が受取の場合:
//!
//! ```toml
//! [gmo_fx.swap.rates]
//! USD_JPY = { long = 100, short = -120 }
//! ```

use crate::config::SwapRateEntry;
use crate::types::Direction;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

/// GMO FX 標準 lot size (10,000 通貨単位 / 1 lot)。
const GMO_FX_LOT_SIZE: Decimal = dec!(10_000);

/// 1 日分の swap fee (signed) を算出。truncate to whole yen。
///
/// 戻り値は `apply_swap_fee` に渡される値で、その符号規約は module docstring
/// と一致 (`>0` = paper 払い、`<0` = paper 受取)。
pub fn compute_daily_swap(rate: SwapRateEntry, direction: Direction, quantity: Decimal) -> Decimal {
    let per_lot = match direction {
        Direction::Long => rate.long,
        Direction::Short => rate.short,
    };
    let lots = quantity / GMO_FX_LOT_SIZE;
    (per_lot * lots).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(long: Decimal, short: Decimal) -> SwapRateEntry {
        SwapRateEntry { long, short }
    }

    #[test]
    fn long_positive_rate_pays_fee() {
        // long = +100 (paper 払い), 1 lot (10_000) → +100
        let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, dec!(10_000));
        assert_eq!(fee, dec!(100));
    }

    #[test]
    fn short_negative_rate_receives_fee() {
        // short = -120 (paper 受取), 1 lot (10_000) → -120
        let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Short, dec!(10_000));
        assert_eq!(fee, dec!(-120));
    }

    #[test]
    fn partial_lot_scales_proportionally() {
        // 0.5 lot (5_000 通貨), long = +100 → 50
        let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, dec!(5_000));
        assert_eq!(fee, dec!(50));
    }

    #[test]
    fn truncates_fractional_yen_to_zero() {
        // 0.33 lot (3_300 通貨), rate = +1 → 0.33 → truncate = 0
        let fee = compute_daily_swap(rate(dec!(1), dec!(-1)), Direction::Long, dec!(3_300));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_rate_returns_zero() {
        let fee = compute_daily_swap(rate(dec!(0), dec!(0)), Direction::Long, dec!(10_000));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_quantity_returns_zero() {
        let fee = compute_daily_swap(rate(dec!(100), dec!(-120)), Direction::Long, Decimal::ZERO);
        assert_eq!(fee, Decimal::ZERO);
    }
}
