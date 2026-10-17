use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Row of the `strategies` table: a trading strategy and its default parameters.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Strategy {
    /// Name of the strategy.
    pub name: String,
    /// Name shown to users.
    pub display_name: String,
    /// Category of the strategy.
    pub category: String,
    /// Risk level of the strategy as text.
    pub risk_level: String,
    /// Description of the strategy, if any.
    pub description: Option<String>,
    /// Description of the algorithm, if any.
    pub algorithm: Option<String>,
    /// Default parameters of the strategy as JSON, if any.
    pub default_params: Option<serde_json::Value>,
    /// Time the strategy was registered.
    pub created_at: DateTime<Utc>,
}

const STRATEGY_COLUMNS: &str =
    "name, display_name, category, risk_level, description, algorithm, default_params, created_at";

/// Returns the strategies, ordered by category and then name. When `category` is given, only the strategies of that category are returned.
///
/// # Errors
///
/// - Returns an error if the database query fails.
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

/// Returns the strategy with the given name, or `None` if there is none.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn get_strategy(pool: &PgPool, name: &str) -> anyhow::Result<Option<Strategy>> {
    let sql = format!("SELECT {STRATEGY_COLUMNS} FROM strategies WHERE name = $1");
    let row = sqlx::query_as::<_, Strategy>(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Returns whether a strategy with the given name exists.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn strategy_exists(pool: &PgPool, name: &str) -> anyhow::Result<bool> {
    let row: (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM strategies WHERE name = $1)")
        .bind(name)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Returns the names of all strategies in ascending order.
///
/// # Errors
///
/// - Returns an error if the database query fails.
pub async fn list_strategy_names(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM strategies ORDER BY name")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

#[cfg(test)]
mod tests;
