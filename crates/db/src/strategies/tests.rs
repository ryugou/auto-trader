use sqlx::PgPool;

use super::*;

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
