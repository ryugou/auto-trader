//! Slack Webhook 送信の統合テスト。wiremock でダミーの Slack サーバーを
//! 立ち上げ、Notifier が適切なペイロードを POST することを検証する。

use auto_trader_core::types::{Direction, Pair};
use auto_trader_notify::*;
use rust_decimal_macros::dec;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn notifier_posts_text_payload_to_slack_url() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let notifier = Notifier::new(Some(server.uri()));
    let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "通常".into(),
        trade_id: Uuid::nil(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        quantity: dec!(0.005),
        price: dec!(11500000),
        at: chrono::Utc::now(),
    });

    notifier
        .send(ev)
        .await
        .expect("send should succeed against 200");
    // Mock::expect(1) が満たされなければドロップ時に panic する
}

#[tokio::test]
async fn notifier_returns_error_on_5xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let notifier = Notifier::new(Some(server.uri()));
    let ev = NotifyEvent::WebSocketDisconnected(WebSocketDisconnectedEvent { duration_secs: 42 });
    let err = notifier.send(ev).await.unwrap_err();
    match err {
        NotifyError::Status(code) => assert_eq!(code, 500),
        other => panic!("expected Status(500), got {:?}", other),
    }
}

#[tokio::test]
async fn notifier_noop_when_url_none() {
    let notifier = Notifier::new(None);
    let ev = NotifyEvent::KillSwitchReleased(KillSwitchReleasedEvent {
        account_name: "通常".into(),
    });
    notifier.send(ev).await.expect("noop should return Ok");
}
