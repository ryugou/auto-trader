//! Phase 2: Auth middleware tests.
//!
//! Isolated in a separate test binary because auth tests mutate process-wide
//! env vars (API_TOKEN) which would interfere with parallel test execution.

use auto_trader_integration_tests::helpers::app;
use std::sync::Mutex;

/// Serialises env-var mutations across tests (process-wide state).
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// RAII guard that removes `API_TOKEN` on drop — ensures cleanup even on panic.
struct EnvGuard;

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("API_TOKEN") };
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_no_token_configured_allows_all(pool: sqlx::PgPool) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::remove_var("API_TOKEN") };
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_valid_token(pool: sqlx::PgPool) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let _env_guard = EnvGuard;
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 200);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_missing_token_returns_401(pool: sqlx::PgPool) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let _env_guard = EnvGuard;
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_token_returns_401(pool: sqlx::PgPool) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let _env_guard = EnvGuard;
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Bearer wrong-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);
}

#[sqlx::test(migrations = "../../migrations")]
async fn auth_invalid_format_returns_401(pool: sqlx::PgPool) {
    let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    unsafe { std::env::set_var("API_TOKEN", "test-secret-token") };
    let _env_guard = EnvGuard;
    let app = app::spawn_test_app(pool).await;
    let client = app.client();

    let resp = client
        .get(app.endpoint("/api/strategies"))
        .header("Authorization", "Basic test-secret-token")
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status().as_u16(), 401);
}
