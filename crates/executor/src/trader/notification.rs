use auto_trader_core::types::Pair;
use auto_trader_notify::{NotifyEvent, OrderFailedEvent};

use super::Trader;

pub(super) enum NotificationFailureLevel {
    Error,
    Warn,
}

impl Trader {
    pub(super) fn order_failed_event(
        &self,
        strategy_name: String,
        pair: Pair,
        reason: String,
    ) -> NotifyEvent {
        NotifyEvent::OrderFailed(OrderFailedEvent {
            account_name: self.account_name.clone(),
            exchange: self.exchange,
            strategy_name,
            pair,
            reason,
        })
    }

    pub(super) fn spawn_notification(
        &self,
        event: NotifyEvent,
        failure_level: NotificationFailureLevel,
        failure_message: &'static str,
    ) {
        let notifier = self.notifier.clone();
        tokio::spawn(async move {
            if let Err(e) = notifier.send(event).await {
                match failure_level {
                    NotificationFailureLevel::Error => {
                        tracing::error!(error = %e, "{failure_message}");
                    }
                    NotificationFailureLevel::Warn => {
                        tracing::warn!(error = %e, "{failure_message}");
                    }
                }
            }
        });
    }
}
