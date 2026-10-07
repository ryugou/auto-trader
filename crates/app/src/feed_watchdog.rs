//! Issue #109: ホストサスペンド検知 + フィード途絶監視 + フィードタスク終了検知。
//!
//! 2026-10-01〜10-03 にかけて本番ホスト (MacBook) がバッテリー切れ/蓋閉じで
//! 約2日間スリープし、売買プロセスが止まったが誰にも通知されなかった障害を
//! 受けて追加する。コード側で欠けていた3点:
//!
//! 1. プロセスが長時間止まっていたこと (ホストのサスペンド) を再開時に検知・通知。
//! 2. プロセスは動いているがフィードが更新されない状態の検知・通知。
//! 3. フィードタスクが `Ok`/`Err`/panic のいずれで終了しても気付ける通知。
//!
//! 判定ロジック (1, 2) は時計や `PriceStore` に依存しない純粋な関数/構造体
//! として切り出してあり、`run()` がそれを壁時計と `PriceStore::health_at`
//! に接続する薄い配線役を担う。3 は `spawn_feed_supervisor` が
//! `classify_feed_task_result` を呼んで配線する (監視タスクの tick ループとは
//! 独立したイベント駆動の仕組みのため)。`main.rs` はフィードタスクを spawn し、
//! その `JoinHandle` を `spawn_feed_supervisor` に渡すだけでよい。
//!
//! 自動再起動・自動補正は一切行わない — 通知のみ。
//!
//! Issue #109 followup: ホスト復帰直後は DNS/Wi-Fi が未復旧で通知の送信自体が
//! 失敗しうる。この4種の通知はいずれも低頻度・高重要度なので、1回失敗したら
//! 諦める `crate::spawn_notify` ではなく `crate::spawn_notify_with_retry` で
//! 指数バックオフ再送する (`[feed_watchdog].notify_retry_attempts` /
//! `notify_retry_initial_secs`)。

use auto_trader_core::config::FeedWatchdogConfig;
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_market::price_store::{FeedStatus, PriceStore};
use auto_trader_notify::{Notifier, NotifyEvent, ProcessAlertEvent, SystemAlertEvent};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;

/// JST はサマータイムが無いため固定オフセットで足りる (`risk_gate::jst_day_start`
/// と同じ考え方)。chrono-tz 等の追加依存は不要。
const JST_OFFSET_HOURS: i64 = 9;

fn to_jst_string(ts: DateTime<Utc>) -> String {
    let jst = ts + chrono::Duration::hours(JST_OFFSET_HOURS);
    format!("{} JST", jst.format("%Y-%m-%d %H:%M:%S"))
}

// ===========================================================================
// 1. ホストサスペンド検知
// ===========================================================================

/// 検知された1回のサスペンド (ホストスリープ等による長時間停止) の情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendGap {
    /// 停止が始まったとみなす時刻 (= 前回 tick の壁時計時刻)。
    pub started_at: DateTime<Utc>,
    /// 停止から復帰した (今回 tick の) 壁時計時刻。
    pub resumed_at: DateTime<Utc>,
    pub gap_secs: i64,
}

/// 壁時計が `previous` から `now` にかけて巻き戻っていれば、巻き戻った
/// 秒数 (正の値) を返す。巻き戻っていなければ `None`。
///
/// NTP 補正やシステムクロックの手動変更で壁時計が後退すると、
/// `detect_suspend_gap` の差分計算が負になり得る (サスペンドでも何でもない
/// のに巻き戻り量がそのまま `gap_secs` に紛れ込む)。`run()` はこの関数で
/// 巻き戻りを検知した回だけ判定一式をスキップし、`previous_wall_clock` の
/// 更新のみ行う。
pub fn wall_clock_backward_secs(previous: DateTime<Utc>, now: DateTime<Utc>) -> Option<i64> {
    if now < previous {
        Some((previous - now).num_seconds())
    } else {
        None
    }
}

/// 前回 tick と今回 tick の壁時計の差が `suspend_gap_secs` 以上なら
/// サスペンドとみなす。`tokio::time::interval` の単調時計はホストのサスペンド
/// 中に進まないことがあるため、経過時間の判定は必ず呼び出し側が
/// `chrono::Utc::now()` で取った壁時計の差分で行うこと (本関数はその差分を
/// 受け取るだけで、自身は時計を一切読まない)。
pub fn detect_suspend_gap(
    previous_wall_clock: DateTime<Utc>,
    now_wall_clock: DateTime<Utc>,
    suspend_gap_secs: u64,
) -> Option<SuspendGap> {
    let gap_secs = (now_wall_clock - previous_wall_clock).num_seconds();
    if gap_secs < suspend_gap_secs as i64 {
        return None;
    }
    Some(SuspendGap {
        started_at: previous_wall_clock,
        resumed_at: now_wall_clock,
        gap_secs,
    })
}

