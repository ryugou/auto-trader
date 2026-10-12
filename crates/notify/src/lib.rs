use auto_trader_core::types::{Direction, Exchange, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct OrderFilledEvent {
    pub account_name: String,
    pub exchange: Exchange,
    pub trade_id: Uuid,
    pub pair: Pair,
    pub direction: Direction,
    pub quantity: Decimal,
    pub price: Decimal,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderFailedEvent {
    pub account_name: String,
    pub exchange: Exchange,
    pub strategy_name: String,
    pub pair: Pair,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionClosedEvent {
    pub account_name: String,
    pub exchange: Exchange,
    pub trade_id: Uuid,
    pub pnl_amount: Decimal,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemAlertEvent {
    pub title: String,
    pub account_name: String,
    pub exchange: Exchange,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcessAlertEvent {
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifyEvent {
    OrderFilled(OrderFilledEvent),
    OrderFailed(OrderFailedEvent),
    PositionClosed(PositionClosedEvent),
    SystemAlert(SystemAlertEvent),
    ProcessAlert(ProcessAlertEvent),
}

impl NotifyEvent {
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

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("failed to send slack request: {0}")]
    Http(reqwest::Error),
    #[error("failed to build slack http client: {0}")]
    BuildClient(reqwest::Error),
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

#[derive(Clone)]
pub struct Notifier {
    slack: Option<SlackWebhook>,
}

impl Notifier {
    pub fn new_disabled() -> Self {
        Self { slack: None }
    }

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
