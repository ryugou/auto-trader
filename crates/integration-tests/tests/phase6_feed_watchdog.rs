//! Issue #109: feed watchdog の end-to-end 配線確認。
//!
//! 純粋ロジック (サスペンド判定・途絶状態遷移・フィード終了分類) は
//! `crates/app/src/feed_watchdog.rs` の単体テストで検証済み。ここでは
//! `auto_trader::feed_watchdog::run` を実際に spawn し、PriceStore に
//! 一度も tick が無いフィードについて、設定した `stale_after_secs` を
//! 超えた後に **実際に Slack (モック webhook) へ通知が届く** ことを
//! 1件確認する (既存の `MockSlackWebhook` + `Notifier` の統合テスト方式
//! に合わせる — 例: `phase4_stop_orders.rs`)。

use std::sync::Arc;
use std::time::Duration;

use auto_trader::feed_watchdog::{FeedWatchdogContext, run, spawn_feed_supervisor};
use auto_trader_core::config::FeedWatchdogConfig;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_integration_tests::mocks::slack_webhook::MockSlackWebhook;
use auto_trader_market::market_feed::MarketFeed;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::Notifier;

#[tokio::test]
async fn never_ticked_feed_triggers_slack_stale_notification() {
    // 1件も tick が無い GmoFx/USD_JPY フィードを「期待されるフィード」として登録。
    let feed_key = FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY"));
    let price_store = PriceStore::new(vec![feed_key]);

    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));

    // 実時間で数秒以内にテストが終わるよう、秒数は極小値にする
    // (validate() の制約: suspend_gap_secs > check_interval_secs,
    //  stale_after_secs >= check_interval_secs は満たしたまま)。
    // 再送の挙動はこのテストの対象外なので notify_retry_attempts=0
    // (再送無し) にして、realert_interval_secs(60) に対する再送の最悪時間の
    // 制約 (validate() 参照) を単純に満たす。
    let config = FeedWatchdogConfig {
        enabled: true,
        check_interval_secs: 1,
        suspend_gap_secs: 60,
        stale_after_secs: 1,
        realert_interval_secs: 60,
        exclude_exchanges: Vec::new(),
        notify_retry_attempts: 0,
        notify_retry_initial_secs: 1,
    };
    config.validate().expect("test config must itself be valid");

    let ctx = FeedWatchdogContext {
        price_store,
        notifier,
        config,
    };

    let handle = tokio::spawn(run(ctx));

    let mut saw_alert = false;
    for _ in 0..50 {
        let bodies = slack.captured_bodies();
        if bodies
            .iter()
            .any(|b| b.contains("feed stale") && b.contains("gmo_fx"))
        {
            saw_alert = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    handle.abort();

    assert!(
        saw_alert,
        "expected a 'feed stale' Slack notification for the never-ticked gmo_fx/USD_JPY feed, got bodies={:?}",
        slack.captured_bodies()
    );
}

// ----- 項目1: exclude_exchanges (OANDA の誤通知防止) -----

#[tokio::test]
async fn excluded_exchange_never_ticked_does_not_trigger_stale_notification() {
    // OANDA は PriceStore を自身で更新せず最新tickが常に古く見えるため、
    // 鮮度監視から除外する (Issue #109 item 1)。exclude_exchanges=["oanda"]
    // を設定した状態で一度も tick が無い OANDA フィードについて、
    // stale_after_secs を十分超えた後も「feed stale」通知が出ないことを確認する。
    let feed_key = FeedKey::new(Exchange::Oanda, Pair::new("USD_JPY"));
    let price_store = PriceStore::new(vec![feed_key]);

    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));

    // 再送の挙動はこのテストの対象外なので notify_retry_attempts=0 (再送無し)
    // にして、realert_interval_secs(60) に対する再送の最悪時間の制約
    // (validate() 参照) を単純に満たす。
    let config = FeedWatchdogConfig {
        enabled: true,
        check_interval_secs: 1,
        suspend_gap_secs: 60,
        stale_after_secs: 1,
        realert_interval_secs: 60,
        exclude_exchanges: vec!["oanda".to_string()],
        notify_retry_attempts: 0,
        notify_retry_initial_secs: 1,
    };
    config.validate().expect("test config must itself be valid");

    let ctx = FeedWatchdogContext {
        price_store,
        notifier,
        config,
    };

    let handle = tokio::spawn(run(ctx));

    // stale_after_secs=1 を複数ラウンド分十分に超えるまで固定時間待つ
    // (除外されていれば、この間に stale 通知は一切飛ばないはず)。
    tokio::time::sleep(Duration::from_secs(3)).await;
    handle.abort();

    let bodies = slack.captured_bodies();
    assert!(
        !bodies.iter().any(|b| b.contains("feed stale")),
        "oanda should be excluded from freshness monitoring, got bodies={bodies:?}"
    );
}

