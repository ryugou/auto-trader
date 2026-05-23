//! GMO FX paper account 用 daily swap point 計算 (pure 関数)。
//!
//! paper account は live exchange と違い bot が自分で swap を計上する必要が
//! ある。config の rate table (pair × direction の代表値) を使って 1 日分の
//! swap を計算し、`apply_swap_fee` 経由で `Trade.fees` に積算する。
//!
//! formula:
//!   per_lot = rate.long_per_lot or rate.short_per_lot (direction で分岐)
//!   lots    = quantity / 10_000  (GMO FX 標準 1 lot = 10,000 通貨単位)
//!   fee     = truncate_yen(per_lot × lots)
//!
//! 戻り値の符号は config rate と direction の組み合わせで決まる。`apply_swap_fee`
//! の規約 (>0 = paper 払い、<0 = paper 受取) と整合させるため、config の
//! `long`/`short` 値は **paper が払う方向を正とする** 規約で記入する。
//! (例: USD_JPY で Long が受取の場合 `long = -100` (paper 受取)、
//!      Short が支払いの場合 `short = +120` (paper 支払い))。

use crate::types::{Direction, Exchange};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;

/// paper 側 swap fee の skeleton。現状は全 exchange 0 を返す。
/// 実際の swap 計算は config rate を使う `compute_daily_swap` が行う。
pub fn estimate(exchange: Exchange) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

/// 1 日分の swap fee (signed) を算出。truncate to whole yen。
///
/// 戻り値は `apply_swap_fee` に渡される値で、その符号規約は:
///   >0 → paper account 払い (fees 増、balance 減)
///   <0 → paper account 受取 (fees 減、balance 増)
pub fn compute_daily_swap(
    long_per_lot: Decimal,
    short_per_lot: Decimal,
    direction: Direction,
    quantity: Decimal,
) -> Decimal {
    let per_lot = match direction {
        Direction::Long => long_per_lot,
        Direction::Short => short_per_lot,
    };
    let lots = quantity / dec!(10_000);
    (per_lot * lots).round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_all_exchanges_currently_zero() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(estimate(ex), Decimal::ZERO);
        }
    }

    #[test]
    fn long_positive_rate_pays_fee() {
        // long_per_lot = +100 (paper 払い), 1 lot (10_000) → +100
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, dec!(10_000));
        assert_eq!(fee, dec!(100));
    }

    #[test]
    fn short_negative_rate_receives_fee() {
        // short_per_lot = -120 (paper 受取), 1 lot (10_000) → -120
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Short, dec!(10_000));
        assert_eq!(fee, dec!(-120));
    }

    #[test]
    fn partial_lot_scales_proportionally() {
        // 0.5 lot (5_000 通貨), long_rate = +100 → 50
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, dec!(5_000));
        assert_eq!(fee, dec!(50));
    }

    #[test]
    fn truncates_fractional_yen_to_zero() {
        // 0.33 lot (3_300 通貨), rate = +1 → 0.33 → truncate = 0
        let fee = compute_daily_swap(dec!(1), dec!(-1), Direction::Long, dec!(3_300));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_rate_returns_zero() {
        let fee = compute_daily_swap(dec!(0), dec!(0), Direction::Long, dec!(10_000));
        assert_eq!(fee, Decimal::ZERO);
    }

    #[test]
    fn zero_quantity_returns_zero() {
        let fee = compute_daily_swap(dec!(100), dec!(-120), Direction::Long, Decimal::ZERO);
        assert_eq!(fee, Decimal::ZERO);
    }
}
