pub mod api;
pub mod balance_drift;
pub mod closer;
pub mod enriched_ingest;
pub mod feed_watchdog;
pub mod knowledge;
pub mod liquidation;
pub mod margin_alert;
pub mod positions;
pub mod price_store;
pub mod regime;
#[doc(hidden)]
pub mod startup;
pub mod startup_reconcile;
pub mod stop_fill;
pub mod swap_freshness;
pub mod weekly_batch;
pub mod wilson;

use std::sync::Arc;

/// 通知イベントを fire-and-forget で送信する。送信失敗は warn ログのみ
/// (通知の失敗で本業務を止めない)。`feed_watchdog` と `main.rs` で共有する
/// 送信ロジック。
pub fn spawn_notify(
    notifier: &Arc<auto_trader_notify::Notifier>,
    event: auto_trader_notify::NotifyEvent,
) {
    let notifier = notifier.clone();
    tokio::spawn(async move {
        if let Err(e) = notifier.send(event).await {
            tracing::warn!("notify send failed: {e}");
        }
    });
}

/// 再送 `attempt` 回目 (1 始まり) の待ち時間 (秒) を計算する純粋関数。
/// `initial_secs` から始めて以後 2 倍で増える (指数バックオフ)。壁時計に
/// 依存しないので `spawn_notify_with_retry` から切り出して単体テストできる。
///
/// `attempt` は config 由来の値を経由しうるため、極端に大きい値が渡されても
/// シフト演算オーバーフローや乗算オーバーフローで panic しないよう
/// `saturating_*` で計算する (Resilience: 誤設定値でプロセスを落とさない)。
pub fn notify_retry_delay_secs(attempt: u64, initial_secs: u64) -> u64 {
    let exponent = attempt.saturating_sub(1).min(63) as u32;
    initial_secs.saturating_mul(1u64 << exponent)
}

/// 要約に載せる body 先頭の最大文字数。giving up の ERROR ログは初回送信から
/// 最大約15分後に出るため、title だけでは直前のログと対応付けられない。
/// 取引所・ペア等の識別情報を含む body 先頭まで載せる。
const SUMMARY_BODY_MAX_CHARS: usize = 200;

/// body を文字単位で `SUMMARY_BODY_MAX_CHARS` に切り詰める (超過時は末尾に
/// "…")。バイト境界で切ると UTF-8 でパニックするため `chars()` で切る。
fn truncate_body_for_summary(body: &str) -> String {
    let mut chars = body.chars();
    let mut head: String = chars.by_ref().take(SUMMARY_BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        head.push('…');
    }
    head
}

/// 通知内容の要約。ERROR ログ (全再送失敗時) に載せ、運用者が「どの取引所・
/// 口座の何の通知が届かなかったか」を即座に判断できるようにする。
/// `SystemAlert`/`ProcessAlert` は title・body 先頭 (+ SystemAlert は
/// exchange/account) を含める。他の変種は現時点で `spawn_notify_with_retry`
/// の呼び出し元が使っていないが、無い情報を捏造せず `variant_name()` のみに
/// フォールバックする。
fn notify_event_summary(event: &auto_trader_notify::NotifyEvent) -> String {
    use auto_trader_notify::NotifyEvent;
    match event {
        NotifyEvent::SystemAlert(e) => format!(
            "{} [exchange={} account={}] ({}): {}",
            e.title,
            e.exchange.as_str(),
            e.account_name,
            event.variant_name(),
            truncate_body_for_summary(&e.body)
        ),
        NotifyEvent::ProcessAlert(e) => format!(
            "{} ({}): {}",
            e.title,
            event.variant_name(),
            truncate_body_for_summary(&e.body)
        ),
        _ => event.variant_name().to_string(),
    }
}

