use auto_trader_sim::data::{ensure_tables, load_bars, upsert_bars};
use auto_trader_sim::error::SimError;
use auto_trader_sim::types::Bar;

fn flat_bar(open_time: i64, bid: i64, ask: i64) -> Bar {
    Bar {
        open_time,
        bid_open: bid,
        bid_high: bid,
        bid_low: bid,
        bid_close: bid,
        ask_open: ask,
        ask_high: ask,
        ask_low: ask,
        ask_close: ask,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn upsert_then_load_round_trips_values_in_ascending_open_time_order(pool: sqlx::PgPool) {
    let bars = vec![
        flat_bar(2_000, 149_700_000, 149_710_000),
        flat_bar(1_000, 149_600_000, 149_610_000),
    ];
    let affected = upsert_bars(&pool, &bars)
        .await
        .expect("inserting two new bars must succeed");
    assert_eq!(affected, 2);

    let loaded = load_bars(&pool, None)
        .await
        .expect("loading all bars must succeed");
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0], flat_bar(1_000, 149_600_000, 149_610_000));
    assert_eq!(loaded[1], flat_bar(2_000, 149_700_000, 149_710_000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn upsert_is_idempotent_and_updates_on_changed_values(pool: sqlx::PgPool) {
    let original = vec![flat_bar(1_000, 149_600_000, 149_610_000)];
    upsert_bars(&pool, &original).await.unwrap();
    upsert_bars(&pool, &original).await.unwrap();

    let loaded = load_bars(&pool, None).await.unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "re-upserting the same primary key must not create a duplicate row"
    );

    let changed = vec![flat_bar(1_000, 149_700_000, 149_720_000)];
    upsert_bars(&pool, &changed).await.unwrap();
    let loaded = load_bars(&pool, None).await.unwrap();
    assert_eq!(
        loaded.len(),
        1,
        "still one row after re-upsert with new values"
    );
    assert_eq!(loaded[0], flat_bar(1_000, 149_700_000, 149_720_000));
}

#[sqlx::test(migrations = "../../migrations")]
async fn load_bars_with_to_exclusive_filters_by_open_time(pool: sqlx::PgPool) {
    let bars = vec![
        flat_bar(1_000, 149_600_000, 149_610_000),
        flat_bar(2_000, 149_700_000, 149_710_000),
        flat_bar(3_000, 149_800_000, 149_810_000),
    ];
    upsert_bars(&pool, &bars).await.unwrap();

    let loaded = load_bars(&pool, Some(2_000)).await.unwrap();
    assert_eq!(
        loaded.iter().map(|b| b.open_time).collect::<Vec<_>>(),
        vec![1_000],
        "to_exclusive=2000 must keep only open_time < 2000"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn ensure_tables_returns_ok_on_a_freshly_migrated_db(pool: sqlx::PgPool) {
    ensure_tables(&pool)
        .await
        .expect("all 4 sim_* tables must exist right after migration");
}

#[sqlx::test(migrations = "../../migrations")]
async fn ensure_tables_reports_the_missing_table_by_name(pool: sqlx::PgPool) {
    sqlx::query("DROP TABLE sim_runs")
        .execute(&pool)
        .await
        .expect("drop must succeed as a test precondition");

    let err = ensure_tables(&pool).await.expect_err("sim_runs is missing");
    match err {
        SimError::MissingTables(missing) => {
            assert_eq!(missing, vec!["sim_runs".to_string()]);
        }
        other => panic!("expected SimError::MissingTables, got {other:?}"),
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn ensure_tables_reports_multiple_missing_tables_in_required_order(pool: sqlx::PgPool) {
    for table in ["sim_runs", "sim_candles"] {
        sqlx::query(&format!("DROP TABLE {table}"))
            .execute(&pool)
            .await
            .expect("drop must succeed as a test precondition");
    }

    let err = ensure_tables(&pool)
        .await
        .expect_err("two tables are missing");
    match err {
        SimError::MissingTables(missing) => {
            assert_eq!(
                missing,
                vec!["sim_candles".to_string(), "sim_runs".to_string()],
                "must follow REQUIRED_TABLES order, not drop order"
            );
        }
        other => panic!("expected SimError::MissingTables, got {other:?}"),
    }
}
