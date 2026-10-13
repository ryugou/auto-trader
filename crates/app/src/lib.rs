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
