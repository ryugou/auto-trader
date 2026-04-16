// Verify: (1) revert migration dropped risk_halts + trades_one_active_per_strategy_pair,
// (2) one_live_per_exchange migration added the partial unique index.

#[sqlx::test(migrations = "../../migrations")]
async fn trading_accounts_one_live_per_exchange_unique_exists(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM pg_indexes
         WHERE indexname = 'trading_accounts_one_live_per_exchange')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        row.0,
        "partial unique index trading_accounts_one_live_per_exchange should exist"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn risk_halts_table_is_dropped_after_revert_migration(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM information_schema.tables
            WHERE table_name = 'risk_halts'
        )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        !row.0,
        "risk_halts should be dropped after revert migration"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn trades_one_active_per_strategy_pair_index_is_dropped(pool: sqlx::PgPool) {
    let row: (bool,) = sqlx::query_as(
        "SELECT EXISTS (
            SELECT 1 FROM pg_indexes
            WHERE indexname = 'trades_one_active_per_strategy_pair'
        )",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!row.0, "duplicate-position unique index should be dropped");
}
