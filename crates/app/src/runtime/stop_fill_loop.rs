use auto_trader::closer::CloseContext;
use auto_trader::stop_fill::detect_and_close_stop_fills;

pub(crate) async fn run(close_ctx: CloseContext, live_forces_dry_run: bool) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        let open_trades = match auto_trader_db::trades::list_open_with_account_name(&close_ctx.pool)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "failed to list open trades for stop-fill detection");
                continue;
            }
        };
        detect_and_close_stop_fills(&close_ctx, &open_trades, live_forces_dry_run).await;
    }
}
