mod bootstrap;
mod regime;
mod runtime;
mod startup_reconcile;
mod weekly_batch;
mod wilson;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::logging::init();
    let config = bootstrap::config::load()?;
    let services = bootstrap::services::connect(&config).await?;
    let runtime::channels::Channels {
        senders,
        pipeline: pipeline_channels,
    } = runtime::channels::open();

    let engine_setup =
        bootstrap::engine_setup::build(&config, &services.pool, senders.signal_tx).await?;
    let accounts = bootstrap::account_setup::resolve(
        &config,
        &services.pool,
        &engine_setup.registered_strategies,
        &services.notifier,
    )
    .await?;
    let feeds = bootstrap::feeds::start(
        &config,
        &services.pool,
        engine_setup.fx_monitor,
        engine_setup.bitflyer_seed,
        engine_setup.expected_feeds,
        &senders.price_tx,
        &services.notifier,
    );
    let macro_analyst = bootstrap::macro_analyst::spawn(
        &config,
        &services.pool,
        &engine_setup.knowledge_store,
        senders.macro_tx.clone(),
    )?;

    let dependencies = runtime::dependencies::SharedDependencies {
        pool: services.pool,
        exchange_apis: services.exchange_apis,
        price_store: feeds.price_store,
        notifier: services.notifier,
        position_sizer: accounts.position_sizer,
        liquidation_levels: accounts.liquidation_levels,
        knowledge_store: engine_setup.knowledge_store,
    };
    let drift_ctx = bootstrap::startup_checks::run(
        &dependencies,
        &accounts.db_accounts,
        accounts.live.forces_dry_run,
    )
    .await?;
    let pipeline = runtime::pipeline::spawn_all(
        &dependencies,
        &senders.trade_tx,
        runtime::pipeline::PipelineInputs {
            engine: engine_setup.engine,
            exchange_pairs: feeds.exchange_pairs,
            live: accounts.live,
            limits: accounts.limits,
            channels: pipeline_channels,
        },
    );
    let background = runtime::background::spawn_all(
        &dependencies,
        &senders.trade_tx,
        &config,
        drift_ctx,
        accounts.live.forces_dry_run,
    )
    .await?;

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    runtime::shutdown::run(
        runtime::shutdown::TaskHandles {
            feed_handles: feeds.feed_handles,
            macro_analyst,
            pipeline,
            background,
        },
        senders.price_tx,
        senders.trade_tx,
    )
    .await;
    Ok(())
}