// ----- 項目8: フィードタスク終了検知の配線 (spawn_feed_supervisor) -----

enum FeedOutcome {
    Ok,
    Err,
    Panic,
}

struct ScriptedFeed(FeedOutcome);

#[async_trait::async_trait]
impl MarketFeed for ScriptedFeed {
    async fn run(
        self: Box<Self>,
        _price_store: Arc<PriceStore>,
        _price_tx: tokio::sync::mpsc::Sender<PriceEvent>,
    ) -> anyhow::Result<()> {
        match self.0 {
            FeedOutcome::Ok => Ok(()),
            FeedOutcome::Err => Err(anyhow::anyhow!("scripted error")),
            FeedOutcome::Panic => panic!("scripted panic"),
        }
    }
}

/// `captured_bodies()` を `needle` を含むものが現れるまでポーリングする。
/// 既存テストのインライン待機ループと同じ方式 (固定スリープ + 上限回数)。
async fn wait_for_body(slack: &MockSlackWebhook, needle: &str) -> bool {
    for _ in 0..50 {
        if slack.captured_bodies().iter().any(|b| b.contains(needle)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

#[tokio::test]
async fn spawn_feed_supervisor_notifies_on_normal_exit() {
    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));
    let (tx, _rx) = tokio::sync::mpsc::channel::<PriceEvent>(1);
    let price_store = PriceStore::new(vec![]);
    let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> =
        tokio::spawn(Box::new(ScriptedFeed(FeedOutcome::Ok)).run(price_store, tx));

    spawn_feed_supervisor(Exchange::GmoFx, notifier, inner_handle, 5, 30);

    assert!(
        wait_for_body(&slack, "normally").await,
        "expected a 'feed task exited ... normally' notification, got bodies={:?}",
        slack.captured_bodies()
    );
}

#[tokio::test]
async fn spawn_feed_supervisor_notifies_on_error_exit() {
    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));
    let (tx, _rx) = tokio::sync::mpsc::channel::<PriceEvent>(1);
    let price_store = PriceStore::new(vec![]);
    let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> =
        tokio::spawn(Box::new(ScriptedFeed(FeedOutcome::Err)).run(price_store, tx));

    spawn_feed_supervisor(Exchange::BitflyerCfd, notifier, inner_handle, 5, 30);

    assert!(
        wait_for_body(&slack, "scripted error").await,
        "expected a notification containing the scripted error, got bodies={:?}",
        slack.captured_bodies()
    );
}

#[tokio::test]
async fn spawn_feed_supervisor_notifies_on_panic() {
    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));
    let (tx, _rx) = tokio::sync::mpsc::channel::<PriceEvent>(1);
    let price_store = PriceStore::new(vec![]);
    let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> =
        tokio::spawn(Box::new(ScriptedFeed(FeedOutcome::Panic)).run(price_store, tx));

    spawn_feed_supervisor(Exchange::Oanda, notifier, inner_handle, 5, 30);

    assert!(
        wait_for_body(&slack, "scripted panic").await,
        "expected a notification containing the scripted panic message, got bodies={:?}",
        slack.captured_bodies()
    );
}

