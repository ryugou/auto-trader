use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use wiremock::MockServer;

use super::GmoFxPrivateApi;
use crate::bitflyer_private::{ChildOrderType, SendChildOrderRequest, Side};

pub(super) fn offline_api() -> GmoFxPrivateApi {
    GmoFxPrivateApi::new("k".into(), "s".into()).unwrap()
}

pub(super) fn api_for(server: &MockServer) -> GmoFxPrivateApi {
    offline_api().with_api_url(server.uri())
}

pub(super) fn market_order(
    price: Option<Decimal>,
    close_position_id: Option<&str>,
) -> SendChildOrderRequest {
    SendChildOrderRequest {
        product_code: "USD_JPY".into(),
        child_order_type: ChildOrderType::Market,
        side: Side::Buy,
        size: dec!(1000),
        price,
        minute_to_expire: None,
        time_in_force: None,
        close_position_id: close_position_id.map(str::to_string),
    }
}
