//! live 口座の current_balance を定期的に bitFlyer から同期。

use auto_trader_db::trading_accounts::{self, TradingAccount};
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_notify::{BalanceDriftEvent, Notifier, NotifyEvent};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Returns true if |exchange - db| / db > threshold. Zero db balance → false
/// (avoids div by zero; caller can still see the raw values if needed).
pub fn is_drift_over_threshold(
    db_balance: Decimal,
    exchange_balance: Decimal,
    threshold: Decimal,
) -> bool {
    if db_balance.is_zero() {
        return false;
    }
    let diff = (exchange_balance - db_balance).abs();
    let ratio = diff / db_balance;
    ratio > threshold
}

pub async fn sync_account(
    pool: &PgPool,
    api: &BitflyerPrivateApi,
    notifier: &Notifier,
    account: &TradingAccount,
    drift_threshold: Decimal,
) -> anyhow::Result<()> {
    let collateral = api.get_collateral().await?;
    let exchange_balance = collateral.collateral;
    if is_drift_over_threshold(account.current_balance, exchange_balance, drift_threshold) {
        // diff_pct は ×100 済みのパーセンテージ値 (例: 5 = 5%)。
        let diff_pct = if account.current_balance.is_zero() {
            Decimal::ZERO
        } else {
            let ratio =
                (exchange_balance - account.current_balance).abs() / account.current_balance;
            ratio * Decimal::ONE_HUNDRED
        };
        let ev = NotifyEvent::BalanceDrift(BalanceDriftEvent {
            account_name: account.name.clone(),
            db_balance: account.current_balance,
            exchange_balance,
            diff_pct,
        });
        if let Err(e) = notifier.send(ev).await {
            tracing::error!("balance_sync notify failed for {}: {e}", account.name);
        }
    }
    // Always pull DB value toward exchange truth.
    trading_accounts::update_balance(pool, account.id, exchange_balance).await?;
    Ok(())
}

pub async fn run_balance_sync_loop(
    pool: PgPool,
    api: Arc<BitflyerPrivateApi>,
    notifier: Arc<Notifier>,
    interval_secs: u64,
    drift_threshold: Decimal,
    approved_live_account_ids: Arc<HashSet<Uuid>>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        let accounts = match trading_accounts::list_all(&pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("balance_sync: list_all failed: {e}");
                continue;
            }
        };
        for acc in &accounts {
            // Only process accounts approved for execution at startup; paper
            // accounts are always allowed, unknown types are refused.
            if !crate::startup::is_account_approved_for_execution(
                &acc.account_type,
                acc.id,
                &approved_live_account_ids,
            ) {
                continue;
            }
            // Balance sync is a live-only concern; skip paper accounts even
            // though they are "approved" — they have no exchange balance.
            if acc.account_type != "live" {
                continue;
            }
            if let Err(e) = sync_account(&pool, &api, &notifier, acc, drift_threshold).await {
                tracing::error!("balance_sync: account {} errored: {e}", acc.name);
            }
        }
    }
}
