use rust_decimal_macros::dec;

use super::*;

#[test]
fn format_order_filled() {
    let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
        account_name: "通常".into(),
        exchange: Exchange::BitflyerCfd,
        trade_id: Uuid::nil(),
        pair: Pair::new("FX_BTC_JPY"),
        direction: Direction::Long,
        quantity: dec!(0.01),
        price: dec!(11500000),
        at: Utc::now(),
    });
    let s = format_for_slack(&ev);
    assert!(s.contains("約定"));
    assert!(s.contains("通常"));
    assert!(s.contains("bitflyer_cfd"));
    assert!(s.contains("FX_BTC_JPY"));
    assert!(s.contains("11500000"));
}

#[test]
fn format_process_alert() {
    let ev = NotifyEvent::ProcessAlert(ProcessAlertEvent {
        title: "process suspend detected".into(),
        body: "process was unresponsive for 2820 minute(s)".into(),
    });
    assert_eq!(ev.variant_name(), "process_alert");
    let s = format_for_slack(&ev);
    assert!(s.contains("process suspend detected"));
    assert!(s.contains("2820 minute"));
}

#[tokio::test]
async fn send_without_webhook_is_noop() {
    let n = Notifier::new(None).unwrap();
    let ev = NotifyEvent::OrderFailed(OrderFailedEvent {
        account_name: "通常".into(),
        exchange: Exchange::BitflyerCfd,
        strategy_name: "test".into(),
        pair: Pair::new("FX_BTC_JPY"),
        reason: "test".into(),
    });
    n.send(ev).await.unwrap();
}
