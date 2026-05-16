//! exchange-agnostic な維持率計算。pure 関数で IO 依存なし。
//!
//! 維持率 = 純資産 / 必要証拠金合計
//!     純資産 = 現金残高(free cash) + ロック中証拠金合計 + 評価損益合計
//!           = `current_balance` + Σ`required_margin` + Σ`unrealized_pnl`
//!
//! `trading_accounts.current_balance` は **margin lock 後** の free cash で、
//! lock 額 (= 必要証拠金) を差し引いた値になっている。ナイーブに
//! `current_balance / required` で割ると open 直後 (unrealized=0) でも
//! 維持率が `(initial - required) / required` まで低下して誤発火する。
//! 純資産計算では `+ Σrequired_margin` で lock 分を戻し、initial_balance
//! ベースの正しい equity に揃える。
//!
//! `Trader::close_position` の force-close 判定で使う。live exchange の
//! ロスカット式と同じ。

use crate::types::Direction;
use rust_decimal::{Decimal, RoundingStrategy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenPosition {
    pub direction: Direction,
    pub entry_price: Decimal,
    pub current_price: Decimal,
    pub quantity: Decimal,
    pub leverage: Decimal,
}

impl OpenPosition {
    /// 評価損益 = (current - entry) * qty for Long, (entry - current) * qty for Short
    pub fn unrealized_pnl(&self) -> Decimal {
        let diff = match self.direction {
            Direction::Long => self.current_price - self.entry_price,
            Direction::Short => self.entry_price - self.current_price,
        };
        diff * self.quantity
    }

    /// 必要証拠金 = `truncate_yen(entry_price × quantity / leverage)`。
    ///
    /// `Trader::execute` / `lock_margin` / `release_margin` は yen 整数化
    /// (`round_dp_with_strategy(0, ToZero)`) で ledger に書く。維持率の
    /// denominator を ledger と揃えないと、threshold 境界で
    /// `required_margin(unrounded) / required_margin(rounded)` 分だけ
    /// ratio がズレて誤発火 / 漏れの可能性。同じ rounding をここでも適用。
    pub fn required_margin(&self) -> Decimal {
        (self.entry_price * self.quantity / self.leverage)
            .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    }
}

/// 維持率 = 純資産 / 必要証拠金合計。
///
/// 純資産 = `current_balance` (free cash, margin lock 後)
///         + Σ`required_margin` (lock 中証拠金を戻す)
///         + Σ`unrealized_pnl`
///
/// 必要証拠金合計が 0 (open position 無し) のとき `None` を返す。
/// 純資産が負になっても比率はそのまま負を返す (caller 側で
/// `< threshold` 比較に使える)。
pub fn compute_maintenance_ratio(
    current_balance: Decimal,
    positions: &[OpenPosition],
) -> Option<Decimal> {
    let total_required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
    if total_required.is_zero() {
        return None;
    }
    let total_unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
    // current_balance は margin lock 後の free cash。lock 中の証拠金合計
    // (== total_required) を加算して initial-balance ベースの equity に戻す。
    let equity = current_balance + total_required + total_unrealized;
    Some(equity / total_required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn long_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
        OpenPosition {
            direction: Direction::Long,
            entry_price: entry,
            current_price: current,
            quantity: qty,
            leverage: lev,
        }
    }

    fn short_at(entry: Decimal, current: Decimal, qty: Decimal, lev: Decimal) -> OpenPosition {
        OpenPosition {
            direction: Direction::Short,
            entry_price: entry,
            current_price: current,
            quantity: qty,
            leverage: lev,
        }
    }

    #[test]
    fn compute_maintenance_ratio_long_in_profit() {
        // 引数の current_balance は free cash (lock 後)。
        // balance=40k (= 100k initial - 60k lock), entry=150, current=151, qty=10000, lev=25
        // required = 150*10000/25 = 60000
        // unrealized = (151-150)*10000 = 10000
        // equity = 40k + 60k(lock 戻し) + 10k = 110k
        // ratio = 110k/60k ≈ 1.8333
        let pos = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(110000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_short_in_loss() {
        // balance=40k (free cash), entry=150, current=152, qty=10000, lev=25, Short
        // required = 60000, unrealized = (150-152)*10000 = -20000
        // equity = 40k + 60k(lock 戻し) - 20k = 80k
        // ratio = 80k/60k ≈ 1.333
        let pos = short_at(dec!(150), dec!(152), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(80000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_open_immediately_after_fill() {
        // 重要: open 直後 (unrealized=0) は理屈上「initial / required」になる
        // べき (lock 額分を戻すので)。balance=40k (= 100k - 60k lock),
        // required=60k, unrealized=0 → equity = 100k → ratio = 100k/60k ≈ 1.667。
        // この test は Copilot round-1 で指摘された誤発火 bug の regression guard。
        let pos = long_at(dec!(150), dec!(150), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(40000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(100000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_multiple_positions_sum() {
        // 2 longs に分けて合計が単一ケースと一致することを確認
        let p1 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
        let p2 = long_at(dec!(150), dec!(151), dec!(5000), dec!(25));
        let ratio_split = compute_maintenance_ratio(dec!(100000), &[p1, p2]).unwrap();
        let p_combined = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
        let ratio_single = compute_maintenance_ratio(dec!(100000), &[p_combined]).unwrap();
        assert_eq!(ratio_split, ratio_single);
    }

    #[test]
    fn compute_maintenance_ratio_zero_required_returns_none() {
        // 空 vec → required=0 → None
        assert!(compute_maintenance_ratio(dec!(100000), &[]).is_none());
    }

    #[test]
    fn required_margin_is_truncated_to_yen_to_match_ledger() {
        // entry=150.123, qty=10000, lev=25 → 150.123*10000/25 = 60049.2
        // ledger 側は `truncate_yen` で 60049 を lock。維持率の denominator
        // も同じ 60049 でなければズレる (Copilot round-3 指摘の regression
        // guard)。
        let pos = long_at(dec!(150.123), dec!(150.123), dec!(10000), dec!(25));
        assert_eq!(pos.required_margin(), dec!(60049));
    }

    #[test]
    fn compute_maintenance_ratio_negative_equity_returns_negative_ratio() {
        // balance=0 (lock で free cash 使い切り想定), 巨大な逆行で equity < 0、ratio も負。
        // entry=150, current=400 → unrealized = (150-400)*10000 = -2,500,000
        // equity = 0 + 60k(lock 戻し) - 2.5M = -2.44M → ratio < 0
        let pos = short_at(dec!(150), dec!(400), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(0), &[pos]).unwrap();
        assert!(ratio.is_sign_negative(), "ratio={ratio}, expected negative");
    }
}
