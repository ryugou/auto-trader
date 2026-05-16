//! exchange-agnostic な維持率計算。pure 関数で IO 依存なし。
//!
//! 維持率 = (現金残高 + 評価損益合計) / 必要証拠金合計
//!
//! `Trader::close_position` の force-close 判定で使う。live exchange の
//! ロスカット式と同じ。

use crate::types::Direction;
use rust_decimal::Decimal;

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

    /// 必要証拠金 = entry_price × quantity / leverage
    pub fn required_margin(&self) -> Decimal {
        self.entry_price * self.quantity / self.leverage
    }
}

/// 維持率 = (現金残高 + 評価損益合計) / 必要証拠金合計。
///
/// 必要証拠金合計が 0 (open position 無し) のとき `None` を返す。
/// 残高 + 評価損益が負になっても比率はそのまま負を返す (caller 側で
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
    Some((current_balance + total_unrealized) / total_required)
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
        // balance=100k, entry=150, current=151, qty=10000, lev=25
        // required = 150*10000/25 = 60000
        // unrealized = (151-150)*10000 = 10000
        // ratio = (100000+10000)/60000 ≈ 1.8333
        let pos = long_at(dec!(150), dec!(151), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(100000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(110000) / dec!(60000));
    }

    #[test]
    fn compute_maintenance_ratio_short_in_loss() {
        // balance=100k, entry=150, current=152, qty=10000, lev=25, Short
        // required = 60000
        // unrealized = (150-152)*10000 = -20000
        // ratio = (100000-20000)/60000 ≈ 1.333
        let pos = short_at(dec!(150), dec!(152), dec!(10000), dec!(25));
        let ratio = compute_maintenance_ratio(dec!(100000), &[pos]).unwrap();
        assert_eq!(ratio, dec!(80000) / dec!(60000));
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
    fn compute_maintenance_ratio_negative_equity_returns_negative_ratio() {
        // balance=10000, big short loss → equity < 0、ratio も負
        let pos = short_at(dec!(150), dec!(170), dec!(10000), dec!(25));
        // required = 60000, unrealized = (150-170)*10000 = -200000
        // ratio = (10000-200000)/60000 ≈ -3.166
        let ratio = compute_maintenance_ratio(dec!(10000), &[pos]).unwrap();
        assert!(ratio.is_sign_negative());
    }
}
