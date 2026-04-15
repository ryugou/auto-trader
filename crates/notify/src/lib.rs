//! 外部通知チャネル (Slack Webhook など)。
//!
//! `db::notifications` はアプリ内通知（UI ベル表示）専用で、オペレータが
//! 外部で気付く通知はこの crate が担う。本 PR では Slack Webhook の
//! 送信のみを実装し、発火ポイント (`LiveTrader` / `RiskGate` / reconciler
//! など) は後続 PR で配線する。

use auto_trader_core::types::{Direction, Pair};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct OrderFilledEvent {
    pub account_name: String,
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
    pub strategy_name: String,
    pub pair: Pair,
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
    pub account_name: String,
    pub db_orphan: Vec<Uuid>,
    pub exchange_orphan_count: usize,
    pub quantity_mismatch_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BalanceDriftEvent {
    pub account_name: String,
    pub db_balance: Decimal,
    pub exchange_balance: Decimal,
    /// 残高の乖離率 **パーセンテージ値** (例: `5` = 5%、`0.5` = 0.5%)。
    /// `db_balance / exchange_balance - 1` の **×100 済み** の値を渡すこと。
    /// format_for_slack は `{}%` として表示するので、呼び出し側で小数
    /// (0.05 = 5%) を渡すと Slack には `0.05%` と出て 100 倍のズレになる。
    pub diff_pct: Decimal,
}

#[derive(Debug, Clone, Serialize)]
pub struct DryRunOrderEvent {
    pub account_name: String,
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub quantity: Decimal,
    pub intended_price: Decimal,
}

/// 通知イベント。Slack には各イベントごとに整形された文面で送る。
///
/// **Slack 4000 文字上限ポリシー**: format_for_slack はリスト系イベント
/// (`StartupReconciliationDiff`) で ID 一覧を埋め込まず `.len()` のみ
/// 表示する方針。巨大な ID 一覧を本文に含めて truncate で末尾欠落する
/// より、件数を出して詳細は DB 照会へ誘導する方が安全。新バリアントを
/// 追加する際もこのポリシーに従うこと。
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

impl NotifyEvent {
    /// イベント種別の short name。失敗ログ等で可読性が高い値を返す。
    /// `std::mem::discriminant` より人間可読で、将来 serde 実装変更にも耐える。
    pub fn variant_name(&self) -> &'static str {
        match self {
            NotifyEvent::OrderFilled(_) => "order_filled",
            NotifyEvent::OrderFailed(_) => "order_failed",
            NotifyEvent::PositionClosed(_) => "position_closed",
            NotifyEvent::KillSwitchTriggered(_) => "kill_switch_triggered",
            NotifyEvent::KillSwitchReleased(_) => "kill_switch_released",
            NotifyEvent::WebSocketDisconnected(_) => "websocket_disconnected",
            NotifyEvent::StartupReconciliationDiff(_) => "startup_reconciliation_diff",
            NotifyEvent::BalanceDrift(_) => "balance_drift",
            NotifyEvent::DryRunOrder(_) => "dry_run_order",
        }
    }
}

#[derive(Debug, Error)]
pub enum NotifyError {
    /// reqwest 由来の HTTP エラー。`From<reqwest::Error>` は `?` 経由で
    /// 呼ばれても `without_url()` で URL (= Slack Webhook secret) を
    /// 必ず落とすよう手書き実装している。`#[from]` を使わない理由は
    /// まさにこの secret redaction 強制のため。
    #[error("http error: {0}")]
    Http(reqwest::Error),
    #[error("slack returned non-2xx status: {0}")]
    Status(u16),
}

impl From<reqwest::Error> for NotifyError {
    fn from(e: reqwest::Error) -> Self {
        // `?` で自動変換された場合でも必ず URL を落とす。
        // この 1 箇所でガードすることで、将来 send() 以外の関数が
        // `self.http.<...>.send().await?` のパターンで reqwest::Error
        // を投げても secret が漏れない。
        NotifyError::Http(e.without_url())
    }
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
    /// No-op notifier for tests: behaves like `new(None)` (Slack skipped).
    pub fn new_disabled() -> Self {
        Self::new(None)
    }

    pub fn new(slack_webhook_url: Option<String>) -> Self {
        Self {
            slack_webhook_url,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client builder should not fail with basic config"),
        }
    }

    /// Slack Webhook にイベントを送信する。
    ///
    /// **呼び出し側規約 (PR 2 以降で厳守)**:
    /// - `OrderFilled` / `DryRunOrder` など高頻度・低重要度: `tokio::spawn`
    ///   で fire-and-forget (`let _ = notifier.send(ev).await;`)
    /// - `KillSwitchTriggered` / `BalanceDrift` / `StartupReconciliationDiff`
    ///   など critical: `.await` で結果を確認し、失敗時は DB
    ///   `notifications` テーブル (UI ベル) に backstop 書き込み
    /// - リトライは本メソッド内で行わない。配線側の責務。
    pub async fn send(&self, event: NotifyEvent) -> Result<(), NotifyError> {
        let variant = event.variant_name();
        let Some(url) = &self.slack_webhook_url else {
            tracing::debug!(
                event = variant,
                "notify: slack webhook not configured, skipping"
            );
            return Ok(());
        };
        let text = format_for_slack(&event);
        let body = serde_json::json!({ "text": text });
        // reqwest::Error の Display (%e) は失敗した URL を含むため、
        // そのままログ出力 / 呼び出し側への return に使うと Slack
        // Webhook URL (= secret) が漏れる。without_url() で URL を
        // 落としてから記録・返却する。
        let resp = match self.http.post(url).json(&body).send().await {
            Ok(resp) => resp,
            Err(e) => {
                let redacted = e.without_url();
                tracing::warn!(
                    event = variant,
                    is_timeout = redacted.is_timeout(),
                    is_connect = redacted.is_connect(),
                    error = %redacted,
                    "notify: slack http error"
                );
                return Err(NotifyError::Http(redacted));
            }
        };
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(
                event = variant,
                status = status.as_u16(),
                "notify: slack returned non-2xx"
            );
            return Err(NotifyError::Status(status.as_u16()));
        }
        tracing::debug!(event = variant, "notify: slack ok");
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
            "⚠️ *リコン差分* `{}` DB のみ={} 件, 取引所のみ={} 件, 数量不一致={} 件",
            e.account_name,
            e.db_orphan.len(),
            e.exchange_orphan_count,
            e.quantity_mismatch_count,
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
            pair: Pair::new("FX_BTC_JPY"),
            direction: Direction::Long,
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
        let ev =
            NotifyEvent::WebSocketDisconnected(WebSocketDisconnectedEvent { duration_secs: 30 });
        n.send(ev).await.unwrap();
    }
}
