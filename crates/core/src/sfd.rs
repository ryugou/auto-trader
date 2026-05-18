//! exchange 別の paper SFD (Swap For Difference) 見積もり pure 関数。
//!
//! 現状は全 exchange で 0 を返す。bitFlyer Crypto CFD は live で課金される
//! が、paper 側で BTC 現物価格 feed を持っていないため正確な計算ができない。
//! 将来 spot feed を追加した時にこのファイルの中身を差し替えるだけで
//! paper/live 両方の Trade.fees が等価のまま追従する設計 (commission モジュール
//! と同じパターン)。
//!
//! Exchange enum を exhaustive match で扱うので、新 variant 追加時はコンパイル
//! エラーで気づける。

use crate::types::{Direction, Exchange};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// paper 側 SFD を見積もる。現状は全 exchange で 0 を返す。
///
/// `_fill_price` / `_qty` は将来 BTC spot との乖離率から SFD を概算する時の
/// 拡張口。今は未使用で warning も抑える。
pub fn estimate(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

/// bitFlyer Crypto CFD 公式 SFD 階段 (**daily** rate)。
///
/// 入力 `divergence_abs` は乖離率の **絶対値** (例: `dec!(0.07)` = 7%)。
/// 呼び出し側で `.abs()` を取ってから渡すこと (負値は 0% region に落ちる)。
///
///   |x| < 5%        → 0.00%
///   5%  ≤ |x| < 10% → 0.25%
///   10% ≤ |x| < 15% → 0.50%
///   15% ≤ |x| < 20% → 1.00%
///   20% ≤ |x|       → 3.00%
///
/// bitFlyer Crypto CFD 公式 docs に基づく。rate 改定時は本 const を更新。
pub fn sfd_daily_rate(divergence_abs: Decimal) -> Decimal {
    if divergence_abs < dec!(0.05) {
        Decimal::ZERO
    } else if divergence_abs < dec!(0.10) {
        dec!(0.0025)
    } else if divergence_abs < dec!(0.15) {
        dec!(0.005)
    } else if divergence_abs < dec!(0.20) {
        dec!(0.01)
    } else {
        dec!(0.03)
    }
}

/// SFD 計算に必要な context。
///
/// `position_notional` は `entry_price × quantity` の絶対値 (建玉評価額)。
/// `fx_price` / `spot_price` はいずれも JPY 建ての価格。
#[derive(Debug, Clone, Copy)]
pub struct SfdContext {
    pub fx_price: Decimal,
    pub spot_price: Decimal,
    pub position_notional: Decimal,
    pub direction: Direction,
}

/// 1 時間分の SFD を算出する。
///
/// formula:
///   divergence = (fx - spot) / spot
///   daily_rate = sfd_daily_rate(|divergence|)
///   hourly_fee = notional × daily_rate / 24
///   sign:
///     fx > spot かつ Long  → +fee (払う)
///     fx > spot かつ Short → -fee (受け取る)
///     fx < spot かつ Long  → -fee (受け取る)
///     fx < spot かつ Short → +fee (払う)
///
/// `spot_price` が 0 / 負の場合は安全弁として `Decimal::ZERO` を返す
/// (除算エラー回避)。
pub fn compute_hourly_sfd(ctx: SfdContext) -> Decimal {
    if ctx.spot_price <= Decimal::ZERO {
        return Decimal::ZERO;
    }
    let divergence = (ctx.fx_price - ctx.spot_price) / ctx.spot_price;
    let rate = sfd_daily_rate(divergence.abs());
    if rate.is_zero() {
        return Decimal::ZERO;
    }
    let magnitude = ctx.position_notional * rate / Decimal::from(24);
    let sign_positive = match (divergence.is_sign_positive(), ctx.direction) {
        (true, Direction::Long) => true,
        (true, Direction::Short) => false,
        (false, Direction::Long) => false,
        (false, Direction::Short) => true,
    };
    if sign_positive { magnitude } else { -magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_all_exchanges_currently_zero() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(estimate(ex, dec!(150), dec!(1)), Decimal::ZERO);
        }
    }

    #[test]
    fn sfd_daily_rate_below_5pct_is_zero() {
        assert_eq!(sfd_daily_rate(dec!(0)), Decimal::ZERO);
        assert_eq!(sfd_daily_rate(dec!(0.04999)), Decimal::ZERO);
    }

    #[test]
    fn sfd_daily_rate_5_to_10pct_is_0_25pct() {
        assert_eq!(sfd_daily_rate(dec!(0.05)), dec!(0.0025));
        assert_eq!(sfd_daily_rate(dec!(0.09999)), dec!(0.0025));
    }

    #[test]
    fn sfd_daily_rate_10_to_15pct_is_0_5pct() {
        assert_eq!(sfd_daily_rate(dec!(0.10)), dec!(0.005));
        assert_eq!(sfd_daily_rate(dec!(0.14999)), dec!(0.005));
    }

    #[test]
    fn sfd_daily_rate_15_to_20pct_is_1pct() {
        assert_eq!(sfd_daily_rate(dec!(0.15)), dec!(0.01));
        assert_eq!(sfd_daily_rate(dec!(0.19999)), dec!(0.01));
    }

    #[test]
    fn sfd_daily_rate_20pct_or_more_is_3pct() {
        assert_eq!(sfd_daily_rate(dec!(0.20)), dec!(0.03));
        assert_eq!(sfd_daily_rate(dec!(0.50)), dec!(0.03));
    }

    #[test]
    fn sfd_daily_rate_negative_input_treated_as_absolute_should_caller() {
        // sfd_daily_rate は呼び出し側で abs() を取って渡す前提。
        // 万一負値が渡ると 0% region と評価される (符号無視)。
        assert_eq!(sfd_daily_rate(dec!(-0.10)), Decimal::ZERO);
    }

    fn ctx(fx: Decimal, spot: Decimal, notional: Decimal, dir: Direction) -> SfdContext {
        SfdContext {
            fx_price: fx,
            spot_price: spot,
            position_notional: notional,
            direction: dir,
        }
    }

    #[test]
    fn compute_hourly_sfd_zero_when_divergence_below_5pct() {
        let c = ctx(dec!(104), dec!(100), dec!(100_000), Direction::Long);
        assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
    }

    #[test]
    fn compute_hourly_sfd_long_pays_when_fx_above_spot() {
        // FX=110, spot=100 → 乖離 +10% → rate 0.5% (daily), hourly = 0.5%/24
        let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Long);
        let fee = compute_hourly_sfd(c);
        assert!(fee > Decimal::ZERO);
        let expected = dec!(100_000) * dec!(0.005) / dec!(24);
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_short_receives_when_fx_above_spot() {
        let c = ctx(dec!(110), dec!(100), dec!(100_000), Direction::Short);
        let fee = compute_hourly_sfd(c);
        assert!(fee < Decimal::ZERO);
        let expected = -(dec!(100_000) * dec!(0.005) / dec!(24));
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_long_receives_when_fx_below_spot() {
        let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Long);
        let fee = compute_hourly_sfd(c);
        assert!(fee < Decimal::ZERO);
        let expected = -(dec!(100_000) * dec!(0.005) / dec!(24));
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_short_pays_when_fx_below_spot() {
        let c = ctx(dec!(90), dec!(100), dec!(100_000), Direction::Short);
        let fee = compute_hourly_sfd(c);
        assert!(fee > Decimal::ZERO);
        let expected = dec!(100_000) * dec!(0.005) / dec!(24);
        assert_eq!(fee, expected);
    }

    #[test]
    fn compute_hourly_sfd_zero_spot_returns_zero_no_panic() {
        let c = ctx(dec!(100), Decimal::ZERO, dec!(100_000), Direction::Long);
        assert_eq!(compute_hourly_sfd(c), Decimal::ZERO);
    }
}
