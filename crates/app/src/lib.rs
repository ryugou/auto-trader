#![doc = "Runtime services of the trading application: the dashboard API, the monitors of balances, margins, stops and feeds, the closing of trades, the startup reconciliation and the weekly strategy evolution."]

/// Router and state of the dashboard API.
pub mod api;
/// Check of the drift between the exchange equity and the bot equity of live accounts.
pub mod balance_drift;
/// Closing of a trade and sending of the closed trade event.
pub mod closer;
/// Texts and feedback rating that describe opened and closed trades.
pub mod enriched_ingest;
/// Detection of process suspends and stale feeds, and supervision of the feed tasks.
pub mod feed_watchdog;
/// Knowledge store backed by Vegapunk.
pub mod knowledge;
/// Detection of dry run accounts whose maintenance ratio is below the liquidation margin level.
pub mod liquidation;
/// Detection of live accounts whose maintenance ratio nears the liquidation margin level.
pub mod margin_alert;
/// Construction of the open positions of trades from the latest prices.
pub mod positions;
/// Store of the latest tick of each feed.
pub mod price_store;
/// Classification of the market regime.
pub mod regime;
#[doc(hidden)]
pub mod startup;
/// Reconciliation of the open trades of live accounts with the exchange positions at startup.
pub mod startup_reconcile;
/// Detection and closing of trades whose stop order has been executed.
pub mod stop_fill;
/// Alert about empty or stale GMO FX swap rates.
pub mod swap_freshness;
/// Weekly evolution of the strategy parameters.
pub mod weekly_batch;
/// Lower bound of the Wilson score interval.
pub mod wilson;

use std::sync::Arc;

/// Spawns a task that sends `event` with `notifier` and returns at once; a failure to send is logged as a warning and not returned.
///
/// # Panics
///
/// Panics if called outside a Tokio runtime.
pub fn spawn_notify(
    notifier: &Arc<auto_trader_notify::Notifier>,
    event: auto_trader_notify::NotifyEvent,
) {
    let notifier = notifier.clone();
    tokio::spawn(async move {
        if let Err(e) = notifier.send(event).await {
            tracing::warn!(error = %e, "failed to send notify event");
        }
    });
}

/// Returns the seconds to wait before retry number `attempt`, counting from 1: `initial_secs` doubled once for each earlier retry, at most 63 times, saturating at `u64::MAX`. An `attempt` of 0 is treated as 1.
pub fn notify_retry_delay_secs(attempt: u64, initial_secs: u64) -> u64 {
    let exponent = attempt.saturating_sub(1).min(63) as u32;
    initial_secs.saturating_mul(1u64 << exponent)
}

const SUMMARY_BODY_MAX_CHARS: usize = 200;

fn truncate_body_for_summary(body: &str) -> String {
    let mut chars = body.chars();
    let mut head: String = chars.by_ref().take(SUMMARY_BODY_MAX_CHARS).collect();
    if chars.next().is_some() {
        head.push('…');
    }
    head
}

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
            event = %label,
            wait_secs = wait_secs,
            attempt = attempt,
            attempts = attempts,
            error = %last_err,
            "failed to send notify event, retrying"
        );
        sleep(wait_secs).await;
        match send().await {
            Ok(()) => {
                tracing::info!(
                    event = %label,
                    attempt = attempt,
                    attempts = attempts,
                    "notify send succeeded on retry"
                );
                return Ok(());
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Spawns a task that sends `event` with `notifier` and returns at once. When a send fails, the task retries up to `attempts` times, waiting `notify_retry_delay_secs(n, initial_secs)` seconds before retry `n`, and logs a warning before each retry. When every retry fails, it logs an error and gives up; the failure is not returned.
///
/// # Panics
///
/// Panics if called outside a Tokio runtime.
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
                attempts = attempts,
                event = %notify_event_summary(&event),
                error = %e,
                "failed to send notify event, giving up"
            );
        }
    });
}

/// Spawns one task per alert of `alerts` with `spawn_notify`, which sends it as a system alert event, and returns at once; the sends are not ordered.
///
/// # Panics
///
/// Panics if called outside a Tokio runtime and `alerts` is not empty.
pub fn spawn_system_alerts(
    notifier: &Arc<auto_trader_notify::Notifier>,
    alerts: Vec<auto_trader_notify::SystemAlertEvent>,
) {
    for ev in alerts {
        spawn_notify(notifier, auto_trader_notify::NotifyEvent::SystemAlert(ev));
    }
}

#[cfg(test)]
mod tests;
