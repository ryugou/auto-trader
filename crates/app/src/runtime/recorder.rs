use std::sync::Arc;

use auto_trader_core::event::{TradeAction, TradeEvent};
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::types::Trade;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

pub(crate) async fn run(
    pool: sqlx::PgPool,
    knowledge_store: Option<Arc<dyn KnowledgeStore>>,
    mut trade_rx: mpsc::Receiver<TradeEvent>,
) {
    while let Some(trade_event) = trade_rx.recv().await {
        match trade_event.action {
            TradeAction::Opened => {}
            TradeAction::Closed { .. } => {
                record_closed_trade(&pool, &knowledge_store, &trade_event).await;
            }
        }
    }
}

async fn record_closed_trade(
    pool: &sqlx::PgPool,
    knowledge_store: &Option<Arc<dyn KnowledgeStore>>,
    trade_event: &TradeEvent,
) {
    let t = &trade_event.trade;
    if let (Some(_exit_price), Some(exit_at), Some(pnl_amount), Some(_exit_reason)) =
        (t.exit_price, t.exit_at, t.pnl_amount, t.exit_reason)
    {
        let account_type = trade_event.account_type.as_deref();
        upsert_close_daily_summary(pool, t, account_type, exit_at, pnl_amount).await;
        spawn_knowledge_task(pool, knowledge_store, t);
    }
}

async fn upsert_close_daily_summary(
    pool: &sqlx::PgPool,
    t: &Trade,
    account_type: Option<&str>,
    exit_at: DateTime<Utc>,
    pnl_amount: Decimal,
) {
    let date = exit_at.date_naive();
    let win = if pnl_amount > Decimal::ZERO { 1 } else { 0 };
    let account_id = t.account_id;
    let account_type = account_type.unwrap_or("paper");
    if let Err(e) = auto_trader_db::summary::upsert_daily_summary(
        pool,
        date,
        &t.strategy_name,
        &t.pair.0,
        account_type,
        t.exchange.as_str(),
        Some(account_id),
        1,
        win,
        pnl_amount,
    )
    .await
    {
        tracing::error!(error = %e, "failed to upsert daily summary");
    }
}

fn spawn_knowledge_task(
    pool: &sqlx::PgPool,
    knowledge_store: &Option<Arc<dyn KnowledgeStore>>,
    trade: &Trade,
) {
    if let Some(store) = knowledge_store.clone() {
        let t = trade.clone();
        let close_pool = pool.clone();
        tokio::spawn(async move {
            run_knowledge_task(store, close_pool, t).await;
        });
    }
}

async fn fetch_entry_indicators(
    pool: &sqlx::PgPool,
    trade_id: uuid::Uuid,
) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT entry_indicators FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .flatten()
}

async fn fetch_account_balances(
    pool: &sqlx::PgPool,
    account_id: uuid::Uuid,
) -> (Option<Decimal>, Option<Decimal>) {
    sqlx::query_as::<_, (Decimal, Decimal)>(
        "SELECT current_balance, initial_balance FROM trading_accounts WHERE id = $1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await
    .unwrap_or(None)
    .map(|(b, i)| (Some(b), Some(i)))
    .unwrap_or((None, None))
}

async fn fetch_vegapunk_search_id(pool: &sqlx::PgPool, trade_id: uuid::Uuid) -> Option<uuid::Uuid> {
    sqlx::query_scalar("SELECT vegapunk_search_id FROM trades WHERE id = $1")
        .bind(trade_id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None)
        .flatten()
}

async fn run_knowledge_task(store: Arc<dyn KnowledgeStore>, close_pool: sqlx::PgPool, t: Trade) {
    let entry_ind = fetch_entry_indicators(&close_pool, t.id).await;
    let (bal, init) = fetch_account_balances(&close_pool, t.account_id).await;

    let ctx = auto_trader_core::knowledge::TradeCloseContext {
        entry_indicators: entry_ind.as_ref(),
        account_balance: bal,
        account_initial: init,
    };
    if let Err(e) = store.record_trade_close(&t, &ctx).await {
        tracing::warn!(
            error = %e,
            "failed to record trade close in knowledge store"
        );
    }

    let search_id = fetch_vegapunk_search_id(&close_pool, t.id).await;

    if let Some(sid) = search_id {
        send_feedback(&store, &t, entry_ind.as_ref(), sid).await;
    }
}

async fn send_feedback(
    store: &Arc<dyn KnowledgeStore>,
    t: &Trade,
    entry_indicators: Option<&serde_json::Value>,
    search_id: uuid::Uuid,
) {
    let rating = auto_trader::enriched_ingest::compute_feedback_rating(t);
    let net_pnl = t.pnl_amount.unwrap_or_default() - t.fees;
    let regime = regime_from_indicators(entry_indicators);
    let comment = feedback_comment(net_pnl, regime);
    let sid_str = search_id.to_string();
    if let Err(e) = store.submit_feedback(&sid_str, rating, &comment).await {
        tracing::warn!(
            error = %e,
            "failed to submit feedback to knowledge store"
        );
    }
}

fn regime_from_indicators(entry_indicators: Option<&serde_json::Value>) -> &str {
    entry_indicators
        .and_then(|i| i.get("regime"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
}

fn feedback_comment(net_pnl: Decimal, regime: &str) -> String {
    format!("PnL: {net_pnl}, regime: {regime}")
}

#[cfg(test)]
mod tests;
