use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

/// Strategy catalog row. The catalog is metadata only — the trading engine
/// still loads enabled strategies, modes, and parameters from
/// `config/default.toml`. See `migrations/20260407000003_strategies.sql`
/// for the rationale.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Strategy {
    pub name: String,
    pub display_name: String,
    pub category: String,
    /// 'low' | 'medium' | 'high' — used by the UI to render a risk badge.
    /// Note: the unified_rewrite migration (PR-1) does NOT add a DB-level
    /// CHECK constraint for these values; the TS union type + seed-data
    /// discipline is currently the only enforcement.
    pub risk_level: String,
    pub description: Option<String>,
    pub algorithm: Option<String>,
    pub default_params: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
}

const STRATEGY_COLUMNS: &str =
    "name, display_name, category, risk_level, description, algorithm, default_params, created_at";

/// List all strategies in the catalog. Optionally filter by category
/// (`fx` / `crypto`) so the account-creation UI can scope the dropdown to
/// strategies compatible with the chosen exchange.
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

/// Look up a single strategy by name. Returns `None` when missing.
pub async fn get_strategy(pool: &PgPool, name: &str) -> anyhow::Result<Option<Strategy>> {
    let sql = format!("SELECT {STRATEGY_COLUMNS} FROM strategies WHERE name = $1");
    let row = sqlx::query_as::<_, Strategy>(&sql)
        .bind(name)
        .fetch_optional(pool)
        .await?;
    Ok(row)
}

/// Cheap "does this strategy name exist in the catalog?" check used by the
/// paper-account create/update path so users can't store references to
/// strategies that the runtime cannot resolve.
pub async fn strategy_exists(pool: &PgPool, name: &str) -> anyhow::Result<bool> {
    let row: (bool,) = sqlx::query_as("SELECT EXISTS (SELECT 1 FROM strategies WHERE name = $1)")
        .bind(name)
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

/// Return all strategy names registered in the catalog. Used at startup to
/// detect drift between `config/default.toml` and the strategies table.
pub async fn list_strategy_names(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String,)> = sqlx::query_as("SELECT name FROM strategies ORDER BY name")
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|(name,)| name).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::PgPool;

    /// Regression: strategies rows with NULL description / algorithm /
    /// default_params must deserialize cleanly into the Rust struct.
    /// Before this fix, the struct required non-null values and
    /// `/api/strategies` returned 500 on any row with NULLs.
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_strategies_deserializes_null_fields(pool: PgPool) {
        sqlx::query(
            "INSERT INTO strategies (name, display_name, category, risk_level,
                                      description, algorithm, default_params)
             VALUES ('test_null', 'Test Null', 'crypto', 'low', NULL, NULL, NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows = list_strategies(&pool, None).await.unwrap();
        let row = rows.iter().find(|r| r.name == "test_null").unwrap();
        assert!(row.description.is_none());
        assert!(row.algorithm.is_none());
        assert!(row.default_params.is_none());

        let one = get_strategy(&pool, "test_null").await.unwrap().unwrap();
        assert!(one.description.is_none());
        assert!(one.algorithm.is_none());
        assert!(one.default_params.is_none());
    }
}
