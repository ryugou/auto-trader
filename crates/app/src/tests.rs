use std::sync::atomic::{AtomicU64, Ordering};

use auto_trader_notify::{Notifier, NotifyError, NotifyEvent, ProcessAlertEvent};

use super::*;

#[test]
fn notify_retry_delay_secs_doubles_each_attempt() {
    assert_eq!(notify_retry_delay_secs(1, 30), 30);
    assert_eq!(notify_retry_delay_secs(2, 30), 60);
    assert_eq!(notify_retry_delay_secs(3, 30), 120);
    assert_eq!(notify_retry_delay_secs(4, 30), 240);
    assert_eq!(notify_retry_delay_secs(5, 30), 480);
}

#[test]
fn notify_retry_delay_secs_does_not_panic_on_extreme_attempt() {
    assert_eq!(notify_retry_delay_secs(u64::MAX, 30), u64::MAX);
}

fn system_alert(body: String) -> NotifyEvent {
    NotifyEvent::SystemAlert(auto_trader_notify::SystemAlertEvent {
        title: "feed stale".to_string(),
        account_name: "(system)".to_string(),
        exchange: auto_trader_core::types::Exchange::GmoFx,
        body,
    })
}

#[test]
fn notify_event_summary_system_alert_includes_title_exchange_and_body_head() {
    let summary =
        notify_event_summary(&system_alert("feed gmo_fx USD_JPY has stopped".to_string()));
    assert_eq!(
        summary,
        "feed stale [exchange=gmo_fx account=(system)] (system_alert): feed gmo_fx USD_JPY has stopped"
    );
}

#[test]
fn notify_event_summary_truncates_long_multibyte_body_without_panic() {
    let summary = notify_event_summary(&system_alert("あ".repeat(300)));
    let expected_tail = format!("{}…", "あ".repeat(200));
    assert!(
        summary.ends_with(&expected_tail),
        "body must be cut to 200 chars and end with an ellipsis: {summary}"
    );
    assert_eq!(summary.matches('あ').count(), 200);
}

#[test]
fn notify_event_summary_process_alert_includes_title_and_body() {
    let summary = notify_event_summary(&NotifyEvent::ProcessAlert(ProcessAlertEvent {
        title: "process suspend detected".to_string(),
        body: "gap 900s".to_string(),
    }));
    assert_eq!(
        summary,
        "process suspend detected (process_alert): gap 900s"
    );
}

async fn immediate_sleep(_secs: u64) {}

#[tokio::test]
async fn retry_send_stops_as_soon_as_a_retry_succeeds() {
    let call_count = AtomicU64::new(0);
    let result = retry_send(
        "test_event",
        || {
            let n = call_count.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                if n < 3 {
                    Err(NotifyError::Status(500))
                } else {
                    Ok(())
                }
            }
        },
        5,
        1,
        immediate_sleep,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        3,
        "must not call send again once a retry succeeds"
    );
}

#[tokio::test]
async fn retry_send_gives_up_after_exhausting_all_attempts() {
    let call_count = AtomicU64::new(0);
    let attempts = 2;
    let result = retry_send(
        "test_event",
        || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async move { Err::<(), _>(NotifyError::Status(500)) }
        },
        attempts,
        1,
        immediate_sleep,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1 + attempts,
        "must try once plus `attempts` retries, then stop"
    );
}

#[tokio::test]
async fn retry_send_with_zero_attempts_tries_exactly_once() {
    let call_count = AtomicU64::new(0);
    let result = retry_send(
        "test_event",
        || {
            call_count.fetch_add(1, Ordering::SeqCst);
            async move { Err::<(), _>(NotifyError::Status(500)) }
        },
        0,
        1,
        immediate_sleep,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_send_does_not_retry_when_notifier_has_no_webhook_configured() {
    let notifier = Notifier::new_disabled();
    let call_count = AtomicU64::new(0);
    let event = NotifyEvent::ProcessAlert(ProcessAlertEvent {
        title: "test".to_string(),
        body: "test".to_string(),
    });
    let result = retry_send(
        "process_alert",
        || {
            call_count.fetch_add(1, Ordering::SeqCst);
            let notifier = &notifier;
            let event = event.clone();
            async move { notifier.send(event).await }
        },
        5,
        1,
        immediate_sleep,
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "webhook未設定なら再送してはならない"
    );
}
