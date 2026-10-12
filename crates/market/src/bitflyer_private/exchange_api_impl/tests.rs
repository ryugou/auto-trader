use rust_decimal_macros::dec;
use wiremock::ResponseTemplate;

use crate::bitflyer_private::test_support::{api_for, position_json, server_with_positions};

#[tokio::test]
async fn trait_fetch_close_sfd_delegates_to_inherent() {
    use crate::exchange_api::ExchangeApi as ExchangeApiTrait;

    let body = serde_json::json!([position_json("SELL", "2026-05-17T10:00:00", "42")]);
    let server = server_with_positions(ResponseTemplate::new(200).set_body_json(body)).await;

    let api: Box<dyn ExchangeApiTrait> = Box::new(api_for(&server));
    let sfd = api.fetch_close_sfd("FX_BTC_JPY").await.unwrap();
    assert_eq!(sfd, dec!(42));
}
