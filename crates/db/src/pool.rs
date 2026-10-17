use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

/// Connects to the database, applies the migrations that are not yet applied and returns a pool of at most 10 connections.
///
/// # Errors
///
/// - Returns an error if the connection to `database_url` cannot be established.
/// - Returns an error if a migration cannot be applied.
/// - Returns an error if a migration recorded in the database is missing from the embedded migrations.
/// - Returns an error if a migration recorded in the database differs from the embedded one.
/// - Returns an error if a migration recorded in the database did not finish.
pub async fn create_pool(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}
