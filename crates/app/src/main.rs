mod bootstrap;
mod regime;
mod runtime;
mod startup_reconcile;
mod weekly_batch;
mod wilson;

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader::api;
use auto_trader::price_store;
use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_db::pool::create_pool;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;
use tokio::sync::mpsc;

fn exchange_from_str(s: &str) -> Option<Exchange> {
    s.parse().ok()
}

fn account_still_tradable(
    account: Option<&auto_trader_db::trading_accounts::TradingAccount>,
) -> bool {
    account.is_some_and(|a| a.active)
}

fn resolve_configured_fx_pairs(config: &AppConfig) -> Vec<Pair> {
    if !config.pairs.active.is_empty() {
        config.pairs.active.iter().map(|s| Pair::new(s)).collect()
    } else {
        config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
    }
}

async fn reconfirm_account_active_before_marking_dispatched(
    pool: &sqlx::PgPool,
    pac: &auto_trader_db::trading_accounts::TradingAccount,
) -> anyhow::Result<bool> {
    let account_now = auto_trader_db::trading_accounts::get(pool, pac.id).await?;
    Ok(account_still_tradable(account_now.as_ref()))
}

async fn count_open_bitflyer_paper_fx_btc_jpy_trades(pool: &sqlx::PgPool) -> anyhow::Result<i64> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM trades t
           JOIN trading_accounts a ON a.id = t.account_id
           WHERE t.pair = 'FX_BTC_JPY' AND t.status = 'open'
             AND a.account_type = 'paper' AND a.exchange = 'bitflyer_cfd'"#,
    )
    .fetch_one(pool)
    .await
    .map_err(anyhow::Error::from)
}

fn trade_is_subject_to_bitflyer_sfd(pair: &str) -> bool {
    pair == "FX_BTC_JPY"
}

fn trade_was_not_yet_open_at_event_time(
    entry_at: chrono::DateTime<chrono::Utc>,
    event_at: chrono::DateTime<chrono::Utc>,
) -> bool {
    entry_at > event_at
}

fn overnight_fee_applies_to_exchange(exchange: Exchange) -> bool {
    match exchange {
        Exchange::BitflyerCfd | Exchange::GmoFx => true,
        Exchange::Oanda => false,
    }
}

fn live_order_blocked_when_live_disabled_leaving_close_paths_unaffected(
    account_type: &str,
    live_enabled: bool,
) -> bool {
    account_type == "live" && !live_enabled
}

fn compute_day_start_balance_net_of_todays_realized_pnl(
    current_balance: Decimal,
    day_net: Decimal,
) -> Decimal {
    current_balance - day_net
}

