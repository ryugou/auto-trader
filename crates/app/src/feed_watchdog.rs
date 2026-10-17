use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::config::FeedWatchdogConfig;
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_market::price_store::{FeedHealth, FeedStatus, PriceStore};
use auto_trader_notify::{Notifier, NotifyEvent, ProcessAlertEvent, SystemAlertEvent};
use chrono::{DateTime, Utc};

const JST_OFFSET_HOURS: i64 = 9;
const MISSED_TICK_BEHAVIOR_WITHOUT_BURST_AFTER_RESUME: tokio::time::MissedTickBehavior =
    tokio::time::MissedTickBehavior::Delay;

fn to_jst_string(ts: DateTime<Utc>) -> String {
    let jst = ts + chrono::Duration::hours(JST_OFFSET_HOURS);
    format!("{} JST", jst.format("%Y-%m-%d %H:%M:%S"))
}

/// Period in which the process appears to have been suspended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspendGap {
    /// Wall-clock time of the check before the gap.
    pub started_at: DateTime<Utc>,
    /// Wall-clock time of the check that found the gap.
    pub resumed_at: DateTime<Utc>,
    /// Length of the gap in seconds.
    pub gap_secs: i64,
}

/// Returns how many seconds `now` is before `previous`, or `None` if it is not before.
pub fn wall_clock_backward_secs(previous: DateTime<Utc>, now: DateTime<Utc>) -> Option<i64> {
    if now < previous {
        Some((previous - now).num_seconds())
    } else {
        None
    }
}

/// Returns the gap between `previous_wall_clock` and `now_wall_clock` if it is at least `suspend_gap_secs` seconds, or `None` if it is shorter.
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

/// Returns the alert text for `gap`, with its length in minutes, its times in JST and a note that it can exceed the outage by up to `check_interval_secs` seconds.
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

/// Tracker of the grace period after a suspend, in which stale feeds are not reported.
#[derive(Debug, Default)]
pub struct SuspendGraceTracker {
    grace_until: Option<DateTime<Utc>>,
}

impl SuspendGraceTracker {
    /// Creates a tracker that has no grace period.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a grace period at `resumed_at` that lasts `stale_after_secs` seconds.
    pub fn note_suspend_resumed(&mut self, resumed_at: DateTime<Utc>, stale_after_secs: u64) {
        self.grace_until = Some(resumed_at + chrono::Duration::seconds(stale_after_secs as i64));
    }

    /// Returns whether `now` is before the end of the grace period.
    pub fn in_grace_period(&self, now: DateTime<Utc>) -> bool {
        self.grace_until.is_some_and(|until| now < until)
    }
}

/// Exchange name and pair name that identify a feed.
pub type FeedId = (String, String);

/// State of one feed at a check.
#[derive(Debug, Clone)]
pub struct FeedObservation {
    /// Identifier of the feed.
    pub key: FeedId,
    /// Whether the market of the feed counts as closed.
    pub market_closed: bool,
    /// Time of the latest tick of the feed, or `None` if it has none.
    pub last_tick_at: Option<DateTime<Utc>>,
}

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
    since: DateTime<Utc>,
    last_alert_at: DateTime<Utc>,
}

/// Change in the staleness of a feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessEvent {
    /// The feed became stale.
    NewlyStale {
        /// Time at which the feed was found stale.
        since: DateTime<Utc>,
    },
    /// The feed is still stale and the re-alert interval has passed.
    StillStale {
        /// Time at which the feed was found stale.
        since: DateTime<Utc>,
    },
    /// The feed recovered.
    Recovered {
        /// Time at which the feed was found stale.
        since: DateTime<Utc>,
        /// Time at which the recovery was found.
        recovered_at: DateTime<Utc>,
    },
}

/// Tracker of the feeds that are stale.
#[derive(Debug, Default)]
pub struct FeedFreshnessTracker {
    states: HashMap<FeedId, StaleState>,
}

impl FeedFreshnessTracker {
    /// Creates a tracker with no stale feed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `None` without changing any state if `suppress` is true; otherwise updates the state of `obs` and returns the change, or `None` if there is none. A feed whose market is closed is cleared. A feed is stale if its latest tick, or the start of the watchdog if it has none, is `stale_after_secs` seconds old or older. A feed that becomes stale gives `NewlyStale`, a stale feed gives `StillStale` once `realert_interval_secs` seconds have passed since its last alert, and a stale feed that is no longer stale gives `Recovered`.
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

enum FeedExitKind {
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

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic payload was not a string".to_string()
    }
}

/// Returns the alert title and body for the result of a feed task, or `None` if the task was canceled. A task that returned `Ok`, returned an error or panicked each gives its own body.
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
            let msg = panic_message(join_err.into_panic());
            Some(format_feed_exit_alert(exchange, &FeedExitKind::Panic(msg)))
        }
    }
}

/// Dependencies of the feed watchdog.
pub struct FeedWatchdogContext {
    /// Store of the latest ticks of the feeds.
    pub price_store: Arc<PriceStore>,
    /// Notifier that the alerts are sent through.
    pub notifier: Arc<Notifier>,
    /// Settings of the watchdog.
    pub config: FeedWatchdogConfig,
}

