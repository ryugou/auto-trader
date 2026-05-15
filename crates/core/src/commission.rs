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

/// commission を見積もる対象の約定種別。
///
/// open / close で個別の料率を取りたい時にここを増やせば、`estimate` の
/// match で全 exchange 対応を強制できる (新 variant 追加時にコンパイル
/// エラーで気づける構造)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Close,
}

/// paper 側 commission を見積もる。現状は全 exchange / 全 action で 0 を返す。
///
/// `_fill_price` / `_qty` は将来 notional / qty 比例料率を入れる時のための
/// 拡張口。今は未使用で warning も抑える。
pub fn estimate(
    _action: Action,
    exchange: Exchange,
    _fill_price: Decimal,
    _qty: Decimal,
) -> Decimal {
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
    fn estimate_all_exchanges_currently_zero_for_open() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(
                estimate(Action::Open, ex, dec!(150), dec!(1)),
                Decimal::ZERO
            );
        }
    }

    #[test]
    fn estimate_all_exchanges_currently_zero_for_close() {
        for ex in [Exchange::BitflyerCfd, Exchange::GmoFx, Exchange::Oanda] {
            assert_eq!(
                estimate(Action::Close, ex, dec!(150), dec!(1)),
                Decimal::ZERO
            );
        }
    }
}
