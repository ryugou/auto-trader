#![doc = "Slack webhook notifications for order, position and system events."]

use auto_trader_core::types::{Direction, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

/// Event emitted when an order is filled.
#[derive(Debug, Clone, Serialize)]
pub struct OrderFilledEvent {
    /// Name of the account that placed the order.
    pub account_name: String,
    /// Exchange the order was filled on.
    pub exchange: Exchange,
    /// Identifier of the trade that the fill belongs to.
    pub trade_id: Uuid,
    /// Currency pair that was traded.
    pub pair: Pair,
    /// Direction of the trade.
    pub direction: Direction,
    /// Filled quantity.
    pub quantity: Decimal,
    /// Fill price.
    pub price: Decimal,
    /// Time of the fill.
    pub at: DateTime<Utc>,
}

/// Event emitted when an order fails.
#[derive(Debug, Clone, Serialize)]
pub struct OrderFailedEvent {
    /// Name of the account that placed the order.
    pub account_name: String,
    /// Exchange the order was sent to.
    pub exchange: Exchange,
    /// Name of the strategy that produced the order.
    pub strategy_name: String,
    /// Currency pair of the order.
    pub pair: Pair,
    /// Reason the order failed.
    pub reason: String,
}

/// Event emitted when a position is closed.
#[derive(Debug, Clone, Serialize)]
pub struct PositionClosedEvent {
    /// Name of the account that held the position.
    pub account_name: String,
    /// Exchange the position was closed on.
    pub exchange: Exchange,
    /// Identifier of the closed trade.
    pub trade_id: Uuid,
    /// Profit or loss of the trade.
    pub pnl_amount: Decimal,
    /// Reason the position was closed.
    pub reason: String,
}

/// Event emitted for an alert that belongs to one account.
#[derive(Debug, Clone, Serialize)]
pub struct SystemAlertEvent {
    /// Short headline of the alert.
    pub title: String,
    /// Name of the affected account.
    pub account_name: String,
    /// Exchange the alert concerns.
    pub exchange: Exchange,
    /// Detail text of the alert.
    pub body: String,
}

/// Event emitted for an alert that belongs to the process as a whole.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessAlertEvent {
    /// Short headline of the alert.
    pub title: String,
    /// Detail text of the alert.
    pub body: String,
}

/// Event that `Notifier::send` delivers to Slack.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifyEvent {
    /// An order was filled.
    OrderFilled(OrderFilledEvent),
    /// An order failed.
    OrderFailed(OrderFailedEvent),
    /// A position was closed.
    PositionClosed(PositionClosedEvent),
    /// An alert about one account.
    SystemAlert(SystemAlertEvent),
    /// An alert about the process.
    ProcessAlert(ProcessAlertEvent),
}

impl NotifyEvent {
    /// Returns the snake_case name of the variant.
    pub fn variant_name(&self) -> &'static str {
        match self {
            NotifyEvent::OrderFilled(_) => "order_filled",
            NotifyEvent::OrderFailed(_) => "order_failed",
            NotifyEvent::PositionClosed(_) => "position_closed",
            NotifyEvent::SystemAlert(_) => "system_alert",
            NotifyEvent::ProcessAlert(_) => "process_alert",
        }
    }
}

/// Error returned when a Slack notification cannot be built or sent.
#[derive(Debug, Error)]
pub enum NotifyError {
    /// The request to Slack failed.
    #[error("failed to send slack request: {0}")]
    Http(reqwest::Error),
    /// The HTTP client could not be built.
    #[error("failed to build slack http client: {0}")]
    BuildClient(reqwest::Error),
    /// Slack answered with a non-success status code.
    #[error("unsupported slack response status: {0}")]
    Status(u16),
}

fn redact_webhook_url(e: reqwest::Error) -> reqwest::Error {
    e.without_url()
}

