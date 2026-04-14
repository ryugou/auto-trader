//! Slack Webhook 送信の統合テスト。wiremock でダミーの Slack サーバーを
//! 立ち上げ、Notifier が適切なペイロードを POST することを検証する。

use auto_trader_core::types::{Direction, Pair};
use auto_trader_notify::*;
use rust_decimal_macros::dec;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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

    // 受信したリクエスト body を検証し、Slack Incoming Webhook が要求する
    // `{"text": "..."}` 形式を満たしているか、および format_for_slack が
    // 期待される要素 (アカウント名 / ペア / 方向 / 数量 / 価格) を全て
    // 含めているか確認する。
    let received: Vec<Request> = server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    let text = body["text"]
        .as_str()
        .expect("body must have a string `text` field");
    assert!(text.contains("通常"), "text missing account_name: {text}");
    assert!(text.contains("FX_BTC_JPY"), "text missing pair: {text}");
    assert!(text.contains("long"), "text missing direction: {text}");
    assert!(text.contains("0.005"), "text missing quantity: {text}");
    assert!(text.contains("11500000"), "text missing price: {text}");
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

/// 回帰防止: `reqwest::Error` の Display は失敗した URL (= Slack
/// Webhook secret) を含むため、`NotifyError::Http` に流し込む前に
/// `without_url()` で落とす必要がある。接続先が存在しない URL
/// (即座に connection refused を起こす 127.0.0.1:1 へ、認識可能な
/// secret マーカーを path に入れて) POST させ、`NotifyError::Http`
/// の Display / Debug 出力に URL / secret 文字列が含まれないこと
/// を検証する。
#[tokio::test]
async fn notifier_http_error_does_not_leak_webhook_url() {
    // 127.0.0.1:1 は通常 connection refused。path に secret マーカーを
    // 入れることで、redact が効いていない場合に検出できる。
    let url_with_secret_marker = "http://127.0.0.1:1/services/SECRET_TOKEN_NEVER_LEAK".to_string();
    let notifier = Notifier::new(Some(url_with_secret_marker));

    let ev = NotifyEvent::KillSwitchReleased(KillSwitchReleasedEvent {
        account_name: "通常".into(),
    });
    let err = notifier
        .send(ev)
        .await
        .expect_err("send should fail against closed port");
    let rendered_display = format!("{err}");
    let rendered_debug = format!("{err:?}");
    assert!(
        !rendered_display.contains("SECRET_TOKEN_NEVER_LEAK"),
        "URL leaked in Display: {rendered_display}"
    );
    assert!(
        !rendered_debug.contains("SECRET_TOKEN_NEVER_LEAK"),
        "URL leaked in Debug: {rendered_debug}"
    );
    assert!(
        !rendered_display.contains("127.0.0.1:1"),
        "host:port leaked in Display: {rendered_display}"
    );
}
