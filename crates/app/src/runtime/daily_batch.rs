use std::sync::Arc;

use auto_trader_core::knowledge::KnowledgeStore;
use chrono::{DateTime, Datelike as _, Duration, NaiveDate, Utc};

const JST_OFFSET_HOURS: i64 = 9;
const DEFAULT_BACKFILL_DAYS: u64 = 7;

pub(crate) struct DailyBatchContext {
    pub(crate) pool: sqlx::PgPool,
    pub(crate) knowledge_store: Option<Arc<dyn KnowledgeStore>>,
    pub(crate) gemini_api_url: String,
    pub(crate) gemini_api_key: String,
    pub(crate) gemini_model: String,
    pub(crate) backfill_days: Option<u64>,
}

pub(crate) async fn run(ctx: DailyBatchContext) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    let today = Utc::now().date_naive();

    run_startup_backfill(&ctx.pool, today, ctx.backfill_days).await;
    purge_old_read_notifications(&ctx.pool).await;

    let mut last_date = today;
    loop {
        interval.tick().await;
        let now_date = Utc::now().date_naive();
        if now_date != last_date {
            process_date_change(&ctx, last_date).await;
            last_date = now_date;
        }
    }
}

async fn run_startup_backfill(pool: &sqlx::PgPool, today: NaiveDate, backfill_days: Option<u64>) {
    for d in backfill_dates(today, backfill_days) {
        tracing::info!(date = %d, "running daily batch backfill at startup");
        if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(pool, d).await {
            tracing::error!(date = %d, error = %e, "failed to run daily batch backfill");
        }
    }
}

async fn purge_old_read_notifications(pool: &sqlx::PgPool) {
    match auto_trader_db::notifications::purge_old_read(pool).await {
        Ok(n) if n > 0 => tracing::info!(purged_count = n, "purged old read notifications"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "failed to purge old read notifications"),
    }
}

async fn process_date_change(ctx: &DailyBatchContext, last_date: NaiveDate) {
    tracing::info!(date = %last_date, "running daily batch");
    if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(&ctx.pool, last_date).await {
        tracing::error!(error = %e, "failed to run daily batch");
    }
    purge_old_read_notifications(&ctx.pool).await;
    spawn_knowledge_store_merge(&ctx.knowledge_store);
    run_weekly_batch_if_jst_sunday(ctx).await;
}

fn spawn_knowledge_store_merge(knowledge_store: &Option<Arc<dyn KnowledgeStore>>) {
    if let Some(store) = knowledge_store.clone() {
        tokio::spawn(async move {
            if let Err(e) = store.run_merge().await {
                tracing::warn!(error = %e, "failed to run daily knowledge store merge");
            } else {
                tracing::info!("daily knowledge store merge completed");
            }
        });
    }
}

async fn run_weekly_batch_if_jst_sunday(ctx: &DailyBatchContext) {
    if is_jst_sunday(Utc::now())
        && let Err(e) = crate::weekly_batch::run(
            &ctx.pool,
            ctx.knowledge_store.as_ref(),
            &ctx.gemini_api_url,
            &ctx.gemini_api_key,
            &ctx.gemini_model,
        )
        .await
    {
        tracing::error!(error = %e, "failed to run weekly evolution batch");
    }
}

fn is_jst_sunday(now_utc: DateTime<Utc>) -> bool {
    (now_utc + Duration::hours(JST_OFFSET_HOURS)).weekday() == chrono::Weekday::Sun
}

fn backfill_dates(today: NaiveDate, backfill_days: Option<u64>) -> Vec<NaiveDate> {
    let days = backfill_days.unwrap_or(DEFAULT_BACKFILL_DAYS) as i64;
    (1..=days)
        .rev()
        .map(|i| today - Duration::days(i))
        .collect()
}

#[cfg(test)]
mod tests;
