//! Phase 3: Notification format — OrderFilled, OrderFailed, PositionClosed.
//!
//! NotifyEvent 各バリアントのフィールド構築と Slack 整形を検証する。

use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_notify::{
    Notifier, NotifyEvent, OrderFailedEvent, OrderFilledEvent, PositionClosedEvent,
};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

// =========================================================================
// 3.100: OrderFilled format
// =========================================================================

/// OrderFilled イベントのフィールドが正しく構築され、
/// variant_name が "order_filled" であること。
#[test]
fn order_filled_event_format() {
    let trade_id = Uuid::new_v4();
    let now = Utc::now();
    let event = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "テスト口座".into(),
        exchange: Exchange::GmoFx,
        trade_id,
        pair: Pair::new("USD_JPY"),
        direction: Direction::Long,
        quantity: dec!(1000),
        price: dec!(150.123),
        at: now,
    });

    assert_eq!(event.variant_name(), "order_filled");

    // Verify the inner fields are accessible
    if let NotifyEvent::OrderFilled(ref e) = event {
        assert_eq!(e.account_name, "テスト口座");
        assert_eq!(e.exchange, Exchange::GmoFx);
        assert_eq!(e.trade_id, trade_id);
        assert_eq!(e.pair, Pair::new("USD_JPY"));
        assert_eq!(e.direction, Direction::Long);
        assert_eq!(e.quantity, dec!(1000));
        assert_eq!(e.price, dec!(150.123));
        assert_eq!(e.at, now);
    } else {
        panic!("expected OrderFilled variant");
    }
}

/// OrderFilled の Slack 送信が no-op (webhook 未設定) で成功すること。
#[tokio::test]
async fn order_filled_noop_send() {
    let notifier = Notifier::new_disabled();
    let event = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "テスト".into(),
        exchange: Exchange::BitflyerCfd,
        trade_id: Uuid::new_v4(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Short,
        quantity: dec!(0.01),
        price: dec!(11500000),
        at: Utc::now(),
    });

    notifier
        .send(event)
        .await
        .expect("noop send should succeed");
}

// =========================================================================
// 3.101: OrderFailed format
// =========================================================================

/// OrderFailed イベントのフィールド検証。
#[test]
fn order_failed_event_format() {
    let event = NotifyEvent::OrderFailed(OrderFailedEvent {
        account_name: "本番口座".into(),
        exchange: Exchange::Oanda,
        strategy_name: "donchian_trend_v1".into(),
        pair: Pair::new("EUR_JPY"),
        reason: "insufficient margin".into(),
    });

    assert_eq!(event.variant_name(), "order_failed");

    if let NotifyEvent::OrderFailed(ref e) = event {
        assert_eq!(e.account_name, "本番口座");
        assert_eq!(e.exchange, Exchange::Oanda);
        assert_eq!(e.strategy_name, "donchian_trend_v1");
        assert_eq!(e.pair, Pair::new("EUR_JPY"));
        assert_eq!(e.reason, "insufficient margin");
    } else {
        panic!("expected OrderFailed variant");
    }
}

/// OrderFailed の Slack 送信が no-op で成功。
#[tokio::test]
async fn order_failed_noop_send() {
    let notifier = Notifier::new_disabled();
    let event = NotifyEvent::OrderFailed(OrderFailedEvent {
        account_name: "テスト".into(),
        exchange: Exchange::GmoFx,
        strategy_name: "bb_mean_revert_v1".into(),
        pair: Pair::new("USD_JPY"),
        reason: "price stale".into(),
    });

    notifier
        .send(event)
        .await
        .expect("noop send should succeed");
}

// =========================================================================
// 3.102: PositionClosed format
// =========================================================================

/// PositionClosed イベントのフィールド検証。
#[test]
fn position_closed_event_format() {
    let trade_id = Uuid::new_v4();
    let event = NotifyEvent::PositionClosed(PositionClosedEvent {
        account_name: "ペーパー口座".into(),
        exchange: Exchange::BitflyerCfd,
        trade_id,
        pnl_amount: dec!(-5000),
        reason: "sl_hit".into(),
    });

    assert_eq!(event.variant_name(), "position_closed");

    if let NotifyEvent::PositionClosed(ref e) = event {
        assert_eq!(e.account_name, "ペーパー口座");
        assert_eq!(e.exchange, Exchange::BitflyerCfd);
        assert_eq!(e.trade_id, trade_id);
        assert_eq!(e.pnl_amount, dec!(-5000));
        assert_eq!(e.reason, "sl_hit");
    } else {
        panic!("expected PositionClosed variant");
    }
}

/// PositionClosed の Slack 送信が no-op で成功。
#[tokio::test]
async fn position_closed_noop_send() {
    let notifier = Notifier::new_disabled();
    let event = NotifyEvent::PositionClosed(PositionClosedEvent {
        account_name: "テスト".into(),
        exchange: Exchange::GmoFx,
        trade_id: Uuid::new_v4(),
        pnl_amount: dec!(12345),
        reason: "tp_hit".into(),
    });

    notifier
        .send(event)
        .await
        .expect("noop send should succeed");
}
