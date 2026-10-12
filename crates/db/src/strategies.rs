use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Strategy {
    pub name: String,
    pub display_name: String,
    pub category: String,
    pub risk_level: String,
    pub description: Option<String>,
    pub algorithm: Option<String>,
    pub default_params: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

const STRATEGY_COLUMNS: &str =
    "name, display_name, category, risk_level, description, algorithm, default_params, created_at";

pub async fn list_strategies(
    pool: &PgPool,
    category: Option<&str>,
) -> anyhow::Result<Vec<Strategy>> {
    let sql = format!(
        "SELECT {STRATEGY_COLUMNS} FROM strategies
         WHERE ($1::text IS NULL OR category = $1)
         ORDER BY category, name"
    );
    let rows = sqlx::query_as::<_, Strategy>(&sql)
        .bind(category)
        .fetch_all(pool)
        .await?;
    Ok(rows)
}

pub async fn get_strategy(pool: &PgPool, name: &str) -> anyhow::Result<Option<Strategy>> {
    let sql = format!("SELECT {STRATEGY_COLUMNS} FROM strategies WHERE name = $1");
    let row = sqlx::query_as::<_, Strategy>(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

pub async fn strategy_exists(pool: &PgPool, name: &str) -> anyhow::Result<bool> {
    let row: (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM strategies WHERE name = $1)")
        .bind(name)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

pub async fn list_strategy_names(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM strategies ORDER BY name")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

#[cfg(test)]
mod tests;