/// サスペンド通知の本文。停止時間 (分)・開始/終了時刻 (JST)・推定原因を含む。
///
/// `gap_secs` は前回 tick から今回 tick までの全区間で測った値であり、実際の
/// サスペンド開始時刻はその間のどこかなので、記録された値は実際の停止時間より
/// 最大で `check_interval_secs` 秒長く出うる。運用者が「正確に何分止まったか」
/// ではなく「概算」として読むよう、文言で明示する。
pub fn format_suspend_alert(gap: &SuspendGap, check_interval_secs: u64) -> String {
    let minutes = gap.gap_secs / 60;
    format!(
        "process appears to have been unresponsive for approximately {minutes} minute(s) \
         ({started} 〜 {resumed}; this duration can be up to {check_interval_secs}s longer than \
         the actual outage because it is measured between watchdog check rounds). \
         Likely cause: host sleep (lid closed / battery suspend) or the process itself was \
         stopped. Price feeds and SL/TP monitoring did not run during this window.",
        started = to_jst_string(gap.started_at),
        resumed = to_jst_string(gap.resumed_at),
    )
}

/// サスペンド検知後の再開直後、`stale_after_secs` の間は途絶判定を抑制する
/// ための猶予期間を追跡する。起動直後の猶予 (watchdog_started_at 基準) とは
/// 独立に、サスペンドの再開ごとに猶予を再設定する。
///
/// bitFlyer の WS 切断検知は最大 `HEARTBEAT_TIMEOUT` (120秒) かかるため、
/// サスペンド検知した「その回だけ」抑制すると次の回 (check_interval_secs 後)
/// に古い tick を見て誤って途絶→復旧を通知してしまう (Issue #109 item 4)。
#[derive(Debug, Default)]
pub struct SuspendGraceTracker {
    grace_until: Option<DateTime<Utc>>,
}

impl SuspendGraceTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// サスペンド検知時に呼ぶ。復帰時刻 (`resumed_at`) から `stale_after_secs`
    /// 後までを新しい猶予期間とする。
    pub fn note_suspend_resumed(&mut self, resumed_at: DateTime<Utc>, stale_after_secs: u64) {
        self.grace_until = Some(resumed_at + chrono::Duration::seconds(stale_after_secs as i64));
    }

    /// `now` が猶予期間内かどうか。
    pub fn in_grace_period(&self, now: DateTime<Utc>) -> bool {
        self.grace_until.is_some_and(|until| now < until)
    }
}

// ===========================================================================
// 2. フィード途絶監視
// ===========================================================================

/// フィードを一意に識別する ID。`auto_trader_core::types::Exchange`/`Pair` に
/// 依存させず文字列にしているのは、この状態機械を型変換抜きで単体テスト
/// できるようにするため (呼び出し側の `run()` で Exchange/Pair に変換する)。
pub type FeedId = (String, String);

/// 1 tick・1 フィード分の観測入力。`PriceStore` を直接見ず、呼び出し側が
/// `health_at()` の結果から組み立てる。
#[derive(Debug, Clone)]
pub struct FeedObservation {
    pub key: FeedId,
    pub market_closed: bool,
    /// 直近 tick のタイムスタンプ。1 件も tick が無ければ `None`。
    pub last_tick_at: Option<DateTime<Utc>>,
}

/// `status` が `FeedStatus::MarketClosed` でも、休場記録時刻
/// (`market_closed_at`) が `stale_after_secs` 以上前なら「現在も休場中」とは
/// みなさない。金曜の休場記録が残ったまま月曜以降もフィード取得に失敗し続ける
/// と、古い記録のせいで途絶通知が永久に抑制されてしまうため (Issue #109
/// item 2)。
fn is_effectively_market_closed(
    status_is_closed: bool,
    market_closed_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    stale_after_secs: u64,
) -> bool {
    status_is_closed
        && market_closed_at.is_some_and(|t| (now - t).num_seconds() < stale_after_secs as i64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StaleState {
    /// 途絶を最初に検知した時刻。
    since: DateTime<Utc>,
    /// 直近に通知した時刻 (re-alert 間隔の起点)。
    last_alert_at: DateTime<Utc>,
}

/// 1 回の判定結果。通知すべきなら `Some`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessEvent {
    /// 新規に途絶を検知した (このフィードについて初めての通知)。
    NewlyStale { since: DateTime<Utc> },
    /// 途絶が継続しており、再通知のタイミングが来た。
    StillStale { since: DateTime<Utc> },
    /// 途絶から復旧した。
    Recovered {
        since: DateTime<Utc>,
        recovered_at: DateTime<Utc>,
    },
}

/// 各フィードの途絶状態を保持する状態機械。`PriceStore` にも壁時計にも
/// 依存しないので、時刻を引数で渡すだけで単体テストできる。
#[derive(Debug, Default)]
pub struct FeedFreshnessTracker {
    states: HashMap<FeedId, StaleState>,
}

impl FeedFreshnessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// `now` 時点でフィード `obs` を評価し、通知すべき遷移があれば返す。
    ///
    /// - `watchdog_started_at`: 監視タスク自身の起動時刻。1 件も tick が無い
    ///   フィードについて「起動から stale_after_secs 経ったか」の基準にする。
    /// - `suppress`: true の間はこの回の判定そのものをスキップする (状態も
    ///   更新しない)。run() はサスペンド復帰後の猶予期間 (SuspendGraceTracker、
    ///   復帰時刻から stale_after_secs の間) 中に true を渡す。復帰直後は全
    ///   フィードが一様に古く見えるため、その間は途絶通知を出さない。
    pub fn evaluate(
        &mut self,
        obs: &FeedObservation,
        now: DateTime<Utc>,
        watchdog_started_at: DateTime<Utc>,
        stale_after_secs: u64,
        realert_interval_secs: u64,
        suppress: bool,
    ) -> Option<FreshnessEvent> {
        if suppress {
            return None;
        }

        if obs.market_closed {
            // 対象外。市場再開後に誤って「復旧」を通知しないよう、途絶状態を
            // 保持せず消しておく。
            self.states.remove(&obs.key);
            return None;
        }

        let is_stale_now = match obs.last_tick_at {
            Some(ts) => (now - ts).num_seconds() >= stale_after_secs as i64,
            None => (now - watchdog_started_at).num_seconds() >= stale_after_secs as i64,
        };

        match (self.states.get(&obs.key).copied(), is_stale_now) {
            (None, false) => None,
            (None, true) => {
                self.states.insert(
                    obs.key.clone(),
                    StaleState {
                        since: now,
                        last_alert_at: now,
                    },
                );
                Some(FreshnessEvent::NewlyStale { since: now })
            }
            (Some(state), true) => {
                if (now - state.last_alert_at).num_seconds() >= realert_interval_secs as i64 {
                    self.states.insert(
                        obs.key.clone(),
                        StaleState {
                            since: state.since,
                            last_alert_at: now,
                        },
                    );
                    Some(FreshnessEvent::StillStale { since: state.since })
                } else {
                    None
                }
            }
            (Some(state), false) => {
                self.states.remove(&obs.key);
                Some(FreshnessEvent::Recovered {
                    since: state.since,
                    recovered_at: now,
                })
            }
        }
    }
}