/// `send` を1回試行し、失敗したら `notify_retry_delay_secs` の間隔で
/// `attempts` 回まで再送する。`label` はログに載せる通知種別
/// (`NotifyEvent::variant_name()`)。
///
/// `send`/`sleep` を引数で注入できるようにしているのはテスト容易性のため:
/// 実際の HTTP 送信や壁時計待機を行わずに、再送回数・成功/失敗の分岐を
/// 単体テストで検証できる (`spawn_notify_with_retry` は実処理を渡す薄い
/// ラッパー)。
async fn retry_send<S, SFut, L, LFut>(
    label: &str,
    mut send: S,
    attempts: u64,
    initial_secs: u64,
    sleep: L,
) -> Result<(), auto_trader_notify::NotifyError>
where
    S: FnMut() -> SFut,
    SFut: std::future::Future<Output = Result<(), auto_trader_notify::NotifyError>>,
    L: Fn(u64) -> LFut,
    LFut: std::future::Future<Output = ()>,
{
    let mut last_err = match send().await {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    for attempt in 1..=attempts {
        let wait_secs = notify_retry_delay_secs(attempt, initial_secs);
        tracing::warn!(
            "notify send failed (event={label}), retrying in {wait_secs}s (attempt {attempt}/{attempts}): {last_err}"
        );
        sleep(wait_secs).await;
        match send().await {
            Ok(()) => {
                tracing::info!(
                    "notify send succeeded on retry (event={label}, attempt {attempt}/{attempts})"
                );
                return Ok(());
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// 送信失敗時に再送する通知送信 (`spawn_notify` と異なり、本業務をブロック
/// しない別タスクの中で再送まで含めて完遂する)。
///
/// feed watchdog が出す4種の通知 (サスペンド検知、フィード途絶/再通知/復旧、
/// フィードタスク終了) はいずれも低頻度・高重要度で、ホスト復帰直後は
/// DNS/Wi-Fi が未復旧で送信が失敗しうる。1回失敗したら諦める `spawn_notify`
/// では「長時間停止した」という最重要の通知自体が失われるため、`attempts`
/// 回まで指数バックオフで再送する (Issue #109 followup)。
///
/// Webhook 未設定 (`Notifier::send` が no-op で `Ok(())` を返す) 場合は初回で
/// 成功扱いになり、再送は発生しない。
///
/// **ベストエフォートであり、配送を保証しない**: `tokio::spawn` した別タスクが
/// バックグラウンドで再送し続けるだけで、呼び出し元はそのタスクを await も
/// 追跡もしない。プロセスがシャットダウンすると、その時点で再送中だった
/// (まだ全 `attempts` を使い切っていない) 通知は失われる。
pub fn spawn_notify_with_retry(
    notifier: &Arc<auto_trader_notify::Notifier>,
    event: auto_trader_notify::NotifyEvent,
    attempts: u64,
    initial_secs: u64,
) {
    let notifier = notifier.clone();
    tokio::spawn(async move {
        let label = event.variant_name();
        let result = retry_send(
            label,
            || {
                let notifier = notifier.clone();
                let event = event.clone();
                async move { notifier.send(event).await }
            },
            attempts,
            initial_secs,
            |secs| tokio::time::sleep(std::time::Duration::from_secs(secs)),
        )
        .await;
        if let Err(e) = result {
            tracing::error!(
                "notify send failed after {attempts} retries, giving up: event={} last_error={e}",
                notify_event_summary(&event)
            );
        }
    });
}

/// SystemAlert 群を fire-and-forget で送る。起動時 one-shot / 毎時 task の
/// 両方から呼ぶ (balance drift dispatch)。
pub fn spawn_system_alerts(
    notifier: &Arc<auto_trader_notify::Notifier>,
    alerts: Vec<auto_trader_notify::SystemAlertEvent>,
) {
    for ev in alerts {
        spawn_notify(notifier, auto_trader_notify::NotifyEvent::SystemAlert(ev));
    }
}

#[cfg(test)]
mod notify_retry_tests {
    use super::*;
    use auto_trader_notify::{Notifier, NotifyError, NotifyEvent, ProcessAlertEvent};
    use std::sync::atomic::{AtomicU64, Ordering};

    // ----- notify_retry_delay_secs (純粋関数) -----

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
        // config 由来の値のため、誤設定で極端に大きい attempt が渡されても
        // シフト/乗算オーバーフローで panic しないことを確認する (Resilience)。
        assert_eq!(notify_retry_delay_secs(u64::MAX, 30), u64::MAX);
    }

    // ----- notify_event_summary -----

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

    // ----- retry_send (内部ヘルパー; send/sleep を注入してテストする) -----

    async fn immediate_sleep(_secs: u64) {}

    #[tokio::test]
    async fn retry_send_stops_as_soon_as_a_retry_succeeds() {
        // 2回失敗し3回目で成功する場合: 送信試行は3回だけ呼ばれ、以後は
        // 呼ばれない (= 通知は1回だけ届く)。
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
        // Notifier::send は webhook 未設定なら即 Ok を返す no-op。実際の
        // Notifier を使い、初回成功で再送が発生しないことを確認する
        // (spawn_notify_with_retry が配線する経路そのもの)。
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
}
