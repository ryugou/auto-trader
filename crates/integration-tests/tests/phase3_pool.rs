//! Phase 3: DB pool exhaustion test (3.114).
//!
//! Create a pool with max_connections=1, hold one connection,
//! try to acquire another with timeout, verify timeout behavior.

use std::time::Duration;

#[sqlx::test(migrations = "../../migrations")]
async fn pool_exhaustion_timeout(pool: sqlx::PgPool) {
    // sqlx::test creates a pool with default settings.
    // We need a pool with max_connections=1 to test exhaustion.
    // Extract the DATABASE_URL from the test pool by running a query
    // that returns the connection info, then create a constrained pool.

    // Get the connection string from the test pool
    let db_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .expect("should get db name");

    // Extract host/port from the test pool by querying inet_server_addr/port.
    // sqlx::test may create a temporary database on the same server.
    // Use the DATABASE_URL env var as the base, replacing only the db name.
    let base_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgresql://auto-trader:auto-trader@localhost:15432/auto_trader".to_string());

    // Replace the database name in the URL
    let constrained_url = if let Some(last_slash) = base_url.rfind('/') {
        format!("{}/{}", &base_url[..last_slash], db_name)
    } else {
        format!("{}/{}", base_url, db_name)
    };

    let constrained_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&constrained_url)
        .await
        .expect("should connect constrained pool");

    // Acquire the only connection
    let _held_conn = constrained_pool
        .acquire()
        .await
        .expect("first acquire should succeed");

    // Try to acquire a second connection — should timeout
    let start = std::time::Instant::now();
    let result = constrained_pool.acquire().await;
    let elapsed = start.elapsed();

    assert!(
        result.is_err(),
        "second acquire should fail (pool exhausted)"
    );

    // Verify it actually waited for the timeout (not instant failure)
    assert!(
        elapsed >= Duration::from_secs(1),
        "should have waited at least 1s for timeout, elapsed: {:?}",
        elapsed
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "should not have waited more than 5s, elapsed: {:?}",
        elapsed
    );

    // After dropping the held connection, acquiring should work again
    drop(_held_conn);

    let _conn = constrained_pool
        .acquire()
        .await
        .expect("acquire should succeed after releasing held connection");
}
