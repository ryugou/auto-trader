//! live アカウントの DB open トレードと取引所建玉の差分検出。

use auto_trader_db::trading_accounts::{self, TradingAccount};
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_notify::{Notifier, NotifyEvent, StartupReconciliationDiffEvent};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct DbOpen {
    pub trade_id: Uuid,
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeOpen {
    pub pair: String,
    pub direction: String,
    pub quantity: Decimal,
}

#[derive(Debug, Clone)]
pub struct QuantityMismatch {
    pub trade_ids: Vec<Uuid>,
    pub pair: String,
    pub db_qty: Decimal,
    pub exchange_qty: Decimal,
}

#[derive(Debug, Default)]
pub struct ReconcileDiff {
    pub db_orphan: Vec<Uuid>,
    pub exchange_orphan: Vec<ExchangeOpen>,
    pub quantity_mismatch: Vec<QuantityMismatch>,
}

pub fn compute_diff(db: &[DbOpen], exch: &[ExchangeOpen]) -> ReconcileDiff {
    use std::collections::HashMap;
    let mut diff = ReconcileDiff::default();
    let mut db_by_key: HashMap<(String, String), (Decimal, Vec<Uuid>)> = HashMap::new();
    for o in db {
        let k = (o.pair.clone(), o.direction.clone());
        let e = db_by_key.entry(k).or_insert((Decimal::ZERO, Vec::new()));
        e.0 += o.quantity;
        e.1.push(o.trade_id);
    }
    let mut exch_by_key: HashMap<(String, String), Decimal> = HashMap::new();
    for o in exch {
        *exch_by_key
            .entry((o.pair.clone(), o.direction.clone()))
            .or_insert(Decimal::ZERO) += o.quantity;
    }
    for (key, (db_qty, trade_ids)) in &db_by_key {
        match exch_by_key.get(key) {
            None => diff.db_orphan.extend(trade_ids),
            Some(ex_qty) => {
                if db_qty != ex_qty {
                    diff.quantity_mismatch.push(QuantityMismatch {
                        trade_ids: trade_ids.clone(),
                        pair: key.0.clone(),
                        db_qty: *db_qty,
                        exchange_qty: *ex_qty,
                    });
                }
            }
        }
    }
    for (key, ex_qty) in &exch_by_key {
        if !db_by_key.contains_key(key) {
            diff.exchange_orphan.push(ExchangeOpen {
                pair: key.0.clone(),
                direction: key.1.clone(),
                quantity: *ex_qty,
            });
        }
    }
    diff
}

pub async fn reconcile_account(
    pool: &PgPool,
    api: &BitflyerPrivateApi,
    notifier: &Notifier,
    account: &TradingAccount,
    product_code: &str,
) -> anyhow::Result<()> {
    let db_rows: Vec<(Uuid, String, String, Decimal)> = sqlx::query_as(
        "SELECT id, pair, direction, quantity FROM trades
         WHERE account_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(account.id)
    .fetch_all(pool)
    .await?;
    let db_opens: Vec<DbOpen> = db_rows
        .into_iter()
        .map(|(id, pair, direction, qty)| DbOpen {
            trade_id: id,
            pair,
            direction,
            quantity: qty,
        })
        .collect();

    let positions = api.get_positions(product_code).await?;
    let exch_opens: Vec<ExchangeOpen> = positions
        .iter()
        .map(|p| {
            let direction = if p.side.eq_ignore_ascii_case("BUY") {
                "long".to_string()
            } else {
                "short".to_string()
            };
            ExchangeOpen {
                pair: p.product_code.clone(),
                direction,
                quantity: p.size,
            }
        })
        .collect();

    let diff = compute_diff(&db_opens, &exch_opens);
    if diff.db_orphan.is_empty()
        && diff.exchange_orphan.is_empty()
        && diff.quantity_mismatch.is_empty()
    {
        return Ok(());
    }
    tracing::warn!(
        "reconciler drift for {}: db_orphan={} exch_orphan={} qty_mismatch={}",
        account.name,
        diff.db_orphan.len(),
        diff.exchange_orphan.len(),
        diff.quantity_mismatch.len(),
    );
    let ev = NotifyEvent::StartupReconciliationDiff(StartupReconciliationDiffEvent {
        account_name: account.name.clone(),
        db_orphan: diff.db_orphan,
        exchange_orphan_count: diff.exchange_orphan.len(),
        quantity_mismatch_count: diff.quantity_mismatch.len(),
    });
    if let Err(e) = notifier.send(ev).await {
        tracing::error!("reconciler notify failed for {}: {e}", account.name);
    }
    Ok(())
}

pub async fn run_reconciler_loop(
    pool: PgPool,
    api: Arc<BitflyerPrivateApi>,
    notifier: Arc<Notifier>,
    product_code: String,
    interval_secs: u64,
    approved_live_account_ids: Arc<HashSet<Uuid>>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        ticker.tick().await;
        let accounts = match trading_accounts::list_all(&pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("reconciler: list_all failed: {e}");
                continue;
            }
        };
        for acc in &accounts {
            if acc.account_type != "live" {
                continue;
            }
            // Only reconcile accounts that were approved at startup; refuse
            // any live account inserted via REST after startup validation ran.
            if !approved_live_account_ids.contains(&acc.id) {
                continue;
            }
            if let Err(e) = reconcile_account(&pool, &api, &notifier, acc, &product_code).await {
                tracing::error!("reconciler: account {} errored: {e}", acc.name);
            }
        }
    }
}