fn log_swap_freshness_at_startup_leaving_slack_to_the_daily_job(
    gmo_fx_swap_config: &auto_trader_core::config::GmoFxSwapConfig,
    today: chrono::NaiveDate,
    has_gmo_paper_accounts: bool,
) {
    if let Some(body) = auto_trader::swap_freshness::swap_freshness_alert(
        gmo_fx_swap_config,
        today,
        has_gmo_paper_accounts,
    ) {
        tracing::warn!(body = %body, "invalid swap config: stale");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    bootstrap::logging::init();

    let config = bootstrap::config::load()?;

    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    let exchange_apis = bootstrap::exchange_apis::build(&config)?;

    let slack_webhook_url = std::env::var("SLACK_WEBHOOK_URL").ok();
    let notifier: Arc<Notifier> = Arc::new(Notifier::new(slack_webhook_url)?);

    let mut expected_feeds: Vec<price_store::FeedKey> = Vec::new();

    let (price_tx, price_rx) = mpsc::channel::<PriceEvent>(256);
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, trade_rx) = mpsc::channel::<TradeEvent>(256);

    const CRYPTO_TIMEFRAME: &str = "M5";
    const H1_TIMEFRAME: &str = "H1";
    const FX_TIMEFRAME: &str = "M5";
    const WARMUP_LIMIT: i64 = 200;

    let fx_monitor =
        bootstrap::fx_monitor::build(&config, &pool, FX_TIMEFRAME, &mut expected_feeds)?;

    let knowledge_store = bootstrap::knowledge_store::connect(&config.vegapunk).await;

    let mut engine =
        bootstrap::strategies::build_engine(&config, &pool, &knowledge_store, signal_tx).await?;
    bootstrap::strategies::warn_catalog_drift(&pool, &engine).await;

    let crypto_pairs_for_warmup: Vec<Pair> =
        bootstrap::feeds::resolve_configured_crypto_pairs(&config);
    let fx_pairs_for_warmup: Vec<Pair> = resolve_configured_fx_pairs(&config);
    let mut bitflyer_seed = runtime::warmup::feed_history(
        &pool,
        &mut engine,
        &crypto_pairs_for_warmup,
        &fx_pairs_for_warmup,
        runtime::warmup::WarmupTimeframes {
            crypto: CRYPTO_TIMEFRAME,
            h1: H1_TIMEFRAME,
            fx: FX_TIMEFRAME,
            limit: WARMUP_LIMIT,
        },
    )
    .await;

    let registered_strategies = bootstrap::strategies::collect_registered_names(&engine);

    let db_accounts =
        bootstrap::accounts::load_and_log(&pool, &registered_strategies, &config.gmo_fx.swap)
            .await?;

    let live = bootstrap::live_flags::resolve(&config);
    bootstrap::live_flags::log_ignored_position_sizing(&config);

    let pair_configs: Arc<HashMap<String, auto_trader_core::config::PairConfig>> =
        Arc::new(config.pair_config.clone());

    let exchange_liquidation_levels: Arc<HashMap<auto_trader_core::types::Exchange, Decimal>> =
        Arc::new(auto_trader::startup::resolve_exchange_liquidation_levels(
            &db_accounts,
            &config,
        )?);

    bootstrap::fx_new_assumptions::alert_mismatches(&db_accounts, &notifier);

    let risk = config.risk.clone().unwrap_or_default();
    let sizing_margin_buffer: Decimal = risk.sizing_margin_buffer;
    let shared_position_sizer =
        bootstrap::position_sizer::build(&pair_configs, sizing_margin_buffer);

    let price_freshness_secs: u64 = risk.price_freshness_secs;

    let daily_loss_limit_pct: Decimal = risk.daily_loss_limit_pct;
    let halt_hours: u64 = risk.halt_hours;

    let feeds = bootstrap::feeds::build(
        &config,
        &pool,
        fx_monitor,
        &mut bitflyer_seed,
        bootstrap::feeds::FeedTimeframes {
            crypto: CRYPTO_TIMEFRAME,
            fx: FX_TIMEFRAME,
        },
        &mut expected_feeds,
    );
    let exchange_pairs = bootstrap::feeds::group_pairs_by_exchange(&expected_feeds);
    let price_store = price_store::PriceStore::new(expected_feeds);
    let feed_handles = bootstrap::feeds::spawn_all(
        feeds,
        &price_store,
        &price_tx,
        &notifier,
        &config.feed_watchdog,
    );

    let (macro_tx, _) =
        tokio::sync::broadcast::channel::<auto_trader_core::strategy::MacroUpdate>(16);
    let macro_rx = macro_tx.subscribe();
    let macro_analyst_handle =
        bootstrap::macro_analyst::spawn(&config, &pool, &knowledge_store, macro_tx.clone())?;

    bootstrap::live_accounts::reconcile(
        &pool,
        &db_accounts,
        &exchange_apis,
        price_store.clone(),
        notifier.clone(),
    )
    .await?;

    let drift_ctx = auto_trader::balance_drift::BalanceDriftContext {
        pool: pool.clone(),
        price_store: price_store.clone(),
        apis: exchange_apis.clone(),
        live_forces_dry_run: live.forces_dry_run,
    };
    runtime::balance_drift_loop::check_and_alert(&drift_ctx, &notifier).await;

    let pos_monitor_handle = runtime::fx_price_drain::spawn(price_monitor_rx);

    let (crypto_price_tx, crypto_price_rx) = mpsc::channel::<PriceEvent>(256);
    let new_close_ctx = || auto_trader::closer::CloseContext {
        pool: pool.clone(),
        apis: exchange_apis.clone(),
        price_store: price_store.clone(),
        notifier: notifier.clone(),
        position_sizer: shared_position_sizer.clone(),
        liquidation_levels: exchange_liquidation_levels.clone(),
        trade_tx: trade_tx.clone(),
    };
    let crypto_monitor_handle = tokio::spawn(runtime::crypto_monitor::run(
        new_close_ctx(),
        live.forces_dry_run,
        crypto_price_rx,
    ));

    let (exit_tx, exit_rx) = mpsc::channel::<auto_trader_core::strategy::ExitSignal>(64);

    let engine_handle = tokio::spawn(runtime::engine_loop::run(
        runtime::engine_loop::EngineLoopContext {
            engine,
            pool: pool.clone(),
            price_store: price_store.clone(),
            price_monitor_tx,
            crypto_price_tx,
            exit_tx,
        },
        price_rx,
        macro_rx,
    ));

    let exit_executor_handle = tokio::spawn(runtime::exit_executor::run(
        new_close_ctx(),
        live.forces_dry_run,
        exit_rx,
    ));

    let ctx = runtime::executor_loop::ExecutorLoopContext {
        pool: pool.clone(),
        trade_tx: trade_tx.clone(),
        exchange_apis: exchange_apis.clone(),
        price_store: price_store.clone(),
        notifier: notifier.clone(),
        exchange_pairs: Arc::clone(&exchange_pairs),
        position_sizer: shared_position_sizer.clone(),
        exchange_liquidation_levels: exchange_liquidation_levels.clone(),
        live_forces_dry_run: live.forces_dry_run,
        live_enabled: live.enabled,
        price_freshness_secs,
        daily_loss_limit_pct,
        halt_hours,
        knowledge_store: knowledge_store.clone(),
    };
    let executor_handle = tokio::spawn(runtime::executor_loop::run(ctx, signal_rx));

    let recorder_handle = tokio::spawn(runtime::recorder::run(
        pool.clone(),
        knowledge_store.clone(),
        trade_rx,
    ));

    let daily_handle = tokio::spawn(runtime::daily_batch::run(
        runtime::daily_batch::DailyBatchContext {
            pool: pool.clone(),
            knowledge_store: knowledge_store.clone(),
            gemini_api_url: config
                .gemini
                .as_ref()
                .map(|g| g.api_url.clone())
                .unwrap_or_default(),
            gemini_api_key: std::env::var("GEMINI_API_KEY").unwrap_or_default(),
            gemini_model: config
                .gemini
                .as_ref()
                .map(|g| g.model.clone())
                .unwrap_or_default(),
            backfill_days: config.monitor.backfill_days,
        },
    ));

    let stop_fill_handle = tokio::spawn(runtime::stop_fill_loop::run(
        new_close_ctx(),
        live.forces_dry_run,
    ));

    let balance_drift_handle = tokio::spawn(runtime::balance_drift_loop::run(
        drift_ctx,
        notifier.clone(),
    ));

    let feed_watchdog_handle = if config.feed_watchdog.enabled {
        let feed_watchdog_ctx = auto_trader::feed_watchdog::FeedWatchdogContext {
            price_store: price_store.clone(),
            notifier: notifier.clone(),
            config: config.feed_watchdog.clone(),
        };
        Some(tokio::spawn(auto_trader::feed_watchdog::run(
            feed_watchdog_ctx,
        )))
    } else {
        tracing::info!("feed watchdog disabled ([feed_watchdog].enabled=false)");
        None
    };

    let overnight_handle = tokio::spawn(runtime::overnight::run(
        pool.clone(),
        config.gmo_fx.swap.clone(),
        notifier.clone(),
    ));

    let sfd_handle = tokio::spawn(runtime::sfd_accrual::run(pool.clone(), price_store.clone()));

    let api_handle = runtime::api_server::spawn(api::AppState {
        pool: pool.clone(),
        price_store: price_store.clone(),
        exchange_liquidation_levels: exchange_liquidation_levels.clone(),
    })
    .await?;

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");
    tokio::signal::ctrl_c().await?;
    runtime::shutdown::run(
        runtime::shutdown::TaskHandles {
            feed_handles,
            overnight: overnight_handle,
            sfd: sfd_handle,
            stop_fill: stop_fill_handle,
            balance_drift: balance_drift_handle,
            feed_watchdog: feed_watchdog_handle,
            daily: daily_handle,
            macro_analyst: macro_analyst_handle,
            api: api_handle,
            engine: engine_handle,
            pos_monitor: pos_monitor_handle,
            crypto_monitor: crypto_monitor_handle,
            exit_executor: exit_executor_handle,
            executor: executor_handle,
            recorder: recorder_handle,
        },
        price_tx,
        trade_tx,
    )
    .await;
    Ok(())
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
