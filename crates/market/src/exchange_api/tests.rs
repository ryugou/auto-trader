use chrono::{DateTime, Utc};
use rust_decimal_macros::dec;

use super::*;
use crate::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};

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
    let fills = vec![
        (dec!(100), dec!(2), dec!(1)),
        (dec!(110), dec!(1), dec!(0.5)),
    ];
    let (avg_price, total_size, total_fee) = weighted_avg_fills(fills).unwrap();
    assert_eq!(avg_price, dec!(310) / dec!(3));
    assert_eq!(total_size, dec!(3));
    assert_eq!(total_fee, dec!(1.5));
}

#[test]
fn weighted_avg_fills_zero_size_errors() {
    let fills: Vec<(Decimal, Decimal, Decimal)> = vec![(dec!(100), dec!(0), dec!(0))];
    assert!(weighted_avg_fills(fills).is_err());
}
