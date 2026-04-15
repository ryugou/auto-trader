// risk_halts table and trades_one_active_per_strategy_pair index were removed
// in migration 20260417000001_revert_pr2_unused.sql. The tests below verify
// that the revert migration applied correctly.

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
