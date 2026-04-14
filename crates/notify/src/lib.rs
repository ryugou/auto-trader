//! 外部通知チャネル (Slack Webhook など)。
//!
//! `db::notifications` はアプリ内通知（UI ベル表示）専用で、オペレータが
//! 外部で気付く通知はこの crate が担う。本 PR では Slack Webhook の
//! 送信のみを実装し、発火ポイント (`LiveTrader` / `RiskGate` / reconciler
//! など) は後続 PR で配線する。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct OrderFilledEvent {
    pub account_name: String,
    pub trade_id: Uuid,
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
    pub price: Decimal,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrderFailedEvent {
    pub account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PositionClosedEvent {
    pub account_name: String,
    pub trade_id: Uuid,
    pub pnl_amount: Decimal,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct KillSwitchTriggeredEvent {
    pub account_name: String,
    pub daily_loss: Decimal,
    pub limit: Decimal,
    pub halted_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct KillSwitchReleasedEvent {
    pub account_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebSocketDisconnectedEvent {
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StartupReconciliationDiffEvent {
    pub orphan_db_trade_ids: Vec<Uuid>,
    pub orphan_exchange_positions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BalanceDriftEvent {
    pub account_name: String,
    pub db_balance: Decimal,
    pub exchange_balance: Decimal,
    pub diff_pct: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct DryRunOrderEvent {
    pub account_name: String,
    pub strategy_name: String,
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
    pub intended_price: Decimal,
}

/// 通知イベント。Slack には各イベントごとに整形された文面で送る。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifyEvent {
    OrderFilled(OrderFilledEvent),
    OrderFailed(OrderFailedEvent),
    PositionClosed(PositionClosedEvent),
    KillSwitchTriggered(KillSwitchTriggeredEvent),
    KillSwitchReleased(KillSwitchReleasedEvent),
    WebSocketDisconnected(WebSocketDisconnectedEvent),
    StartupReconciliationDiff(StartupReconciliationDiffEvent),
    BalanceDrift(BalanceDriftEvent),
    DryRunOrder(DryRunOrderEvent),
}

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("slack returned non-2xx status: {0}")]
    Status(u16),
}

/// Slack Webhook 送信クライアント。`slack_webhook_url` が None なら
/// no-op（ログのみ）。通知失敗は本業務を止めないため、送信失敗は
/// warn ログに留め、呼び出し側に Result を返しつつも実運用では
/// 結果を無視してよい設計。
#[derive(Clone)]
pub struct Notifier {
    slack_webhook_url: Option<String>,
    http: reqwest::Client,
}

impl Notifier {
    pub fn new(slack_webhook_url: Option<String>) -> Self {
        Self {
            slack_webhook_url,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
        }
    }

    /// URL を差し替えたい（テスト用）場合のコンストラクタ。
    pub fn with_client(slack_webhook_url: Option<String>, http: reqwest::Client) -> Self {
        Self {
            slack_webhook_url,
            http,
        }
    }

    pub async fn send(&self, event: NotifyEvent) -> Result<(), NotifyError> {
        let Some(url) = &self.slack_webhook_url else {
            tracing::debug!(?event, "notify: slack webhook not configured, skipping");
            return Ok(());
        };
        let text = format_for_slack(&event);
        let body = serde_json::json!({ "text": text });
        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(NotifyError::Http)?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(status = status.as_u16(), "notify: slack returned non-2xx");
            return Err(NotifyError::Status(status.as_u16()));
        }
        Ok(())
    }
}

fn format_for_slack(event: &NotifyEvent) -> String {
    match event {
        NotifyEvent::OrderFilled(e) => format!(
            "✅ *約定* `{}` {} {} {} @ {} (trade {})",
            e.account_name, e.pair, e.direction, e.quantity, e.price, e.trade_id
        ),
        NotifyEvent::OrderFailed(e) => format!(
            "❌ *発注失敗* `{}` {} {} — {}",
            e.account_name, e.strategy_name, e.pair, e.reason
        ),
        NotifyEvent::PositionClosed(e) => format!(
            "🔒 *クローズ* `{}` pnl={} reason={} (trade {})",
            e.account_name, e.pnl_amount, e.reason, e.trade_id
        ),
        NotifyEvent::KillSwitchTriggered(e) => format!(
            "🛑 *Kill Switch 発動* `{}` 日次損失 {} / 上限 {} — 再開予定 {}",
            e.account_name, e.daily_loss, e.limit, e.halted_until
        ),
        NotifyEvent::KillSwitchReleased(e) => format!("🟢 *Kill Switch 解除* `{}`", e.account_name),
        NotifyEvent::WebSocketDisconnected(e) => {
            format!("⚠️ *WebSocket 切断* {} 秒", e.duration_secs)
        }
        NotifyEvent::StartupReconciliationDiff(e) => format!(
            "⚠️ *起動時リコン差分* DB のみ={} 件, 取引所のみ={} 件",
            e.orphan_db_trade_ids.len(),
            e.orphan_exchange_positions.len()
        ),
        NotifyEvent::BalanceDrift(e) => format!(
            "⚠️ *残高ズレ* `{}` DB={} / 取引所={} ({}%)",
            e.account_name, e.db_balance, e.exchange_balance, e.diff_pct
        ),
        NotifyEvent::DryRunOrder(e) => format!(
            "🧪 *DRY RUN* `{}` {} {} {} {} @ {} (発注せず)",
            e.account_name, e.strategy_name, e.pair, e.direction, e.quantity, e.intended_price
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn format_order_filled() {
        let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
            account_name: "通常".into(),
            trade_id: Uuid::nil(),
            pair: "FX_BTC_JPY".into(),
            direction: "long".into(),
            quantity: dec!(0.01),
            price: dec!(11500000),
            at: Utc::now(),
        });
        let s = format_for_slack(&ev);
        assert!(s.contains("約定"));
        assert!(s.contains("通常"));
        assert!(s.contains("FX_BTC_JPY"));
        assert!(s.contains("11500000"));
    }

    #[test]
    fn format_kill_switch_triggered() {
        let ev = NotifyEvent::KillSwitchTriggered(KillSwitchTriggeredEvent {
            account_name: "通常".into(),
            daily_loss: dec!(-1500),
            limit: dec!(-1500),
            halted_until: Utc::now(),
        });
        let s = format_for_slack(&ev);
        assert!(s.contains("Kill Switch"));
        assert!(s.contains("通常"));
    }

    #[tokio::test]
    async fn send_without_webhook_is_noop() {
        let n = Notifier::new(None);
        let ev = NotifyEvent::WebSocketDisconnected(WebSocketDisconnectedEvent {
            duration_secs: 30,
        });
        // webhook が None なので即 Ok(())
        n.send(ev).await.unwrap();
    }
}
