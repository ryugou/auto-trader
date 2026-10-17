use super::*;

#[sqlx::test(migrations = "../../migrations")]
async fn harness_initializes_with_seeded_account(pool: PgPool) {
    let harness = PipelineHarness::new(
        pool.clone(),
        PipelineHarnessConfig {
            account_name: "harness_init_test".to_string(),
            exchange: Exchange::GmoFx,
            pair_str: "USD_JPY".to_string(),
            strategy: "test_strategy".to_string(),
            balance: 30_000,
            liquidation_margin_level: dec!(1.00),
            min_order_size: dec!(1),
        },
    )
    .await
    .expect("failed to build pipeline harness");

    let row: (Decimal,) =
        sqlx::query_as("SELECT current_balance FROM trading_accounts WHERE id = $1")
            .bind(harness.account_id)
            .fetch_one(&pool)
            .await
            .expect("seeded account must exist");
    assert_eq!(row.0, Decimal::from(30_000));
}
