use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use auto_trader::api::{self, AppState};
use auto_trader_core::types::Exchange;
use auto_trader_market::price_store::PriceStore;
use rust_decimal_macros::dec;
use sqlx::PgPool;
use tokio::task::JoinHandle;

/// API server that runs in the test process on an ephemeral port.
pub struct TestApp {
    /// Base URL of the server, made of the scheme, the host and the port.
    pub url: String,
    /// Handle of the background task that serves the API; dropping the `TestApp` aborts the task.
    pub handle: JoinHandle<()>,
    /// Price store that the server reads; tests insert ticks into it before they send requests.
    pub price_store: Arc<PriceStore>,
}

impl TestApp {
    /// Returns a new `reqwest::Client` with the default settings.
    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::new()
    }

    /// Returns the URL of `path` on the server, built by appending `path` to `url`.
    pub fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn liquidation_levels_covering_every_exchange() -> Arc<HashMap<Exchange, rust_decimal::Decimal>> {
    let mut m = HashMap::new();
    m.insert(Exchange::BitflyerCfd, dec!(0.50));
    m.insert(Exchange::GmoFx, dec!(1.00));
    m.insert(Exchange::Oanda, dec!(1.00));
    Arc::new(m)
}

/// Starts the API server in the test process on an ephemeral port, with the given database pool, an empty price store and liquidation levels for every exchange.
///
/// # Errors
///
/// - Returns an error if binding the port fails.
/// - Returns an error if reading the local address of the listener fails.
///
/// # Panics
///
/// Panics if the server does not accept a connection within 250 ms.
pub async fn spawn_test_app(pool: PgPool) -> anyhow::Result<TestApp> {
    spawn_test_app_with_price_store(pool, PriceStore::new(vec![])).await
}

/// Starts the API server as `spawn_test_app` does, with the given price store.
///
/// # Errors
///
/// - Returns an error if binding the port fails.
/// - Returns an error if reading the local address of the listener fails.
///
/// # Panics
///
/// Panics if the server does not accept a connection within 250 ms.
pub async fn spawn_test_app_with_price_store(
    pool: PgPool,
    price_store: Arc<PriceStore>,
) -> anyhow::Result<TestApp> {
    spawn_test_app_with_levels(
        pool,
        price_store,
        liquidation_levels_covering_every_exchange(),
    )
    .await
}

/// Starts the API server as `spawn_test_app` does, with the given price store and the given liquidation level of each exchange.
///
/// # Errors
///
/// - Returns an error if binding the port fails.
/// - Returns an error if reading the local address of the listener fails.
///
/// # Panics
///
/// Panics if the server does not accept a connection within 250 ms.
pub async fn spawn_test_app_with_levels(
    pool: PgPool,
    price_store: Arc<PriceStore>,
    exchange_liquidation_levels: Arc<HashMap<Exchange, rust_decimal::Decimal>>,
) -> anyhow::Result<TestApp> {
    let state = AppState {
        pool,
        price_store: price_store.clone(),
        exchange_liquidation_levels,
    };

    let router = api::router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind ephemeral port")?;
    let addr = listener.local_addr().context("failed to get local addr")?;
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            panic!("failed to serve test app: {e}");
        }
    });

    wait_until_accepting_connections(addr).await;

    Ok(TestApp {
        url,
        handle,
        price_store,
    })
}

async fn wait_until_accepting_connections(addr: SocketAddr) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("failed to start test app server: no connection accepted within 250ms at {addr}");
}
