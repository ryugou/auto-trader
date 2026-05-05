//! Test API server helper.
//!
//! `spawn_test_app` starts the auto-trader API server in-process on an
//! ephemeral port and returns the base URL + a JoinHandle for cleanup.

use auto_trader::api::{self, AppState};
use auto_trader_core::types::Exchange;
use auto_trader_market::price_store::PriceStore;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// A running test API server.
pub struct TestApp {
    /// Base URL including scheme and port, e.g. `http://127.0.0.1:12345`.
    pub url: String,
    /// Handle to the background server task. Drop or abort to shut down.
    pub handle: JoinHandle<()>,
    /// Shared PriceStore — tests can insert ticks before making requests.
    pub price_store: Arc<PriceStore>,
}

impl TestApp {
    /// Convenience: build a reqwest client pre-configured with the base URL.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Build a full endpoint URL, e.g. `self.endpoint("/api/trading-accounts")`.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Default liquidation levels for tests — covers every Exchange variant so
/// existing test fixtures (gmo_fx / bitflyer_cfd / oanda) all pass the
/// `accounts::create` defense-in-depth check that rejects unknown exchanges.
///
/// Values mirror `config/default.toml` so tests don't accidentally exercise
/// a different sizing regime than production.
fn default_liquidation_levels() -> Arc<HashMap<Exchange, rust_decimal::Decimal>> {
    let mut m = HashMap::new();
    m.insert(Exchange::BitflyerCfd, dec!(0.50));
    m.insert(Exchange::GmoFx, dec!(1.00));
    m.insert(Exchange::Oanda, dec!(1.00));
    Arc::new(m)
}

/// Start the API server in-process on an ephemeral port.
///
/// The server uses the given DB pool and an empty PriceStore (no expected
/// feeds). Tests that need price data should call `price_store.update()`
/// before making requests.
pub async fn spawn_test_app(pool: PgPool) -> TestApp {
    spawn_test_app_with_price_store(pool, PriceStore::new(vec![])).await
}

/// Start the API server with a custom PriceStore (for health endpoint tests
/// that need expected feeds).
pub async fn spawn_test_app_with_price_store(
    pool: PgPool,
    price_store: Arc<PriceStore>,
) -> TestApp {
    spawn_test_app_with_levels(pool, price_store, default_liquidation_levels()).await
}

/// Start the API server with a custom liquidation-levels map. Used by tests
/// that exercise the `accounts::create` validation against a constrained
/// (e.g. single-exchange) config.
pub async fn spawn_test_app_with_levels(
    pool: PgPool,
    price_store: Arc<PriceStore>,
    exchange_liquidation_levels: Arc<HashMap<Exchange, rust_decimal::Decimal>>,
) -> TestApp {
    let state = AppState {
        pool,
        price_store: price_store.clone(),
        exchange_liquidation_levels,
    };

    let router = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind ephemeral port");
    let addr = listener.local_addr().expect("failed to get local addr");
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("server error");
    });

    // Wait for the server to start accepting connections (TCP retry loop).
    let mut connected = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            connected = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    if !connected {
        panic!("test app server did not start within 250ms at {}", addr);
    }

    TestApp {
        url,
        handle,
        price_store,
    }
}
