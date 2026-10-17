use auto_trader::api;

const API_BIND_ADDR: &str = "0.0.0.0:3001";

pub(crate) async fn spawn(state: api::AppState) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let app = api::router(state);
    let listener = tokio::net::TcpListener::bind(API_BIND_ADDR)
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind API server: {e}"))?;
    tracing::info!(bind_addr = %API_BIND_ADDR, "api server listening");
    Ok(tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "failed to serve api");
        }
    }))
}