#[tokio::test]
async fn spawn_feed_supervisor_does_not_notify_on_abort() {
    let (slack, webhook_url) = MockSlackWebhook::start().await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));
    let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        Ok(())
    });

    // spawn_feed_supervisor が返す AbortHandle は inner_handle 自身のものなので、
    // これを abort すると classify_feed_task_result が is_cancelled() を見て
    // 通知しない (main.rs の shutdown パスと同じ経路)。
    let abort_handle = spawn_feed_supervisor(Exchange::GmoFx, notifier, inner_handle, 5, 30);
    abort_handle.abort();

    // 通知が無いことの確認なのでポジティブ待機ではなく固定スリープで様子を見る。
    tokio::time::sleep(Duration::from_millis(500)).await;
    let bodies = slack.captured_bodies();
    assert!(
        bodies.is_empty(),
        "abort must not trigger a feed-exit notification, got bodies={bodies:?}"
    );
}

// ----- Issue #109 followup: 通知失敗時の再送 -----

/// webhook が一時的にエラーを返していても、再送によって最終的に通知が届く
/// ことを確認する (ホスト復帰直後の DNS/Wi-Fi 未復旧を想定したシナリオ)。
/// `MockSlackWebhook::with_error_response` で先にエラーへ切り替えておき、
/// 最初の送信試行が失敗応答を受け取るのを確認した後、`with_success_response`
/// で復旧させ、再送によって同じ通知が最終的に 200 を受け取ることまで確認する
/// (モックに届いた件数の増加だけでは、再送が成功応答を得たかまでは分からない)。
#[tokio::test]
async fn stale_notification_is_delivered_after_transient_webhook_failures_recover() {
    let feed_key = FeedKey::new(Exchange::GmoFx, Pair::new("USD_JPY"));
    let price_store = PriceStore::new(vec![feed_key]);

    let (slack, webhook_url) = MockSlackWebhook::start().await;
    slack.with_error_response(500).await;
    let notifier = Arc::new(Notifier::new(Some(webhook_url)));

    // notify_retry_initial_secs は validate() の最小許容値 (>=1)。
    // notify_retry_attempts も本番既定 (5) である必要はなく、テストが長引か
    // ない範囲 (delays: 1s, 2s, 4s) にする。
    let config = FeedWatchdogConfig {
        enabled: true,
        check_interval_secs: 1,
        suspend_gap_secs: 60,
        stale_after_secs: 1,
        realert_interval_secs: 60,
        exclude_exchanges: Vec::new(),
        notify_retry_attempts: 3,
        notify_retry_initial_secs: 1,
    };
    config.validate().expect("test config must itself be valid");

    let ctx = FeedWatchdogContext {
        price_store,
        notifier,
        config,
    };
    let handle = tokio::spawn(run(ctx));

    /// 対象の通知 (本文で特定: "feed stale" + "gmo_fx") だけを抽出する。
    fn target_requests(
        requests: &[auto_trader_integration_tests::mocks::slack_webhook::CapturedRequest],
    ) -> Vec<&auto_trader_integration_tests::mocks::slack_webhook::CapturedRequest> {
        requests
            .iter()
            .filter(|r| r.body.contains("feed stale") && r.body.contains("gmo_fx"))
            .collect()
    }

    // 1. webhook がエラーを返している間に、最初の送信試行が (失敗応答のまま)
    //    Slack モックへ届くのを確認する。
    let mut saw_failed_attempt = false;
    for _ in 0..50 {
        if target_requests(&slack.captured_requests())
            .iter()
            .any(|r| r.status != 200)
        {
            saw_failed_attempt = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        saw_failed_attempt,
        "expected at least one failing stale-notification attempt before the webhook recovers, got requests={:?}",
        slack.captured_requests()
    );

    // 2. webhook を復旧させ、再送が最終的に 200 を受け取ることを確認する。
    //    件数の増加だけでは「再送したが引き続き失敗している」場合と区別
    //    できないため、実際に成功応答 (200) まで確認する。
    slack.with_success_response().await;

    let mut saw_success_after_failure = false;
    for _ in 0..50 {
        let requests = slack.captured_requests();
        let matching = target_requests(&requests);
        if let Some(success_pos) = matching.iter().position(|r| r.status == 200)
            && matching[..success_pos].iter().any(|r| r.status != 200)
        {
            saw_success_after_failure = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    handle.abort();

    assert!(
        saw_success_after_failure,
        "expected the stale notification to eventually receive a 200 response after a prior \
         failure, got requests={:?}",
        slack.captured_requests()
    );
}
