mod regime;
mod runtime;
mod startup_reconcile;
mod weekly_batch;
mod wilson;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use auto_trader::api;
use auto_trader::price_store;
use auto_trader::spawn_system_alerts;
use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent};
use auto_trader_core::types::{Exchange, Pair};
use auto_trader_db::pool::create_pool;
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::market_feed::MarketFeed;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_market::oanda_private::OandaPrivateApi;
use auto_trader_notify::{Notifier, NotifyEvent, SystemAlertEvent};
use auto_trader_strategy::engine::StrategyEngine;
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

fn parse_live_dry_run_override(raw: Option<&str>, config_dry_run: bool) -> bool {
    match raw {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                tracing::warn!(
                    value = ?raw,
                    fallback = config_dry_run,
                    "invalid env var \"LIVE_DRY_RUN\": unrecognized value, accepted values are \
                     1/true/yes/on/0/false/no/off, falling back to configured value"
                );
                config_dry_run
            }
        },
        None => config_dry_run,
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

fn spawn_fx_price_drain_since_fx_position_monitor_is_disabled(
    mut rx: mpsc::Receiver<PriceEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { while rx.recv().await.is_some() {} })
}

async fn reconcile_live_accounts_before_spawning_any_trading_task(
    pool: &sqlx::PgPool,
    db_accounts: &[auto_trader_db::trading_accounts::TradingAccount],
    exchange_apis: &HashMap<Exchange, Arc<dyn ExchangeApi>>,
    price_store: Arc<auto_trader::price_store::PriceStore>,
    notifier: Arc<Notifier>,
) -> anyhow::Result<()> {
    startup_reconcile::reconcile_live_accounts_at_startup(
        pool,
        db_accounts,
        exchange_apis,
        price_store,
        notifier,
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "failed to reconcile live accounts at startup: {e}; refusing to start with potentially inconsistent state"
        )
    })
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path =
        std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config/default.toml".to_string());
    let mut config = AppConfig::load(&PathBuf::from(&config_path))?;
    tracing::info!(config_path = %config_path, "config loaded");
    if let Some(bf) = config.bitflyer.as_mut() {
        bf.api_key = std::env::var("BITFLYER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        bf.api_secret = std::env::var("BITFLYER_API_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
    }

    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    let bitflyer_api: Arc<BitflyerPrivateApi> =
        Arc::new(if let Some(bf) = config.bitflyer.as_ref() {
            let api_key = bf.api_key.clone().unwrap_or_default();
            let api_secret = bf.api_secret.clone().unwrap_or_default();
            BitflyerPrivateApi::new(api_key, api_secret)?
        } else {
            BitflyerPrivateApi::new(String::new(), String::new())?
        });

    let mut exchange_apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
    exchange_apis.insert(Exchange::BitflyerCfd, bitflyer_api.clone());

    fn resolve_oanda_account_id(config: &auto_trader_core::config::AppConfig) -> Option<String> {
        std::env::var("OANDA_ACCOUNT_ID")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                config
                    .oanda
                    .as_ref()
                    .map(|c| c.account_id.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
    }

    fn resolve_oanda_api_key() -> Option<String> {
        std::env::var("OANDA_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    let oanda_account_id = resolve_oanda_account_id(&config);
    let oanda_api_key = resolve_oanda_api_key();
    if let (Some(account_id), Some(api_key), Some(oanda_config)) =
        (oanda_account_id, oanda_api_key, config.oanda.as_ref())
    {
        let oanda_api: Arc<dyn ExchangeApi> = Arc::new(OandaPrivateApi::new(
            oanda_config.api_url.clone(),
            account_id,
            api_key,
        )?);
        exchange_apis.insert(Exchange::Oanda, oanda_api);
        tracing::info!(exchange = %Exchange::Oanda, "exchange api registered");
    } else {
        tracing::info!(
            exchange = %Exchange::Oanda,
            "no exchange api registered for exchange, requires OANDA_API_KEY env, an account id \
             (OANDA_ACCOUNT_ID env or [oanda].account_id config), and [oanda].api_url in config"
        );
    }

    fn env_trimmed(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    match (env_trimmed("GMO_API_KEY"), env_trimmed("GMO_API_SECRET")) {
        (Some(key), Some(secret)) => {
            let gmo_api: Arc<dyn ExchangeApi> = Arc::new(
                auto_trader_market::gmo_fx_private::GmoFxPrivateApi::new(key, secret)?,
            );
            exchange_apis.insert(Exchange::GmoFx, gmo_api);
            tracing::info!(exchange = %Exchange::GmoFx, "exchange api registered");
        }
        _ => tracing::info!(
            exchange = %Exchange::GmoFx,
            "no exchange api registered for exchange, requires GMO_API_KEY and GMO_API_SECRET env"
        ),
    }

    let exchange_apis = Arc::new(exchange_apis);

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

    let fx_pairs: Vec<Pair> = resolve_configured_fx_pairs(&config);
    let fx_pairs_configured = !fx_pairs.is_empty();
    let fx_monitor: Option<MarketMonitor> = if !fx_pairs.is_empty() {
        if let (Some(api_key), Some(oanda_config)) =
            (resolve_oanda_api_key(), config.oanda.as_ref())
        {
            match resolve_oanda_account_id(&config) {
                Some(account_id) => {
                    for p in &fx_pairs {
                        expected_feeds.push(price_store::FeedKey::new(
                            auto_trader_core::types::Exchange::Oanda,
                            p.clone(),
                        ));
                    }
                    let oanda = OandaClient::new(&oanda_config.api_url, &account_id, &api_key)?;
                    Some(
                        MarketMonitor::new(
                            oanda,
                            fx_pairs,
                            config.monitor.interval_secs,
                            FX_TIMEFRAME,
                        )
                        .with_db(pool.clone()),
                    )
                }
                None => {
                    tracing::warn!(
                        exchange = %Exchange::Oanda,
                        "invalid oanda account id: not set"
                    );
                    None
                }
            }
        } else {
            tracing::info!(
                exchange = %Exchange::Oanda,
                "not configured or api key not set, fx monitor disabled"
            );
            None
        }
    } else {
        tracing::info!("no fx pairs configured, fx monitor disabled");
        None
    };

    if fx_monitor.is_none() && !fx_pairs_configured {
        let has_fx_strategy = config
            .strategies
            .iter()
            .any(|s| s.enabled && s.name.starts_with("swing_llm"));
        if has_fx_strategy {
            tracing::warn!(
                remedy = "configure the oanda or gmo_fx feed, otherwise fx strategies receive no price data",
                "invalid fx price source: not configured"
            );
        }
    }

    let vegapunk_auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
    let knowledge_store: Option<Arc<dyn auto_trader_core::knowledge::KnowledgeStore>> =
        match auto_trader_vegapunk::client::VegapunkClient::connect(
            &config.vegapunk.endpoint,
            &config.vegapunk.schema,
            vegapunk_auth_token.as_deref(),
        )
        .await
        {
            Ok(client) => {
                tracing::info!(endpoint = %config.vegapunk.endpoint, "vegapunk connected");
                Some(
                    Arc::new(auto_trader::knowledge::VegapunkKnowledgeStore::new(
                        Arc::new(client),
                    )) as Arc<dyn auto_trader_core::knowledge::KnowledgeStore>,
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to connect to vegapunk");
                None
            }
        };

    let mut engine = StrategyEngine::new(signal_tx);
    auto_trader::startup::register_strategies(
        &mut engine,
        &config.strategies,
        &pool,
        &knowledge_store,
        config.gemini.as_ref(),
    )
    .await?;

    match auto_trader_db::strategies::list_strategy_names(&pool).await {
        Ok(catalog_names) => {
            let catalog_set: std::collections::HashSet<&str> =
                catalog_names.iter().map(|s| s.as_str()).collect();
            for name in engine.registered_names() {
                if !catalog_set.contains(name) {
                    tracing::warn!(
                        strategy_name = %name,
                        remedy = "add a row to the strategies table (see migrations/20260407000003_strategies.sql)",
                        "invalid strategy: missing from strategies catalog"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to check strategy catalog drift");
        }
    }

    let crypto_pairs_for_warmup: Vec<Pair> = config
        .pairs
        .crypto
        .as_ref()
        .map(|v| v.iter().map(|s| Pair::new(s)).collect())
        .unwrap_or_default();
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

    let registered_strategies: Vec<String> = engine
        .registered_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let db_accounts = match auto_trader_db::trading_accounts::list_active_or_with_open_trades(&pool)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            anyhow::bail!(
                "failed to list trading accounts at startup: {e}; cannot validate live-trading preconditions, refusing to start"
            );
        }
    };
    if db_accounts.is_empty() {
        tracing::info!("no trading accounts found in DB at startup");
    }
    for pac in &db_accounts {
        tracing::info!(
            account_id = %pac.id,
            account_name = %pac.name,
            account_type = %pac.account_type,
            exchange = %pac.exchange,
            strategy_name = %pac.strategy,
            current_balance = %pac.current_balance,
            initial_balance = %pac.initial_balance,
            leverage = %pac.leverage,
            active = pac.active,
            "found trading account at startup"
        );
        if !registered_strategies.iter().any(|s| s == &pac.strategy) {
            tracing::warn!(
                account_name = %pac.name,
                strategy_name = %pac.strategy,
                "invalid strategy: not registered"
            );
        }
    }

    {
        let has_gmo_paper_accounts = db_accounts
            .iter()
            .any(|a| a.exchange == "gmo_fx" && a.account_type == "paper");
        log_swap_freshness_at_startup_leaving_slack_to_the_daily_job(
            &config.gmo_fx.swap,
            chrono::Utc::now().date_naive(),
            has_gmo_paper_accounts,
        );
    }

    let live_cfg_dry_run = config.live.as_ref().is_some_and(|l| l.dry_run);
    let live_forces_dry_run: bool = parse_live_dry_run_override(
        std::env::var("LIVE_DRY_RUN").ok().as_deref(),
        live_cfg_dry_run,
    );
    let live_enabled = config.live.as_ref().is_some_and(|l| l.enabled);

    if let Some(ps) = config.position_sizing.as_ref() {
        tracing::info!(
            method = %ps.method,
            risk_rate = %ps.risk_rate,
            "position_sizing config is ignored, sizing is now per-strategy via signal allocation_pct"
        );
    }

    let pair_configs: Arc<HashMap<String, auto_trader_core::config::PairConfig>> =
        Arc::new(config.pair_config.clone());

    let exchange_liquidation_levels: Arc<HashMap<auto_trader_core::types::Exchange, Decimal>> =
        Arc::new(auto_trader::startup::resolve_exchange_liquidation_levels(
            &db_accounts,
            &config,
        )?);

    for mismatch in auto_trader::startup::check_fx_new_account_assumptions(&db_accounts) {
        let notify_exchange = mismatch
            .account_exchange
            .parse::<Exchange>()
            .unwrap_or(auto_trader_strategy::fx_new::FX_NEW_EXCHANGE);
        match &mismatch.kind {
            auto_trader::startup::FxNewMismatchKind::Leverage {
                account_leverage,
                expected_leverage,
            } => {
                let body = format!(
                    "account '{}' has leverage {} but fx_new strategy assumes {} — risk sizing (1% of equity at stop) is wrong for this account until leverage is corrected",
                    mismatch.account_name, account_leverage, expected_leverage
                );
                tracing::error!(body = %body, "invalid leverage: mismatch with fx_new");
                auto_trader::spawn_notify(
                    &notifier,
                    NotifyEvent::SystemAlert(SystemAlertEvent {
                        title: "fx_new leverage mismatch".to_string(),
                        account_name: mismatch.account_name.clone(),
                        exchange: notify_exchange,
                        body,
                    }),
                );
            }
            auto_trader::startup::FxNewMismatchKind::Exchange { expected_exchange } => {
                let body = format!(
                    "account '{}' has exchange '{}' but fx_new only processes {} events — the executor will not dispatch fx_new signals to this account until the exchange is corrected, so no order will be placed for it",
                    mismatch.account_name,
                    mismatch.account_exchange,
                    expected_exchange.as_str()
                );
                tracing::error!(body = %body, "invalid exchange: mismatch with fx_new");
                auto_trader::spawn_notify(
                    &notifier,
                    NotifyEvent::SystemAlert(SystemAlertEvent {
                        title: "fx_new exchange mismatch".to_string(),
                        account_name: mismatch.account_name.clone(),
                        exchange: notify_exchange,
                        body,
                    }),
                );
            }
        }
    }

    let risk = config.risk.clone().unwrap_or_default();
    let sizing_margin_buffer: Decimal = risk.sizing_margin_buffer;
    let shared_position_sizer: Arc<auto_trader_executor::position_sizer::PositionSizer> = {
        let min_order_sizes: HashMap<Pair, Decimal> = pair_configs
            .iter()
            .map(|(k, v)| (Pair::new(k), v.min_order_size))
            .collect();
        let price_units: HashMap<Pair, Decimal> = pair_configs
            .iter()
            .map(|(k, v)| (Pair::new(k), v.price_unit))
            .collect();
        Arc::new(
            auto_trader_executor::position_sizer::PositionSizer::new(
                min_order_sizes,
                sizing_margin_buffer,
            )
            .with_price_units(price_units),
        )
    };

    let price_freshness_secs: u64 = risk.price_freshness_secs;

    let daily_loss_limit_pct: Decimal = risk.daily_loss_limit_pct;
    let halt_hours: u64 = risk.halt_hours;

    let mut feeds: HashMap<Exchange, Box<dyn MarketFeed>> = HashMap::new();

    if let Some(fx_monitor) = fx_monitor {
        feeds.insert(Exchange::Oanda, Box::new(fx_monitor));
    }

    if let Some(bf_config) = &config.bitflyer {
        let crypto_pairs: Vec<Pair> = config
            .pairs
            .crypto
            .as_ref()
            .map(|v| v.iter().map(|s| Pair::new(s)).collect())
            .unwrap_or_default();
        if !crypto_pairs.is_empty() {
            for p in &crypto_pairs {
                expected_feeds.push(price_store::FeedKey::new(
                    auto_trader_core::types::Exchange::BitflyerCfd,
                    p.clone(),
                ));
            }
            let bf_feed = auto_trader_market::bitflyer::BitflyerMonitor::new(
                &bf_config.ws_url,
                crypto_pairs,
                CRYPTO_TIMEFRAME,
            )
            .with_db(pool.clone())
            .with_candle_seed(
                std::mem::take(&mut bitflyer_seed.highs),
                std::mem::take(&mut bitflyer_seed.lows),
                std::mem::take(&mut bitflyer_seed.closes),
            );
            feeds.insert(Exchange::BitflyerCfd, Box::new(bf_feed));
        }
    }

    {
        let gmo_fx_pairs: Vec<Pair> = resolve_configured_fx_pairs(&config);
        if !gmo_fx_pairs.is_empty() {
            for p in &gmo_fx_pairs {
                expected_feeds.push(price_store::FeedKey::new(
                    auto_trader_core::types::Exchange::GmoFx,
                    p.clone(),
                ));
            }
            let gmo_feed =
                auto_trader_market::gmo_fx::GmoFxFeed::new(gmo_fx_pairs.clone(), FX_TIMEFRAME)
                    .with_db(pool.clone());
            feeds.insert(Exchange::GmoFx, Box::new(gmo_feed));
            tracing::info!(pair_count = gmo_fx_pairs.len(), "gmo fx feed registered");
        } else {
            tracing::info!("no fx pairs configured, gmo fx feed disabled");
        }
    }
    let exchange_pairs: Arc<HashMap<Exchange, HashSet<String>>> = {
        let mut map: HashMap<Exchange, HashSet<String>> = HashMap::new();
        for fk in &expected_feeds {
            map.entry(fk.exchange)
                .or_default()
                .insert(fk.pair.0.clone());
        }
        Arc::new(map)
    };

    let price_store = price_store::PriceStore::new(expected_feeds);

    let mut feed_handles: Vec<tokio::task::AbortHandle> = Vec::new();
    for (exchange, feed) in feeds {
        let feed_price_store = price_store.clone();
        let feed_price_tx = price_tx.clone();
        let exchange_label = exchange;
        let inner_handle: tokio::task::JoinHandle<anyhow::Result<()>> = tokio::spawn(async move {
            tracing::info!(exchange = %exchange_label, "starting market feed");
            feed.run(feed_price_store, feed_price_tx).await
        });
        let abort_handle = auto_trader::feed_watchdog::spawn_feed_supervisor(
            exchange_label,
            notifier.clone(),
            inner_handle,
            config.feed_watchdog.notify_retry_attempts,
            config.feed_watchdog.notify_retry_initial_secs,
        );
        feed_handles.push(abort_handle);
    }

    let (macro_tx, _) =
        tokio::sync::broadcast::channel::<auto_trader_core::strategy::MacroUpdate>(16);
    let macro_rx = macro_tx.subscribe();
    let macro_analyst_handle = if config.macro_analyst.as_ref().is_some_and(|m| m.enabled) {
        let mac = config.macro_analyst.as_ref().unwrap();
        let gemini_api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => {
                tracing::warn!("invalid env var \"GEMINI_API_KEY\": not set or empty");
                None
            }
        };
        let gemini_config = config.gemini.as_ref();
        match (gemini_api_key, gemini_config) {
            (Some(_), None) | (None, _) => {
                tracing::info!(
                    "missing env var \"GEMINI_API_KEY\" or gemini config, skipping macro analyst"
                );
                None
            }
            (Some(api_key), Some(gemini_config)) => {
                let mut analyst = auto_trader_macro_analyst::analyst::MacroAnalyst::new(
                    mac.news_sources.clone(),
                    &gemini_config.api_url,
                    &api_key,
                    &gemini_config.model,
                )?
                .with_db(pool.clone());

                if let Some(store) = &knowledge_store {
                    analyst = analyst.with_knowledge(Arc::clone(store));
                }

                let news_interval = std::time::Duration::from_secs(mac.news_interval_secs);
                let macro_tx_clone = macro_tx.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = analyst.run(macro_tx_clone, news_interval).await {
                        tracing::error!(error = %e, "failed to run macro analyst");
                    }
                }))
            }
        }
    } else {
        tracing::info!("macro analyst disabled");
        None
    };

    reconcile_live_accounts_before_spawning_any_trading_task(
        &pool,
        &db_accounts,
        &exchange_apis,
        price_store.clone(),
        notifier.clone(),
    )
    .await?;

    {
        let drift_ctx = auto_trader::balance_drift::BalanceDriftContext {
            pool: pool.clone(),
            price_store: price_store.clone(),
            apis: exchange_apis.clone(),
            live_forces_dry_run,
        };
        let alerts = auto_trader::balance_drift::check_live_accounts(&drift_ctx).await;
        spawn_system_alerts(&notifier, alerts);
    }

    let pos_monitor_handle =
        spawn_fx_price_drain_since_fx_position_monitor_is_disabled(price_monitor_rx);

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
        live_forces_dry_run,
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
        live_forces_dry_run,
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
        live_forces_dry_run,
        live_enabled,
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

    let stop_fill_pool = pool.clone();
    let stop_fill_apis = exchange_apis.clone();
    let stop_fill_price_store = price_store.clone();
    let stop_fill_notifier = notifier.clone();
    let stop_fill_position_sizer = shared_position_sizer.clone();
    let stop_fill_liquidation_levels = exchange_liquidation_levels.clone();
    let stop_fill_trade_tx = trade_tx.clone();
    let stop_fill_live_forces_dry_run = live_forces_dry_run;
    let stop_fill_handle = tokio::spawn(async move {
        let close_ctx = auto_trader::closer::CloseContext {
            pool: stop_fill_pool.clone(),
            apis: stop_fill_apis.clone(),
            price_store: stop_fill_price_store.clone(),
            notifier: stop_fill_notifier.clone(),
            position_sizer: stop_fill_position_sizer.clone(),
            liquidation_levels: stop_fill_liquidation_levels.clone(),
            trade_tx: stop_fill_trade_tx.clone(),
        };
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let open_trades =
                match auto_trader_db::trades::list_open_with_account_name(&stop_fill_pool).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "failed to list open trades for stop-fill detection"
                        );
                        continue;
                    }
                };
            auto_trader::stop_fill::detect_and_close_stop_fills(
                &close_ctx,
                &open_trades,
                stop_fill_live_forces_dry_run,
            )
            .await;
        }
    });

    let drift_pool = pool.clone();
    let drift_price_store = price_store.clone();
    let drift_apis = exchange_apis.clone();
    let drift_notifier = notifier.clone();
    let drift_live_forces_dry_run = live_forces_dry_run;
    let balance_drift_handle = tokio::spawn(async move {
        let drift_ctx = auto_trader::balance_drift::BalanceDriftContext {
            pool: drift_pool.clone(),
            price_store: drift_price_store.clone(),
            apis: drift_apis.clone(),
            live_forces_dry_run: drift_live_forces_dry_run,
        };
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
        loop {
            interval.tick().await;
            let alerts = auto_trader::balance_drift::check_live_accounts(&drift_ctx).await;
            spawn_system_alerts(&drift_notifier, alerts);
        }
    });

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

    let api_state = api::AppState {
        pool: pool.clone(),
        price_store: price_store.clone(),
        exchange_liquidation_levels: exchange_liquidation_levels.clone(),
    };
    let api_handle = tokio::spawn(async move {
        let app = api::router(api_state);
        let bind_addr = "0.0.0.0:3001";
        let listener = tokio::net::TcpListener::bind(bind_addr)
            .await
            .expect("failed to bind API server");
        tracing::info!(bind_addr = %bind_addr, "api server listening");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "failed to serve api");
        }
    });

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down... draining channels");

    drop(price_tx);
    drop(trade_tx);
    for h in feed_handles {
        h.abort();
    }
    overnight_handle.abort();
    sfd_handle.abort();
    stop_fill_handle.abort();
    balance_drift_handle.abort();
    if let Some(h) = feed_watchdog_handle {
        h.abort();
    }
    daily_handle.abort();
    if let Some(h) = macro_analyst_handle {
        h.abort();
    }
    api_handle.abort();

    let drain_timeout = tokio::time::Duration::from_secs(5);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = engine_handle.await;
        let _ = pos_monitor_handle.await;
        let _ = crypto_monitor_handle.await;
        let _ = exit_executor_handle.await;
        let _ = executor_handle.await;
        let _ = recorder_handle.await;
    })
    .await;

    tracing::info!("shutdown complete");
    Ok(())
}

#[cfg(test)]
#[path = "main/tests.rs"]
mod tests;
