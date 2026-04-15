#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_table_exists(pool: sqlx::PgPool) -> sqlx::Result<()> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'risk_halts'
        )",
    )
    .fetch_one(&pool)
    .await?;
    assert!(row.0, "risk_halts table should exist after migrations");
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_active_partial_index_exists(pool: sqlx::PgPool) -> sqlx::Result<()> {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'risk_halts_account_active'
        )",
    )
    .fetch_one(&pool)
    .await?;
    assert!(row.0);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_one_active_per_account_unique_exists(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'risk_halts_one_active_per_account'
        )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_one_active_per_strategy_pair_unique_exists(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'trades_one_active_per_strategy_pair'
        )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.0, "partial unique index must exist");
}
