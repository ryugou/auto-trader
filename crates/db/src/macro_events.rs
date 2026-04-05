use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert_macro_event(
    pool: &PgPool,
    summary: &str,
    event_type: &str,
    impact: &str,
    event_at: DateTime<Utc>,
    source: Option<&str>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO macro_events (id, summary, event_type, impact, event_at, source)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(summary)
    .bind(event_type)
    .bind(impact)
    .bind(event_at)
    .bind(source)
    .execute(pool)
    .await?;
    Ok(id)
}