/// `since` は途絶を「検知した」時刻であり、実際に tick が止まった時刻ではない
/// (最終 tick はそれより少なくとも stale_after_secs 前)。運用者が停止開始時刻を
/// 誤認しないよう、最終 tick 時刻 (無ければ監視開始時刻) を併記する。
fn format_stale_alert(
    exchange: Exchange,
    pair: &Pair,
    since: DateTime<Utc>,
    last_tick_at: Option<DateTime<Utc>>,
    watchdog_started_at: DateTime<Utc>,
) -> String {
    let last_tick = match last_tick_at {
        Some(ts) => format!("last tick at {}", to_jst_string(ts)),
        None => format!(
            "no tick received since watchdog start ({})",
            to_jst_string(watchdog_started_at)
        ),
    };
    format!(
        "feed {} {} has stopped receiving ticks ({last_tick}; detected stale at {}). \
         Price-based monitoring (SL/TP, signal generation) may be blind for this feed.",
        exchange.as_str(),
        pair,
        to_jst_string(since)
    )
}

fn format_recovered_alert(
    exchange: Exchange,
    pair: &Pair,
    since: DateTime<Utc>,
    recovered_at: DateTime<Utc>,
) -> String {
    // 起点は途絶の検知時刻であり、実際の途絶時間より短く出る点を文言で明示する。
    let minutes_since_detection = (recovered_at - since).num_seconds().max(0) / 60;
    format!(
        "feed {} {} recovered at {} (stale detected at {}, ~{minutes_since_detection} \
         minute(s) after detection; actual outage began earlier).",
        exchange.as_str(),
        pair,
        to_jst_string(recovered_at),
        to_jst_string(since)
    )
}

// ===========================================================================
// 3. フィードタスク終了検知
// ===========================================================================

enum FeedExitKind {
    /// `feed.run()` が `Ok(())` を返した (長時間稼働するフィードとしては想定外)。
    Normal,
    Error(String),
    Panic(String),
}

fn format_feed_exit_alert(exchange: Exchange, kind: &FeedExitKind) -> (String, String) {
    let title = "feed task exited".to_string();
    let body = match kind {
        FeedExitKind::Normal => format!(
            "market feed for {} exited normally (returned Ok) — unexpected for a \
             long-running feed. It will NOT auto-restart; the feed is now down.",
            exchange.as_str()
        ),
        FeedExitKind::Error(e) => format!(
            "market feed for {} exited with an error — it will NOT auto-restart. error: {e}",
            exchange.as_str()
        ),
        FeedExitKind::Panic(msg) => format!(
            "market feed for {} PANICKED — it will NOT auto-restart. panic: {msg}",
            exchange.as_str()
        ),
    };
    (title, body)
}

/// panic payload (`Box<dyn Any + Send>`) から人間可読なメッセージを取り出す。
/// Rust の panic! マクロは大抵 `&str` か `String` を積むので、そのどちらでも
/// なければ素直に「非文字列」と報告する (ここで推測で中身を捏造しない)。
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic payload was not a string".to_string()
    }
}

