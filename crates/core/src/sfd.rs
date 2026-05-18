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

use crate::types::Exchange;
use rust_decimal::Decimal;

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

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn estimate_all_exchanges_currently_zero() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(estimate(ex, dec!(150), dec!(1)), Decimal::ZERO);
        }
    }
}
