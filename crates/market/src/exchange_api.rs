//! Trait abstraction over exchange-specific Private API clients.
//!
//! Adding a new exchange means implementing this trait for a new client
//! struct. `Trader` and `main.rs` dispatch consume the trait object, so
//! strategy + DB + Signal layers are already exchange-agnostic.

use async_trait::async_trait;

// Re-use the existing request/response types from bitflyer_private for now.
// If another exchange needs a different shape, introduce neutral types in
// this module in a follow-up — for now the trait mirrors bitFlyer's shape
// since it's the only implementor.
use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use rust_decimal::Decimal;

/// 約定リストの (price, size, fee) を size 加重平均に集約する。
/// 戻り値 (加重平均 price, 合計 size, 合計 fee)。合計 size が 0 なら Err。
/// 金融計算なので必ずこの 1 実装を使うこと (trader / bitFlyer / GMO 共通)。
pub fn weighted_avg_fills<I>(fills: I) -> anyhow::Result<(Decimal, Decimal, Decimal)>
where
    I: IntoIterator<Item = (Decimal, Decimal, Decimal)>, // (price, size, fee)
{
    let (total_size, total_notional, total_fee) = fills.into_iter().fold(
        (Decimal::ZERO, Decimal::ZERO, Decimal::ZERO),
        |(s, n, f), (price, size, fee)| (s + size, n + price * size, f + fee),
    );
    if total_size.is_zero() {
        anyhow::bail!("weighted_avg_fills: total size is zero");
    }
    Ok((total_notional / total_size, total_size, total_fee))
}

/// 取引所側ストップ (逆指値) 注文の状態。
#[derive(Debug, Clone, PartialEq)]
pub enum StopOrderStatus {
    /// まだ発火していない
    Active,
    /// 発火して約定済み。price は加重平均約定価格。
    Executed { price: Decimal, commission: Decimal },
    /// キャンセル / 失効 / 拒否 (もはや存在しない)
    Gone,
}

#[async_trait]
pub trait ExchangeApi: Send + Sync {
    async fn send_child_order(
        &self,
        req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse>;

    async fn get_child_orders(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>>;

    async fn get_executions(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>>;

    async fn get_positions(&self, product_code: &str) -> anyhow::Result<Vec<ExchangePosition>>;

    async fn get_collateral(&self) -> anyhow::Result<Collateral>;

    async fn cancel_child_order(
        &self,
        product_code: &str,
        child_order_acceptance_id: &str,
    ) -> anyhow::Result<()>;

    /// Return the exchange-side position identifier created by a recent open
    /// order. `after` is the timestamp when the open was sent;
    /// `expected_side` / `expected_size` are additional discriminators so the
    /// implementation does not match an unrelated position opened by another
    /// strategy / manual trade / parallel process on the same account.
    /// Returns `Ok(None)` when the exchange does not model positions
    /// individually (bitFlyer nets positions internally) or no matching
    /// position is found.
    async fn resolve_position_id(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> anyhow::Result<Option<String>>;

    /// `true` when this exchange requires a position identifier on close
    /// requests. GMO FX returns `true` (close requests without a position id
    /// would silently open an opposite position via `/v1/order`). bitFlyer
    /// returns `false` (closes are opposite-side market orders against the
    /// netted position). Trader uses this to refuse live closes that lack a
    /// resolved exchange_position_id.
    fn requires_close_position_id(&self) -> bool {
        false
    }

    /// Accumulated SFD (Swap For Difference, bitFlyer Crypto CFD only) for
    /// the given product at close time. Default implementation returns 0 —
    /// only exchanges that charge SFD override.
    ///
    /// SFD は bitFlyer Crypto CFD で現物価格と FX 価格の乖離が一定以上の時
    /// 発生する累積手数料。close 直前に 1 度読んで `Trade.fees` に積む。
    async fn fetch_close_sfd(&self, _product_code: &str) -> anyhow::Result<Decimal> {
        Ok(Decimal::ZERO)
    }

    /// SL ストップ注文を置く。戻り値は取引所発行の注文 ID。
    /// `position_id` は GMO のように close 対象 position の指定が必要な
    /// 取引所で Some、bitFlyer (netting) では None。
    ///
    /// デフォルトは bail — dry_run / 未対応取引所からは呼ばれない設計であり、
    /// 呼ばれたら即座に露見させる。bitFlyer / GMO のみ override する。
    async fn place_stop_order(
        &self,
        _product_code: &str,
        _close_side: Side,
        _size: Decimal,
        _trigger_price: Decimal,
        _position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("place_stop_order not supported on this exchange")
    }

    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::bail!("cancel_stop_order not supported on this exchange")
    }

    async fn stop_order_status(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        anyhow::bail!("stop_order_status not supported on this exchange")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitflyer_private::{
        ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
        SendChildOrderResponse, Side,
    };
    use chrono::{DateTime, Utc};

    /// default fetch_close_sfd 実装が常に Ok(0) を返すことを確認する
    /// 簡易スタブ。method を override しなければこの挙動になる。
    struct StubApi;

    #[async_trait]
    impl ExchangeApi for StubApi {
        async fn send_child_order(
            &self,
            _: SendChildOrderRequest,
        ) -> anyhow::Result<SendChildOrderResponse> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn get_child_orders(&self, _: &str, _: &str) -> anyhow::Result<Vec<ChildOrder>> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn get_executions(&self, _: &str, _: &str) -> anyhow::Result<Vec<Execution>> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn get_positions(&self, _: &str) -> anyhow::Result<Vec<ExchangePosition>> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn get_collateral(&self) -> anyhow::Result<Collateral> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn cancel_child_order(&self, _: &str, _: &str) -> anyhow::Result<()> {
            panic!("not used in default_fetch_close_sfd_returns_zero test")
        }
        async fn resolve_position_id(
            &self,
            _: &str,
            _: DateTime<Utc>,
            _: Side,
            _: Decimal,
        ) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn default_fetch_close_sfd_returns_zero() {
        let api = StubApi;
        let sfd = api.fetch_close_sfd("FX_BTC_JPY").await.unwrap();
        assert_eq!(sfd, Decimal::ZERO);
    }

    #[test]
    fn weighted_avg_fills_happy_path() {
        use rust_decimal_macros::dec;
        // fill 1: price=100, size=2, fee=1 / fill 2: price=110, size=1, fee=0.5
        let fills = vec![
            (dec!(100), dec!(2), dec!(1)),
            (dec!(110), dec!(1), dec!(0.5)),
        ];
        let (avg_price, total_size, total_fee) = weighted_avg_fills(fills).unwrap();
        // (100*2 + 110*1) / 3 = 310/3
        assert_eq!(avg_price, dec!(310) / dec!(3));
        assert_eq!(total_size, dec!(3));
        assert_eq!(total_fee, dec!(1.5));
    }

    #[test]
    fn weighted_avg_fills_zero_size_errors() {
        use rust_decimal_macros::dec;
        let fills: Vec<(Decimal, Decimal, Decimal)> = vec![(dec!(100), dec!(0), dec!(0))];
        assert!(weighted_avg_fills(fills).is_err());
    }
}