/// `tokio::spawn(feed.run(...))` の `JoinHandle` を await した結果を分類し、
/// 通知すべきなら `(title, body)` を返す。
///
/// シャットダウン時の `AbortHandle::abort()` による終了
/// (`JoinError::is_cancelled() == true`) は障害ではなく既存の正常系なので
/// `None` を返す — これにより main.rs 側の shutdown 経路を変更せずに
/// 「abort では通知しない」要件を満たす。
pub fn classify_feed_task_result(
    exchange: Exchange,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> Option<(String, String)> {
    match result {
        Ok(Ok(())) => Some(format_feed_exit_alert(exchange, &FeedExitKind::Normal)),
        Ok(Err(e)) => Some(format_feed_exit_alert(
            exchange,
            &FeedExitKind::Error(e.to_string()),
        )),
        Err(join_err) if join_err.is_cancelled() => None,
        Err(join_err) => {
            // tokio::task::JoinError is either "cancelled" or "panic"; cancelled
            // is handled above, so reaching here always means a panic.
            let msg = panic_message(join_err.into_panic());
            Some(format_feed_exit_alert(exchange, &FeedExitKind::Panic(msg)))
        }
    }
}

// ===========================================================================
// 配線: 監視タスク本体
// ===========================================================================

/// `run()` が参照する読み取り専用の環境。main.rs の起動シーケンスから
/// 1 回構築して `tokio::spawn(run(ctx))` に渡す。
pub struct FeedWatchdogContext {
    pub price_store: Arc<PriceStore>,
    pub notifier: Arc<Notifier>,
    pub config: FeedWatchdogConfig,
}

/// `tokio::spawn(feed.run(...))` の `JoinHandle` を見張り、Ok/Err/panic で
/// 終了した場合に通知する supervisor を spawn する。shutdown 時の
/// `AbortHandle::abort()` による終了は `classify_feed_task_result` が判別して
/// 通知しない。呼び出し側 (main.rs) は、戻り値の `AbortHandle` を保持して
/// shutdown 時に feed 本体を止める。
///
/// 監視ロジックを main.rs のフィード起動ループから分離して、ここに集約して
/// いる。`exclude_exchanges` (item 1) はこの終了
/// 検知には関係しない — 鮮度監視の対象外にしたフィードでも、タスク自体が
/// 落ちたことは運用者に知らせる必要があるため。
///
/// `notify_retry_attempts` / `notify_retry_initial_secs` は
/// `crate::spawn_notify_with_retry` にそのまま渡す
/// (`[feed_watchdog].notify_retry_attempts` / `notify_retry_initial_secs`)。
pub fn spawn_feed_supervisor(
    exchange: Exchange,
    notifier: Arc<Notifier>,
    inner_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
    notify_retry_attempts: u64,
    notify_retry_initial_secs: u64,
) -> tokio::task::AbortHandle {
    let abort_handle = inner_handle.abort_handle();
    tokio::spawn(async move {
        let result = inner_handle.await;
        if let Some((title, body)) = classify_feed_task_result(exchange, result) {
            tracing::error!("market feed for {:?} terminated: {body}", exchange);
            crate::spawn_notify_with_retry(
                &notifier,
                NotifyEvent::SystemAlert(SystemAlertEvent {
                    title,
                    account_name: "(system)".to_string(),
                    exchange,
                    body,
                }),
                notify_retry_attempts,
                notify_retry_initial_secs,
            );
        }
    });
    abort_handle
}

/// フィード監視タスク本体。`check_interval_secs` ごとに:
///   1. 壁時計の tick 間隔からホストサスペンドを検知 (検知したら通知)。
///   2. `PriceStore::health_at` から各フィードの途絶/復旧を判定 (該当すれば通知)。
///
/// 呼び出し側 (main.rs) が `config.feed_watchdog.enabled` を見て spawn するか
/// どうかを決める (無効時はそもそも spawn しない — 他の optional task と同じ
/// パターン)。本関数はループを抜けない。
pub async fn run(ctx: FeedWatchdogContext) {
    tracing::info!(
        "feed watchdog started (check_interval={}s suspend_gap={}s stale_after={}s realert={}s)",
        ctx.config.check_interval_secs,
        ctx.config.suspend_gap_secs,
        ctx.config.stale_after_secs,
        ctx.config.realert_interval_secs,
    );
    tracing::info!(
        "feed watchdog: excluding {:?} from freshness monitoring",
        ctx.config.exclude_exchanges
    );

    let watchdog_started_at = Utc::now();
    let mut previous_wall_clock = watchdog_started_at;
    let mut freshness = FeedFreshnessTracker::new();
    let mut suspend_grace = SuspendGraceTracker::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        ctx.config.check_interval_secs,
    ));
    // 既定の Burst だと、復帰時に取りこぼした tick が連続発火し、同じ復帰について
    // 短時間に何度も判定ラウンドが走る。Delay なら復帰後の次 tick を
    // check_interval 後に揃えられる。
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;
        let now = Utc::now();

        if let Some(diff_secs) = wall_clock_backward_secs(previous_wall_clock, now) {
            tracing::warn!(
                "feed watchdog: wall clock went backwards by {diff_secs}s, skipping this round's checks"
            );
            previous_wall_clock = now;
            continue;
        }

        let suspend_gap = detect_suspend_gap(previous_wall_clock, now, ctx.config.suspend_gap_secs);
        previous_wall_clock = now;
        if let Some(gap) = &suspend_gap {
            suspend_grace.note_suspend_resumed(now, ctx.config.stale_after_secs);
            let body = format_suspend_alert(gap, ctx.config.check_interval_secs);
            tracing::error!("feed watchdog: suspend detected: {body}");
            crate::spawn_notify_with_retry(
                &ctx.notifier,
                NotifyEvent::ProcessAlert(ProcessAlertEvent {
                    title: "process suspend detected".to_string(),
                    body,
                }),
                ctx.config.notify_retry_attempts,
                ctx.config.notify_retry_initial_secs,
            );
        }

        let health = ctx.price_store.health_at(now).await;
        for h in &health {
            if ctx
                .config
                .exclude_exchanges
                .iter()
                .any(|e| e == &h.exchange)
            {
                // 鮮度監視の対象外 (例: OANDA)。フィードタスク終了検知は
                // spawn_feed_supervisor が別経路で常に行うので影響しない。
                continue;
            }
            let obs = FeedObservation {
                key: (h.exchange.clone(), h.pair.clone()),
                market_closed: is_effectively_market_closed(
                    h.status == FeedStatus::MarketClosed,
                    h.market_closed_at,
                    now,
                    ctx.config.stale_after_secs,
                ),
                last_tick_at: h
                    .last_tick_age_secs
                    .map(|age| now - chrono::Duration::seconds(age)),
            };
            let Some(event) = freshness.evaluate(
                &obs,
                now,
                watchdog_started_at,
                ctx.config.stale_after_secs,
                ctx.config.realert_interval_secs,
                suspend_grace.in_grace_period(now),
            ) else {
                continue;
            };
            let exchange = match h.exchange.parse::<Exchange>() {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(
                        "feed watchdog: unknown exchange '{}' in health report, skipping notify: {e}",
                        h.exchange
                    );
                    continue;
                }
            };
            let pair = Pair::new(&h.pair);
            let (title, body, log_level_is_error) = match event {
                FreshnessEvent::NewlyStale { since } | FreshnessEvent::StillStale { since } => (
                    "feed stale".to_string(),
                    format_stale_alert(
                        exchange,
                        &pair,
                        since,
                        obs.last_tick_at,
                        watchdog_started_at,
                    ),
                    true,
                ),
                FreshnessEvent::Recovered {
                    since,
                    recovered_at,
                } => (
                    "feed recovered".to_string(),
                    format_recovered_alert(exchange, &pair, since, recovered_at),
                    false,
                ),
            };
            if log_level_is_error {
                tracing::error!("feed watchdog: {body}");
            } else {
                tracing::info!("feed watchdog: {body}");
            }
            crate::spawn_notify_with_retry(
                &ctx.notifier,
                NotifyEvent::SystemAlert(SystemAlertEvent {
                    title,
                    account_name: "(system)".to_string(),
                    exchange,
                    body,
                }),
                ctx.config.notify_retry_attempts,
                ctx.config.notify_retry_initial_secs,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(secs_from_epoch: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs_from_epoch, 0)
            .unwrap()
    }

    fn feed(exchange: &str, pair: &str) -> FeedId {
        (exchange.to_string(), pair.to_string())
    }

    // ----- 1. サスペンド検知 -----

    #[test]
    fn suspend_not_detected_below_threshold() {
        let prev = t(0);
        let now = t(299);
        assert_eq!(detect_suspend_gap(prev, now, 300), None);
    }

    #[test]
    fn suspend_detected_exactly_at_threshold() {
        let prev = t(0);
        let now = t(300);
        let gap = detect_suspend_gap(prev, now, 300).expect("boundary must detect");
        assert_eq!(gap.gap_secs, 300);
        assert_eq!(gap.started_at, prev);
        assert_eq!(gap.resumed_at, now);
    }

    #[test]
    fn suspend_detected_above_threshold() {
        let prev = t(0);
        let now = t(2 * 24 * 3600 + 3540); // 約2日分 (issue の実障害相当)
        let gap = detect_suspend_gap(prev, now, 300).expect("long gap must detect");
        assert_eq!(gap.gap_secs, 2 * 24 * 3600 + 3540);
    }

    #[test]
    fn suspend_alert_body_contains_duration_and_jst_timestamps() {
        let gap = SuspendGap {
            started_at: Utc.with_ymd_and_hms(2026, 10, 1, 6, 49, 0).unwrap(), // 15:49 JST
            resumed_at: Utc.with_ymd_and_hms(2026, 10, 3, 5, 58, 0).unwrap(), // 14:58 JST
            gap_secs: 169_740,                                                // 2829 min
        };
        let check_interval_secs = 60;
        let body = format_suspend_alert(&gap, check_interval_secs);
        assert!(body.contains("approximately 2829 minute"), "body={body}");
        assert!(body.contains("2026-10-01 15:49:00 JST"), "body={body}");
        assert!(body.contains("2026-10-03 14:58:00 JST"), "body={body}");
        assert!(
            body.contains(&format!("up to {check_interval_secs}s longer")),
            "body must explain the check-interval measurement error margin: {body}"
        );
        assert!(
            body.to_lowercase().contains("sleep") || body.to_lowercase().contains("suspend"),
            "body must mention host sleep/suspend possibility: {body}"
        );
    }

    // ----- 0. 壁時計巻き戻り検出 -----

    #[test]
    fn wall_clock_backward_secs_none_when_now_equals_previous() {
        assert_eq!(wall_clock_backward_secs(t(0), t(0)), None);
    }

    #[test]
    fn wall_clock_backward_secs_none_when_now_after_previous() {
        assert_eq!(wall_clock_backward_secs(t(0), t(1)), None);
    }

    #[test]
    fn wall_clock_backward_secs_some_when_now_before_previous() {
        assert_eq!(wall_clock_backward_secs(t(100), t(40)), Some(60));
    }

    // ----- 2a. 休場記録の鮮度判定 (is_effectively_market_closed) -----

    #[test]
    fn effectively_market_closed_true_when_recently_recorded() {
        let now = t(1000);
        let closed_at = now - chrono::Duration::seconds(10);
        assert!(is_effectively_market_closed(
            true,
            Some(closed_at),
            now,
            STALE_AFTER
        ));
    }

    #[test]
    fn effectively_market_closed_false_when_record_too_old() {
        let now = t(1000);
        let closed_at = now - chrono::Duration::seconds(STALE_AFTER as i64);
        assert!(!is_effectively_market_closed(
            true,
            Some(closed_at),
            now,
            STALE_AFTER
        ));
    }

    #[test]
    fn effectively_market_closed_false_when_status_is_not_closed() {
        let now = t(1000);
        assert!(!is_effectively_market_closed(
            false,
            Some(now),
            now,
            STALE_AFTER
        ));
    }

    #[test]
    fn effectively_market_closed_false_when_no_record() {
        let now = t(1000);
        assert!(!is_effectively_market_closed(true, None, now, STALE_AFTER));
    }

    // ----- 2b. サスペンド復帰後の猶予期間 (SuspendGraceTracker) -----

    #[test]
    fn suspend_grace_tracker_in_grace_immediately_after_resume() {
        let mut tracker = SuspendGraceTracker::new();
        let resumed_at = t(0);
        tracker.note_suspend_resumed(resumed_at, STALE_AFTER);
        assert!(tracker.in_grace_period(resumed_at));
    }

    #[test]
    fn suspend_grace_tracker_grace_ends_at_stale_after_secs() {
        let mut tracker = SuspendGraceTracker::new();
        let resumed_at = t(0);
        tracker.note_suspend_resumed(resumed_at, STALE_AFTER);
        let just_before = resumed_at + chrono::Duration::seconds(STALE_AFTER as i64 - 1);
        assert!(tracker.in_grace_period(just_before));
        let at_boundary = resumed_at + chrono::Duration::seconds(STALE_AFTER as i64);
        assert!(!tracker.in_grace_period(at_boundary));
    }

    #[test]
    fn suspend_grace_tracker_with_no_suspend_never_in_grace() {
        let tracker = SuspendGraceTracker::new();
        assert!(!tracker.in_grace_period(t(0)));
    }

    #[test]
    fn suspend_grace_tracker_latest_call_overrides_previous_grace() {
        let mut tracker = SuspendGraceTracker::new();
        tracker.note_suspend_resumed(t(0), STALE_AFTER);
        // 2回目のサスペンド検知で猶予が再設定される。
        let second_resume = t(10);
        tracker.note_suspend_resumed(second_resume, STALE_AFTER);
        let after_first_windows_original_end = t(0) + chrono::Duration::seconds(STALE_AFTER as i64);
        assert!(
            tracker.in_grace_period(after_first_windows_original_end),
            "second note_suspend_resumed should extend grace beyond the first window"
        );
    }

    // ----- 2. フィード途絶の状態遷移 -----

    const STALE_AFTER: u64 = 600;
    const REALERT: u64 = 3600;

    fn obs(
        key: FeedId,
        market_closed: bool,
        last_tick_at: Option<DateTime<Utc>>,
    ) -> FeedObservation {
        FeedObservation {
            key,
            market_closed,
            last_tick_at,
        }
    }

    #[test]
    fn fresh_feed_produces_no_event() {
        let mut tracker = FeedFreshnessTracker::new();
        let now = t(1000);
        let o = obs(
            feed("gmo_fx", "USD_JPY"),
            false,
            Some(now - chrono::Duration::seconds(10)),
        );
        let ev = tracker.evaluate(&o, now, t(0), STALE_AFTER, REALERT, false);
        assert_eq!(ev, None);
    }

    #[test]
    fn startup_grace_period_suppresses_missing_tick_alert() {
        // 起動直後 (watchdog_started_at == now 相当) はまだ stale_after_secs
        // 経っていないので、1件も tick が無くても途絶扱いしない。
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let now = started + chrono::Duration::seconds(STALE_AFTER as i64 - 1);
        let o = obs(feed("gmo_fx", "USD_JPY"), false, None);
        let ev = tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false);
        assert_eq!(ev, None);
    }

    #[test]
    fn no_tick_since_startup_becomes_stale_at_threshold() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let now = started + chrono::Duration::seconds(STALE_AFTER as i64);
        let o = obs(feed("gmo_fx", "USD_JPY"), false, None);
        let ev = tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false);
        assert_eq!(ev, Some(FreshnessEvent::NewlyStale { since: now }));
    }

    #[test]
    fn fresh_to_stale_transition_fires_once() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let last_tick = t(0);
        let now_stale = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
        let o = obs(feed("bitflyer_cfd", "FX_BTC_JPY"), false, Some(last_tick));

        let first = tracker.evaluate(&o, now_stale, started, STALE_AFTER, REALERT, false);
        assert_eq!(first, Some(FreshnessEvent::NewlyStale { since: now_stale }));

        // 次の tick でも last_tick_at が同じまま (更新が無い) → まだ再通知間隔
        // 未満なら通知しない。
        let soon_after = now_stale + chrono::Duration::seconds(60);
        let second = tracker.evaluate(&o, soon_after, started, STALE_AFTER, REALERT, false);
        assert_eq!(
            second, None,
            "must not re-fire before realert_interval_secs elapses"
        );
    }

    #[test]
    fn realert_fires_after_interval_elapses_while_still_stale() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let last_tick = t(0);
        let o = obs(feed("bitflyer_cfd", "FX_BTC_JPY"), false, Some(last_tick));

        let first_detect = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
        tracker
            .evaluate(&o, first_detect, started, STALE_AFTER, REALERT, false)
            .expect("first detection must fire");

        let before_realert = first_detect + chrono::Duration::seconds(REALERT as i64 - 1);
        assert_eq!(
            tracker.evaluate(&o, before_realert, started, STALE_AFTER, REALERT, false),
            None
        );

        let at_realert = first_detect + chrono::Duration::seconds(REALERT as i64);
        assert_eq!(
            tracker.evaluate(&o, at_realert, started, STALE_AFTER, REALERT, false),
            Some(FreshnessEvent::StillStale {
                since: first_detect
            })
        );
    }

    #[test]
    fn recovery_fires_once_and_resets_state() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let last_tick = t(0);
        let key = feed("bitflyer_cfd", "FX_BTC_JPY");

        let stale_obs = obs(key.clone(), false, Some(last_tick));
        let detect_at = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
        tracker
            .evaluate(&stale_obs, detect_at, started, STALE_AFTER, REALERT, false)
            .expect("must detect stale first");

        // 新しい tick が来た (last_tick_at が更新され、もう古くない)。
        let recovered_at = detect_at + chrono::Duration::seconds(30);
        let fresh_obs = obs(key.clone(), false, Some(recovered_at));
        let ev = tracker.evaluate(
            &fresh_obs,
            recovered_at,
            started,
            STALE_AFTER,
            REALERT,
            false,
        );
        assert_eq!(
            ev,
            Some(FreshnessEvent::Recovered {
                since: detect_at,
                recovered_at
            })
        );

        // 状態が Fresh に戻っているので、直後に再評価しても通知は出ない。
        let after = recovered_at + chrono::Duration::seconds(1);
        let still_fresh_obs = obs(key, false, Some(recovered_at));
        assert_eq!(
            tracker.evaluate(
                &still_fresh_obs,
                after,
                started,
                STALE_AFTER,
                REALERT,
                false
            ),
            None
        );
    }

    #[test]
    fn market_closed_feed_is_never_flagged_stale() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        // 長時間 tick が無くても market_closed なら対象外。
        let now = started + chrono::Duration::seconds(STALE_AFTER as i64 * 10);
        let o = obs(feed("gmo_fx", "USD_JPY"), true, None);
        assert_eq!(
            tracker.evaluate(&o, now, started, STALE_AFTER, REALERT, false),
            None
        );
    }

    #[test]
    fn market_closed_clears_prior_stale_state_without_recovered_notification() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let key = feed("gmo_fx", "USD_JPY");
        let last_tick = t(0);
        let stale_obs = obs(key.clone(), false, Some(last_tick));
        let detect_at = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
        tracker
            .evaluate(&stale_obs, detect_at, started, STALE_AFTER, REALERT, false)
            .expect("must detect stale first");

        // 市場close に切り替わった → 通知無しで静かに状態クリア。
        let closed_at = detect_at + chrono::Duration::seconds(10);
        let closed_obs = obs(key, true, Some(last_tick));
        assert_eq!(
            tracker.evaluate(&closed_obs, closed_at, started, STALE_AFTER, REALERT, false),
            None,
            "market_closed transition itself must not fire a 'recovered' notification"
        );
    }

    #[test]
    fn suspend_round_suppresses_new_stale_notification_but_resumes_next_round() {
        let mut tracker = FeedFreshnessTracker::new();
        let started = t(0);
        let key = feed("gmo_fx", "USD_JPY");
        let last_tick = t(0);
        let o = obs(key, false, Some(last_tick));

        // ラウンド1: サスペンド検知した回。本来なら途絶のはずだが抑制される。
        let round1 = last_tick + chrono::Duration::seconds(STALE_AFTER as i64);
        let ev1 = tracker.evaluate(&o, round1, started, STALE_AFTER, REALERT, true);
        assert_eq!(
            ev1, None,
            "suspend round must suppress the stale notification"
        );

        // (run() では猶予期間中 suppress=true が続く。ここは evaluate 単体の挙動確認。)
        // ラウンド2: 抑制フラグ無し。state は未更新のまま (round1 で評価して
        // いないので Fresh 扱い) → ここで初めて NewlyStale が発火する。
        let round2 = round1 + chrono::Duration::seconds(60);
        let ev2 = tracker.evaluate(&o, round2, started, STALE_AFTER, REALERT, false);
        assert_eq!(ev2, Some(FreshnessEvent::NewlyStale { since: round2 }));
    }

    // ----- 2b. 通知本文の整形 -----

    #[test]
    fn stale_alert_with_tick_shows_last_tick_and_detection_time() {
        // t(0) = 2023-11-14 22:13:20 UTC = 2023-11-15 07:13:20 JST
        let body = format_stale_alert(
            Exchange::GmoFx,
            &Pair::new("USD_JPY"),
            t(600),
            Some(t(0)),
            t(-3600),
        );
        assert!(body.contains("gmo_fx"), "body={body}");
        assert!(
            body.contains("last tick at 2023-11-15 07:13:20 JST"),
            "body={body}"
        );
        assert!(
            body.contains("detected stale at 2023-11-15 07:23:20 JST"),
            "body={body}"
        );
        assert!(!body.contains("no tick received"), "body={body}");
    }

    #[test]
    fn stale_alert_without_tick_shows_watchdog_start() {
        let body = format_stale_alert(Exchange::GmoFx, &Pair::new("USD_JPY"), t(600), None, t(0));
        assert!(
            body.contains("no tick received since watchdog start (2023-11-15 07:13:20 JST)"),
            "body={body}"
        );
        assert!(
            body.contains("detected stale at 2023-11-15 07:23:20 JST"),
            "body={body}"
        );
        assert!(!body.contains("last tick at"), "body={body}");
    }

    #[test]
    fn recovered_alert_states_times_relative_to_detection() {
        let body = format_recovered_alert(Exchange::GmoFx, &Pair::new("USD_JPY"), t(0), t(185));
        assert!(
            body.contains("recovered at 2023-11-15 07:16:25 JST"),
            "body={body}"
        );
        assert!(
            body.contains("stale detected at 2023-11-15 07:13:20 JST"),
            "body={body}"
        );
        assert!(body.contains("~3 minute(s) after detection"), "body={body}");
    }

    // ----- 3. フィードタスク終了検知 -----

    #[tokio::test]
    async fn classify_reports_normal_exit() {
        let handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async { Ok(()) });
        let result = handle.await;
        let (title, body) = classify_feed_task_result(Exchange::BitflyerCfd, result)
            .expect("normal exit must notify");
        assert_eq!(title, "feed task exited");
        assert!(body.contains("bitflyer_cfd"), "body={body}");
        assert!(body.contains("normally"), "body={body}");
    }

    #[tokio::test]
    async fn classify_reports_error_exit() {
        let handle: tokio::task::JoinHandle<anyhow::Result<()>> =
            tokio::spawn(async { Err(anyhow::anyhow!("ws disconnected")) });
        let result = handle.await;
        let (title, body) =
            classify_feed_task_result(Exchange::Oanda, result).expect("error exit must notify");
        assert_eq!(title, "feed task exited");
        assert!(body.contains("oanda"), "body={body}");
        assert!(body.contains("ws disconnected"), "body={body}");
    }

    #[tokio::test]
    async fn classify_reports_panic_with_message() {
        let handle: tokio::task::JoinHandle<anyhow::Result<()>> =
            tokio::spawn(async { panic!("boom") });
        let result = handle.await;
        let (title, body) =
            classify_feed_task_result(Exchange::GmoFx, result).expect("panic must notify");
        assert_eq!(title, "feed task exited");
        assert!(body.contains("gmo_fx"), "body={body}");
        assert!(body.contains("boom"), "body={body}");
    }

    #[tokio::test]
    async fn classify_returns_none_for_shutdown_abort() {
        let handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async {
            std::future::pending::<()>().await;
            #[allow(unreachable_code)]
            Ok(())
        });
        handle.abort();
        let result = handle.await;
        assert!(result.as_ref().is_err_and(|e| e.is_cancelled()));
        assert_eq!(classify_feed_task_result(Exchange::GmoFx, result), None);
    }

    #[test]
    fn panic_message_extracts_str_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("plain str panic");
        assert_eq!(panic_message(payload), "plain str panic");
    }

    #[test]
    fn panic_message_extracts_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(String::from("owned string panic"));
        assert_eq!(panic_message(payload), "owned string panic");
    }

    #[test]
    fn panic_message_falls_back_for_non_string_payload() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42_i32);
        assert_eq!(panic_message(payload), "panic payload was not a string");
    }
}