/// Spawns a task that waits for `inner_handle` and, if the feed task did not end by cancellation, logs an error and sends a system alert about it with retries; returns the handle that aborts the feed task.
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
            tracing::error!(exchange = %exchange, body = %body, "market feed terminated");
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

/// Runs the watchdog until the task is aborted: at each interval it skips the round if the wall clock went backwards, alerts and starts a grace period if it finds a suspend gap, and then sends an alert for each feed of a non-excluded exchange whose staleness changed.
pub async fn run(ctx: FeedWatchdogContext) {
    log_watchdog_started(&ctx.config);

    let watchdog_started_at = Utc::now();
    let mut previous_wall_clock = watchdog_started_at;
    let mut freshness = FeedFreshnessTracker::new();
    let mut suspend_grace = SuspendGraceTracker::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
        ctx.config.check_interval_secs,
    ));
    interval.set_missed_tick_behavior(MISSED_TICK_BEHAVIOR_WITHOUT_BURST_AFTER_RESUME);

    loop {
        interval.tick().await;
        let now = Utc::now();

        if let Some(diff_secs) = wall_clock_backward_secs(previous_wall_clock, now) {
            tracing::warn!(
                diff_secs = diff_secs,
                "wall clock went backwards, skipping this round's checks"
            );
            previous_wall_clock = now;
            continue;
        }

        let suspend_gap = detect_suspend_gap(previous_wall_clock, now, ctx.config.suspend_gap_secs);
        previous_wall_clock = now;
        if let Some(gap) = &suspend_gap {
            alert_suspend_resumed(&ctx, &mut suspend_grace, gap, now);
        }

        check_feed_freshness(
            &ctx,
            &mut freshness,
            &suspend_grace,
            now,
            watchdog_started_at,
        )
        .await;
    }
}

fn log_watchdog_started(config: &FeedWatchdogConfig) {
    tracing::info!(
        check_interval_secs = config.check_interval_secs,
        suspend_gap_secs = config.suspend_gap_secs,
        stale_after_secs = config.stale_after_secs,
        realert_interval_secs = config.realert_interval_secs,
        "feed watchdog started"
    );
    tracing::info!(
        exclude_exchanges = ?config.exclude_exchanges,
        "excluding exchanges from freshness monitoring"
    );
}

fn alert_suspend_resumed(
    ctx: &FeedWatchdogContext,
    suspend_grace: &mut SuspendGraceTracker,
    gap: &SuspendGap,
    now: DateTime<Utc>,
) {
    suspend_grace.note_suspend_resumed(now, ctx.config.stale_after_secs);
    let body = format_suspend_alert(gap, ctx.config.check_interval_secs);
    tracing::error!(body = %body, "suspend detected");
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

async fn check_feed_freshness(
    ctx: &FeedWatchdogContext,
    freshness: &mut FeedFreshnessTracker,
    suspend_grace: &SuspendGraceTracker,
    now: DateTime<Utc>,
    watchdog_started_at: DateTime<Utc>,
) {
    let health = ctx.price_store.health_at(now).await;
    for h in health
        .iter()
        .filter(|h| !ctx.config.exclude_exchanges.contains(&h.exchange))
    {
        let obs = observe_feed(h, now, ctx.config.stale_after_secs);
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
        let Some(exchange) = parse_feed_exchange(h) else {
            continue;
        };
        let pair = Pair::new(&h.pair);
        let (title, body, log_level_is_error) = build_freshness_alert(
            exchange,
            &pair,
            event,
            obs.last_tick_at,
            watchdog_started_at,
        );
        if log_level_is_error {
            tracing::error!(title = %title, body = %body, "feed freshness alert");
        } else {
            tracing::info!(title = %title, body = %body, "feed freshness alert");
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

fn parse_feed_exchange(h: &FeedHealth) -> Option<Exchange> {
    match h.exchange.parse::<Exchange>() {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::warn!(
                exchange = %h.exchange,
                error = %e,
                "failed to parse exchange, skipping notify"
            );
            None
        }
    }
}

fn observe_feed(h: &FeedHealth, now: DateTime<Utc>, stale_after_secs: u64) -> FeedObservation {
    FeedObservation {
        key: (h.exchange.clone(), h.pair.clone()),
        market_closed: is_effectively_market_closed(
            h.status == FeedStatus::MarketClosed,
            h.market_closed_at,
            now,
            stale_after_secs,
        ),
        last_tick_at: h
            .last_tick_age_secs
            .map(|age| now - chrono::Duration::seconds(age)),
    }
}

fn build_freshness_alert(
    exchange: Exchange,
    pair: &Pair,
    event: FreshnessEvent,
    last_tick_at: Option<DateTime<Utc>>,
    watchdog_started_at: DateTime<Utc>,
) -> (String, String, bool) {
    match event {
        FreshnessEvent::NewlyStale { since } | FreshnessEvent::StillStale { since } => (
            "feed stale".to_string(),
            format_stale_alert(exchange, pair, since, last_tick_at, watchdog_started_at),
            true,
        ),
        FreshnessEvent::Recovered {
            since,
            recovered_at,
        } => (
            "feed recovered".to_string(),
            format_recovered_alert(exchange, pair, since, recovered_at),
            false,
        ),
    }
}

#[cfg(test)]
mod tests;