impl From<reqwest::Error> for NotifyError {
    fn from(e: reqwest::Error) -> Self {
        NotifyError::Http(redact_webhook_url(e))
    }
}

#[derive(Clone)]
struct SlackWebhook {
    url: String,
    http: reqwest::Client,
}

/// Notifier that sends events to a Slack incoming webhook, or discards them when no webhook is configured.
#[derive(Clone)]
pub struct Notifier {
    slack: Option<SlackWebhook>,
}

impl Notifier {
    /// Creates a notifier that discards every event.
    pub fn new_disabled() -> Self {
        Self { slack: None }
    }

    /// Creates a notifier for the given webhook URL, or a disabled one when the URL is `None`.
    ///
    /// # Errors
    ///
    /// - Returns an error if the HTTP client cannot be built.
    pub fn new(slack_webhook_url: Option<String>) -> Result<Self, NotifyError> {
        let Some(url) = slack_webhook_url else {
            return Ok(Self::new_disabled());
        };
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(
                auto_trader_core::config::NOTIFY_SEND_TIMEOUT_SECS,
            ))
            .build()
            .map_err(|e| NotifyError::BuildClient(redact_webhook_url(e)))?;
        Ok(Self {
            slack: Some(SlackWebhook { url, http }),
        })
    }

    /// Sends the event to Slack, doing nothing when no webhook is configured.
    ///
    /// # Errors
    ///
    /// - Returns an error if the request to Slack fails.
    /// - Returns an error if Slack answers with a non-success status.
    pub async fn send(&self, event: NotifyEvent) -> Result<(), NotifyError> {
        let variant = event.variant_name();
        let Some(slack) = &self.slack else {
            tracing::debug!(event = variant, "slack webhook not configured, skipping");
            return Ok(());
        };
        let text = format_for_slack(&event);
        let body = serde_json::json!({ "text": text });
        let resp = slack
            .http
            .post(&slack.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| log_send_error(variant, e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(log_non_success_status(variant, status));
        }
        tracing::debug!(event = variant, "slack ok");
        Ok(())
    }
}

fn log_send_error(variant: &str, e: reqwest::Error) -> NotifyError {
    let redacted = redact_webhook_url(e);
    tracing::warn!(
        event = variant,
        is_timeout = redacted.is_timeout(),
        is_connect = redacted.is_connect(),
        error = %redacted,
        "failed to send slack request"
    );
    NotifyError::Http(redacted)
}

fn log_non_success_status(variant: &str, status: reqwest::StatusCode) -> NotifyError {
    tracing::warn!(
        event = variant,
        status = status.as_u16(),
        "unsupported slack response status"
    );
    NotifyError::Status(status.as_u16())
}

fn format_for_slack(event: &NotifyEvent) -> String {
    match event {
        NotifyEvent::OrderFilled(e) => format!(
            "✅ *約定* `{}` [{}] {} {} {} @ {} (trade {})",
            e.account_name,
            e.exchange.as_str(),
            e.pair,
            e.direction,
            e.quantity,
            e.price,
            e.trade_id
        ),
        NotifyEvent::OrderFailed(e) => format!(
            "❌ *発注失敗* `{}` [{}] {} {} — {}",
            e.account_name,
            e.exchange.as_str(),
            e.strategy_name,
            e.pair,
            e.reason
        ),
        NotifyEvent::PositionClosed(e) => format!(
            "🔒 *クローズ* `{}` [{}] pnl={} reason={} (trade {})",
            e.account_name,
            e.exchange.as_str(),
            e.pnl_amount,
            e.reason,
            e.trade_id
        ),
        NotifyEvent::SystemAlert(e) => format!(
            ":rotating_light: *{}* — {} [{}]\n{}",
            e.title,
            e.account_name,
            e.exchange.as_str(),
            e.body
        ),
        NotifyEvent::ProcessAlert(e) => format!(":rotating_light: *{}*\n{}", e.title, e.body),
    }
}

#[cfg(test)]
mod tests;
