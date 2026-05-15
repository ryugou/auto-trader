//! exchange 別の公式 commission を計算する pure 関数群。
//!
//! 現状 全 Exchange で 0 を返す (各取引所の現実の commission レートを反映:
//! GMO FX は spread のみ、bitFlyer Crypto CFD は基本 0、OANDA はマージン取引で 0)。
//! 将来 commission レートが 0 でなくなった時、このファイルを更新するだけで
//! paper 側の Trade.fees 計算が live と等価のまま保たれる (live は
//! `Execution.commission` 経由で API 値を自動追従)。
//!
//! Exchange enum を exhaustive match で扱うので、新 variant 追加時はコンパイル
//! エラーで気づける。

use crate::types::Exchange;
use rust_decimal::Decimal;

/// open 約定時の commission を計算する (paper 側 estimate)。
pub fn estimate_open(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
    match exchange {
        Exchange::BitflyerCfd => Decimal::ZERO,
        Exchange::GmoFx => Decimal::ZERO,
        Exchange::Oanda => Decimal::ZERO,
    }
}

/// close 約定時の commission を計算する (paper 側 estimate)。
pub fn estimate_close(exchange: Exchange, _fill_price: Decimal, _qty: Decimal) -> Decimal {
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
    fn estimate_open_all_exchanges_currently_zero() {
        assert_eq!(
            estimate_open(Exchange::BitflyerCfd, dec!(150), dec!(1)),
            Decimal::ZERO
        );
        assert_eq!(
            estimate_open(Exchange::GmoFx, dec!(150), dec!(1)),
            Decimal::ZERO
        );
        assert_eq!(
            estimate_open(Exchange::Oanda, dec!(150), dec!(1)),
            Decimal::ZERO
        );
    }

    #[test]
    fn estimate_close_all_exchanges_currently_zero() {
        assert_eq!(
            estimate_close(Exchange::BitflyerCfd, dec!(150), dec!(1)),
            Decimal::ZERO
        );
        assert_eq!(
            estimate_close(Exchange::GmoFx, dec!(150), dec!(1)),
            Decimal::ZERO
        );
        assert_eq!(
            estimate_close(Exchange::Oanda, dec!(150), dec!(1)),
            Decimal::ZERO
        );
    }
}
