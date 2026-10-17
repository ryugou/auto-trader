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
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeAction, TradeEvent};
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

async fn update_price_store_from_candle_path_the_only_source_for_exchanges_without_raw_tick_feed(
    price_store: &auto_trader::price_store::PriceStore,
    key: price_store::FeedKey,
    tick: price_store::LatestTick,
) {
    price_store.update(key, tick).await;
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

    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, mut trade_rx) = mpsc::channel::<TradeEvent>(256);

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

    let mut bitflyer_highs_seed: std::collections::HashMap<String, Vec<Decimal>> =
        std::collections::HashMap::new();
    let mut bitflyer_lows_seed: std::collections::HashMap<String, Vec<Decimal>> =
        std::collections::HashMap::new();
    let mut bitflyer_closes_seed: std::collections::HashMap<String, Vec<Decimal>> =
        std::collections::HashMap::new();

    {
        use auto_trader_core::event::PriceEvent;
        use auto_trader_core::types::Exchange as ExchangeTy;
        use std::collections::HashMap as StdHashMap;

        async fn load_warmup_history(
            pool: &sqlx::PgPool,
            exchange: &str,
            pair: &str,
            timeframe: &str,
            limit: i64,
        ) -> Vec<auto_trader_core::types::Candle> {
            match auto_trader_db::candles::get_candles(pool, exchange, pair, timeframe, limit).await
            {
                Ok(most_recent_first) => most_recent_first.into_iter().rev().collect(),
                Err(e) => {
                    tracing::warn!(
                        exchange = %exchange,
                        pair = %pair,
                        timeframe = %timeframe,
                        error = %e,
                        "failed to load warmup history"
                    );
                    Vec::new()
                }
            }
        }

        for pair in &crypto_pairs_for_warmup {
            let candles = load_warmup_history(
                &pool,
                ExchangeTy::BitflyerCfd.as_str(),
                &pair.0,
                CRYPTO_TIMEFRAME,
                WARMUP_LIMIT,
            )
            .await;
            if candles.is_empty() {
                continue;
            }
            let highs: Vec<Decimal> = candles.iter().map(|c| c.high).collect();
            let lows: Vec<Decimal> = candles.iter().map(|c| c.low).collect();
            let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();
            bitflyer_highs_seed.insert(pair.0.clone(), highs);
            bitflyer_lows_seed.insert(pair.0.clone(), lows);
            bitflyer_closes_seed.insert(pair.0.clone(), closes);

            let n = candles.len();
            let events: Vec<PriceEvent> = candles
                .into_iter()
                .map(|c| PriceEvent {
                    pair: c.pair.clone(),
                    exchange: ExchangeTy::BitflyerCfd,
                    timestamp: c.timestamp,
                    candle: c,
                    indicators: StdHashMap::new(),
                })
                .collect();
            engine.warmup(&events).await;
            tracing::info!(
                exchange = %ExchangeTy::BitflyerCfd,
                timeframe = CRYPTO_TIMEFRAME,
                pair = %pair.0,
                candle_count = n,
                "fed warmup candles"
            );
        }

        for pair in &crypto_pairs_for_warmup {
            let h1_candles = load_warmup_history(
                &pool,
                ExchangeTy::BitflyerCfd.as_str(),
                &pair.0,
                H1_TIMEFRAME,
                WARMUP_LIMIT,
            )
            .await;
            if h1_candles.is_empty() {
                continue;
            }
            let n = h1_candles.len();
            let h1_events: Vec<PriceEvent> = h1_candles
                .into_iter()
                .map(|c| PriceEvent {
                    pair: c.pair.clone(),
                    exchange: ExchangeTy::BitflyerCfd,
                    timestamp: c.timestamp,
                    candle: c,
                    indicators: StdHashMap::new(),
                })
                .collect();
            engine.warmup(&h1_events).await;
            tracing::info!(
                exchange = %ExchangeTy::BitflyerCfd,
                timeframe = H1_TIMEFRAME,
                pair = %pair.0,
                candle_count = n,
                "fed warmup candles"
            );
        }

        let fx_pairs_for_warmup: Vec<Pair> = resolve_configured_fx_pairs(&config);
        for pair in &fx_pairs_for_warmup {
            let candles = load_warmup_history(
                &pool,
                ExchangeTy::Oanda.as_str(),
                &pair.0,
                FX_TIMEFRAME,
                WARMUP_LIMIT,
            )
            .await;
            if candles.is_empty() {
                continue;
            }
            let n = candles.len();
            let events: Vec<PriceEvent> = candles
                .into_iter()
                .map(|c| PriceEvent {
                    pair: c.pair.clone(),
                    exchange: ExchangeTy::Oanda,
                    timestamp: c.timestamp,
                    candle: c,
                    indicators: StdHashMap::new(),
                })
                .collect();
            engine.warmup(&events).await;
            tracing::info!(
                exchange = %ExchangeTy::Oanda,
                timeframe = FX_TIMEFRAME,
                pair = %pair.0,
                candle_count = n,
                "fed warmup candles"
            );
        }

        for pair in &fx_pairs_for_warmup {
            let candles = load_warmup_history(
                &pool,
                ExchangeTy::GmoFx.as_str(),
                &pair.0,
                FX_TIMEFRAME,
                WARMUP_LIMIT,
            )
            .await;
            if candles.is_empty() {
                continue;
            }
            let n = candles.len();
            let events: Vec<PriceEvent> = candles
                .into_iter()
                .map(|c| PriceEvent {
                    pair: c.pair.clone(),
                    exchange: ExchangeTy::GmoFx,
                    timestamp: c.timestamp,
                    candle: c,
                    indicators: StdHashMap::new(),
                })
                .collect();
            engine.warmup(&events).await;
            tracing::info!(
                exchange = %ExchangeTy::GmoFx,
                timeframe = FX_TIMEFRAME,
                pair = %pair.0,
                candle_count = n,
                "fed warmup candles"
            );
        }

        for pair in &fx_pairs_for_warmup {
            let h1_candles = load_warmup_history(
                &pool,
                ExchangeTy::GmoFx.as_str(),
                &pair.0,
                H1_TIMEFRAME,
                WARMUP_LIMIT,
            )
            .await;
            if h1_candles.is_empty() {
                continue;
            }
            let n = h1_candles.len();
            let h1_events: Vec<PriceEvent> = h1_candles
                .into_iter()
                .map(|c| PriceEvent {
                    pair: c.pair.clone(),
                    exchange: ExchangeTy::GmoFx,
                    timestamp: c.timestamp,
                    candle: c,
                    indicators: StdHashMap::new(),
                })
                .collect();
            engine.warmup(&h1_events).await;
            tracing::info!(
                exchange = %ExchangeTy::GmoFx,
                timeframe = H1_TIMEFRAME,
                pair = %pair.0,
                candle_count = n,
                "fed warmup candles"
            );
        }
    }

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
                std::mem::take(&mut bitflyer_highs_seed),
                std::mem::take(&mut bitflyer_lows_seed),
                std::mem::take(&mut bitflyer_closes_seed),
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
    let engine_pool = pool.clone();
    let price_store_for_engine = price_store.clone();

    let engine_handle = tokio::spawn(async move {
        let mut macro_rx = macro_rx;
        loop {
            tokio::select! {
                price = price_rx.recv() => {
                    match price {
                        Some(event) => {
                            update_price_store_from_candle_path_the_only_source_for_exchanges_without_raw_tick_feed(
                                &price_store_for_engine,
                                price_store::FeedKey::new(event.exchange, event.pair.clone()),
                                price_store::LatestTick {
                                    price: event.candle.close,
                                    best_bid: event.candle.best_bid,
                                    best_ask: event.candle.best_ask,
                                    ts: event.timestamp,
                                },
                            )
                            .await;
                            if event.exchange == auto_trader_core::types::Exchange::Oanda
                                && price_monitor_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("fx position monitor channel closed");
                            }
                            if (event.exchange == auto_trader_core::types::Exchange::BitflyerCfd
                                || event.exchange == auto_trader_core::types::Exchange::GmoFx)
                                && crypto_price_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("position monitor channel closed");
                            }

                            let positions_by_strategy = match auto_trader_db::trades::list_open_with_account_name_for_pair(
                                &engine_pool,
                                event.exchange.as_str(),
                                &event.pair.0,
                            ).await {
                                Ok(rows) => {
                                    let mut by_strategy: std::collections::HashMap<String, Vec<auto_trader_core::types::Position>> =
                                        std::collections::HashMap::new();
                                    for r in rows {
                                        by_strategy
                                            .entry(r.trade.strategy_name.clone())
                                            .or_default()
                                            .push(auto_trader_core::types::Position { trade: r.trade });
                                    }
                                    by_strategy
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to list open trades for on_open_positions"
                                    );
                                    std::collections::HashMap::new()
                                }
                            };

                            let exits = engine
                                .on_price_with_positions(&event, &positions_by_strategy)
                                .await;
                            for exit in exits {
                                if exit_tx.send(exit).await.is_err() {
                                    tracing::warn!("failed to send strategy exit: channel closed");
                                }
                            }
                        }
                        None => break,
                    }
                }
                macro_update = macro_rx.recv() => {
                    match macro_update {
                        Ok(update) => engine.on_macro_update(&update),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(skipped_updates = n, "failed to receive macro broadcast: lagged");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::info!("macro broadcast channel closed");
                        }
                    }
                }
            }
        }
    });

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

    let recorder_pool = pool.clone();
    let knowledge_store_recorder = knowledge_store.clone();
    let recorder_handle = tokio::spawn(async move {
        while let Some(trade_event) = trade_rx.recv().await {
            match trade_event.action {
                TradeAction::Opened => {}
                TradeAction::Closed { .. } => {
                    let t = &trade_event.trade;
                    if let (
                        Some(_exit_price),
                        Some(exit_at),
                        Some(pnl_amount),
                        Some(_exit_reason),
                    ) = (t.exit_price, t.exit_at, t.pnl_amount, t.exit_reason)
                    {
                        let date = exit_at.date_naive();
                        let win = if pnl_amount > Decimal::ZERO { 1 } else { 0 };
                        let account_id = t.account_id;
                        let account_type = trade_event.account_type.as_deref().unwrap_or("paper");
                        if let Err(e) = auto_trader_db::summary::upsert_daily_summary(
                            &recorder_pool,
                            date,
                            &t.strategy_name,
                            &t.pair.0,
                            account_type,
                            t.exchange.as_str(),
                            Some(account_id),
                            1,
                            win,
                            pnl_amount,
                        )
                        .await
                        {
                            tracing::error!(error = %e, "failed to upsert daily summary");
                        }
                        if let Some(store) = knowledge_store_recorder.clone() {
                            let t = t.clone();
                            let close_pool = recorder_pool.clone();
                            tokio::spawn(async move {
                                let entry_ind: Option<serde_json::Value> = sqlx::query_scalar(
                                    "SELECT entry_indicators FROM trades WHERE id = $1",
                                )
                                .bind(t.id)
                                .fetch_optional(&close_pool)
                                .await
                                .unwrap_or(None)
                                .flatten();

                                let (bal, init): (
                                    Option<rust_decimal::Decimal>,
                                    Option<rust_decimal::Decimal>,
                                ) = {
                                    let aid = t.account_id;
                                    sqlx::query_as::<_, (rust_decimal::Decimal, rust_decimal::Decimal)>(
                                        "SELECT current_balance, initial_balance FROM trading_accounts WHERE id = $1",
                                    )
                                    .bind(aid)
                                    .fetch_optional(&close_pool)
                                    .await
                                    .unwrap_or(None)
                                    .map(|(b, i)| (Some(b), Some(i)))
                                    .unwrap_or((None, None))
                                };

                                let ctx = auto_trader_core::knowledge::TradeCloseContext {
                                    entry_indicators: entry_ind.as_ref(),
                                    account_balance: bal,
                                    account_initial: init,
                                };
                                if let Err(e) = store.record_trade_close(&t, &ctx).await {
                                    tracing::warn!(
                                        error = %e,
                                        "failed to record trade close in knowledge store"
                                    );
                                }

                                let search_id: Option<uuid::Uuid> = sqlx::query_scalar(
                                    "SELECT vegapunk_search_id FROM trades WHERE id = $1",
                                )
                                .bind(t.id)
                                .fetch_optional(&close_pool)
                                .await
                                .unwrap_or(None)
                                .flatten();

                                if let Some(sid) = search_id {
                                    let rating =
                                        auto_trader::enriched_ingest::compute_feedback_rating(&t);
                                    let net_pnl = t.pnl_amount.unwrap_or_default() - t.fees;
                                    let regime = entry_ind
                                        .as_ref()
                                        .and_then(|i| i.get("regime"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let comment = format!("PnL: {net_pnl}, regime: {regime}");
                                    let sid_str = sid.to_string();
                                    if let Err(e) =
                                        store.submit_feedback(&sid_str, rating, &comment).await
                                    {
                                        tracing::warn!(
                                            error = %e,
                                            "failed to submit feedback to knowledge store"
                                        );
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }
    });

    let knowledge_store_daily = knowledge_store.clone();
    let knowledge_store_weekly = knowledge_store.clone();
    let gemini_api_url = config
        .gemini
        .as_ref()
        .map(|g| g.api_url.clone())
        .unwrap_or_default();
    let gemini_api_key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    let gemini_model = config
        .gemini
        .as_ref()
        .map(|g| g.model.clone())
        .unwrap_or_default();
    let daily_pool = pool.clone();
    let daily_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let today = chrono::Utc::now().date_naive();

        let backfill_days: i64 = config.monitor.backfill_days.unwrap_or(7) as i64;
        for i in (1..=backfill_days).rev() {
            let d = today - chrono::Duration::days(i);
            tracing::info!(date = %d, "running daily batch backfill at startup");
            if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(&daily_pool, d).await
            {
                tracing::error!(date = %d, error = %e, "failed to run daily batch backfill");
            }
        }

        match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
            Ok(n) if n > 0 => tracing::info!(purged_count = n, "purged old read notifications"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "failed to purge old read notifications"),
        }

        let mut last_date = today;
        loop {
            interval.tick().await;
            let now_date = chrono::Utc::now().date_naive();
            if now_date != last_date {
                tracing::info!(date = %last_date, "running daily batch");
                if let Err(e) =
                    auto_trader_db::summary::update_daily_max_drawdown(&daily_pool, last_date).await
                {
                    tracing::error!(error = %e, "failed to run daily batch");
                }
                match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
                    Ok(n) if n > 0 => {
                        tracing::info!(purged_count = n, "purged old read notifications")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "failed to purge old read notifications"),
                }

                if let Some(store) = knowledge_store_daily.clone() {
                    tokio::spawn(async move {
                        if let Err(e) = store.run_merge().await {
                            tracing::warn!(error = %e, "failed to run daily knowledge store merge");
                        } else {
                            tracing::info!("daily knowledge store merge completed");
                        }
                    });
                }

                use chrono::Datelike as _;
                let jst_weekday = (chrono::Utc::now() + chrono::Duration::hours(9)).weekday();
                if jst_weekday == chrono::Weekday::Sun
                    && let Err(e) = crate::weekly_batch::run(
                        &daily_pool,
                        knowledge_store_weekly.as_ref(),
                        &gemini_api_url,
                        &gemini_api_key,
                        &gemini_model,
                    )
                    .await
                {
                    tracing::error!(error = %e, "failed to run weekly evolution batch");
                }

                last_date = now_date;
            }
        }
    });

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

    let overnight_pool = pool.clone();
    let swap_config = config.gmo_fx.swap.clone();
    let overnight_notifier = notifier.clone();
    let overnight_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let bitflyer_fee_rate = rust_decimal_macros::dec!(0.0004);
        let mut last_date = chrono::Utc::now().date_naive();
        loop {
            interval.tick().await;
            let today = chrono::Utc::now().date_naive();
            if today != last_date {
                let accounts =
                    match auto_trader_db::trading_accounts::list_active_or_with_open_trades(
                        &overnight_pool,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "failed to list trading accounts for overnight fee, retrying next tick"
                            );
                            continue;
                        }
                    };
                let has_gmo_paper = accounts
                    .iter()
                    .any(|a| a.exchange == "gmo_fx" && a.account_type == "paper");
                if let Some(body) = auto_trader::swap_freshness::swap_freshness_alert(
                    &swap_config,
                    today,
                    has_gmo_paper,
                ) {
                    let ev = auto_trader_notify::NotifyEvent::SystemAlert(
                        auto_trader_notify::SystemAlertEvent {
                            title: "swap rates freshness".to_string(),
                            account_name: "(config)".to_string(),
                            exchange: auto_trader_core::types::Exchange::GmoFx,
                            body,
                        },
                    );
                    let notifier = overnight_notifier.clone();
                    tokio::spawn(async move {
                        if let Err(e) = notifier.send(ev).await {
                            tracing::warn!(error = %e, "failed to send swap freshness alert");
                        }
                    });
                }

                let event_at = today
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc();
                for pac in accounts {
                    if pac.account_type != "paper" {
                        continue;
                    }
                    let exchange = match exchange_from_str(&pac.exchange) {
                        Some(e) => e,
                        None => {
                            tracing::warn!(
                                account_name = %pac.name,
                                account_id = %pac.id,
                                exchange = %pac.exchange,
                                "unsupported exchange"
                            );
                            continue;
                        }
                    };
                    if !overnight_fee_applies_to_exchange(exchange) {
                        continue;
                    }
                    let open_trades = match auto_trader_db::trades::get_open_trades_by_account(
                        &overnight_pool,
                        pac.id,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(
                                account_name = %pac.name,
                                error = %e,
                                "failed to list open trades for overnight fee/swap"
                            );
                            continue;
                        }
                    };
                    let mut applied_count: usize = 0;
                    for trade in &open_trades {
                        let fee = match exchange {
                            Exchange::BitflyerCfd => (trade.entry_price
                                * trade.quantity
                                * bitflyer_fee_rate)
                                .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero),
                            Exchange::GmoFx => {
                                let Some(rate) = swap_config.rates.get(trade.pair.0.as_str())
                                else {
                                    tracing::debug!(
                                        pair = %trade.pair,
                                        trade_id = %trade.id,
                                        "no swap rate configured for pair, skipping"
                                    );
                                    continue;
                                };
                                auto_trader_core::swap::compute_daily_swap(
                                    *rate,
                                    trade.direction,
                                    trade.quantity,
                                )
                            }
                            Exchange::Oanda => unreachable!("filtered above"),
                        };
                        if fee.is_zero() {
                            continue;
                        }
                        let result = async {
                            let mut tx = overnight_pool.begin().await?;
                            let applied = match exchange {
                                Exchange::BitflyerCfd => {
                                    auto_trader_db::trades::apply_overnight_fee(
                                        &mut tx, pac.id, trade.id, fee,
                                    )
                                    .await?
                                }
                                Exchange::GmoFx => {
                                    auto_trader_db::trades::apply_swap_fee(
                                        &mut tx, pac.id, trade.id, fee, event_at,
                                    )
                                    .await?
                                }
                                Exchange::Oanda => unreachable!("filtered above"),
                            };
                            tx.commit().await?;
                            anyhow::Ok(applied)
                        }
                        .await;
                        match result {
                            Ok(Some(_new_balance)) => {
                                applied_count += 1;
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    trade_id = %trade.id,
                                    "overnight fee/swap apply skipped, trade closed mid-tick"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    trade_id = %trade.id,
                                    error = %e,
                                    "failed to apply overnight fee or swap"
                                );
                            }
                        }
                    }
                    if applied_count > 0 {
                        tracing::info!(
                            account_name = %pac.name,
                            exchange = %exchange,
                            applied_count,
                            "applied overnight fee/swap to trades"
                        );
                    }
                }
                last_date = today;
            }
        }
    });

    let sfd_pool = pool.clone();
    let sfd_price_store = price_store.clone();
    let sfd_handle = tokio::spawn(async move {
        use auto_trader_core::sfd;
        use auto_trader_market::price_store::FeedKey;
        use chrono::Timelike as _;
        const SFD_MAX_CATCH_UP_HOURS: i64 = 24;
        const SFD_STALE_THRESHOLD_SECS: u64 = 300;

        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let mut last_hour: Option<chrono::DateTime<chrono::Utc>> = None;
        loop {
            interval.tick().await;
            let now = chrono::Utc::now();
            let current_hour = now
                .date_naive()
                .and_hms_opt(now.hour(), 0, 0)
                .expect("hms_opt(now.hour(), 0, 0) is always valid")
                .and_utc();
            let Some(mut lh) = last_hour else {
                last_hour = Some(current_hour);
                continue;
            };
            if lh + chrono::Duration::hours(1) > current_hour {
                continue;
            }

            let mut pending = (current_hour - lh).num_hours();
            if pending > SFD_MAX_CATCH_UP_HOURS {
                let dropped = pending - SFD_MAX_CATCH_UP_HOURS;
                tracing::warn!(
                    missed_hours = pending,
                    dropped_hours = dropped,
                    max_catch_up = SFD_MAX_CATCH_UP_HOURS,
                    remedy = "audit the balance, a long ws outage likely dropped older accruals",
                    "invalid sfd catch-up: too many missed hours"
                );
                lh = current_hour - chrono::Duration::hours(SFD_MAX_CATCH_UP_HOURS);
                pending = SFD_MAX_CATCH_UP_HOURS;
            }

            let accounts =
                match auto_trader_db::trading_accounts::list_active_or_with_open_trades(&sfd_pool)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            "failed to list trading accounts for sfd accrual, retrying next tick"
                        );
                        continue;
                    }
                };
            let has_paper_bitflyer = accounts.iter().any(|pac| {
                pac.account_type == "paper"
                    && exchange_from_str(&pac.exchange) == Some(Exchange::BitflyerCfd)
            });
            if !has_paper_bitflyer {
                last_hour = Some(current_hour);
                continue;
            }

            let open_target_count = match count_open_bitflyer_paper_fx_btc_jpy_trades(&sfd_pool)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(
                        error = %e,
                        "failed to count open fx_btc_jpy trades for sfd accrual, retrying next tick"
                    );
                    continue;
                }
            };
            if open_target_count == 0 {
                last_hour = Some(current_hour);
                continue;
            }

            let fx_key = FeedKey::new(
                Exchange::BitflyerCfd,
                auto_trader_core::types::Pair::new("FX_BTC_JPY"),
            );
            let spot_key = FeedKey::new(
                Exchange::BitflyerCfd,
                auto_trader_core::types::Pair::new("BTC_JPY"),
            );
            let fx_age = sfd_price_store.last_tick_age_for(&fx_key).await;
            let spot_age = sfd_price_store.last_tick_age_for(&spot_key).await;
            let fresh = matches!(fx_age, Some(a) if a <= SFD_STALE_THRESHOLD_SECS)
                && matches!(spot_age, Some(a) if a <= SFD_STALE_THRESHOLD_SECS);
            if !fresh {
                tracing::warn!(
                    fx_age_secs = ?fx_age,
                    spot_age_secs = ?spot_age,
                    stale_threshold_secs = SFD_STALE_THRESHOLD_SECS,
                    "invalid sfd hourly tick: stale"
                );
                continue;
            }

            let fx_ba = sfd_price_store.latest_bid_ask(&fx_key).await;
            let spot_ba = sfd_price_store.latest_bid_ask(&spot_key).await;
            let (fx_mid_fixed_for_every_catchup_hour, spot_mid_fixed_for_every_catchup_hour) =
                match (fx_ba, spot_ba) {
                    (Some((b1, a1)), Some((b2, a2))) => {
                        ((b1 + a1) / Decimal::from(2), (b2 + a2) / Decimal::from(2))
                    }
                    _ => {
                        tracing::warn!("invalid sfd hourly tick: missing bid/ask");
                        continue;
                    }
                };

            for _ in 0..pending {
                for pac in &accounts {
                    if pac.account_type != "paper" {
                        continue;
                    }
                    let exchange = match exchange_from_str(&pac.exchange) {
                        Some(e) => e,
                        None => {
                            tracing::warn!(
                                account_name = %pac.name,
                                account_id = %pac.id,
                                exchange = %pac.exchange,
                                "unsupported exchange"
                            );
                            continue;
                        }
                    };
                    if exchange != Exchange::BitflyerCfd {
                        continue;
                    }
                    let open_trades =
                        match auto_trader_db::trades::get_open_trades_by_account(&sfd_pool, pac.id)
                            .await
                        {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::error!(
                                    account_name = %pac.name,
                                    error = %e,
                                    "failed to list open trades for sfd hourly"
                                );
                                continue;
                            }
                        };
                    for trade in &open_trades {
                        if !trade_is_subject_to_bitflyer_sfd(&trade.pair.0) {
                            continue;
                        }
                        let notional = trade.entry_price * trade.quantity;
                        let fee = sfd::compute_hourly_sfd(sfd::SfdContext {
                            fx_price: fx_mid_fixed_for_every_catchup_hour,
                            spot_price: spot_mid_fixed_for_every_catchup_hour,
                            position_notional: notional,
                            direction: trade.direction,
                        });
                        if fee.is_zero() {
                            continue;
                        }
                        let event_at = lh + chrono::Duration::hours(1);
                        if trade_was_not_yet_open_at_event_time(trade.entry_at, event_at) {
                            continue;
                        }
                        let result = async {
                            let mut tx = sfd_pool.begin().await?;
                            let applied = auto_trader_db::trades::apply_sfd_fee(
                                &mut tx, pac.id, trade.id, fee, event_at,
                            )
                            .await?;
                            tx.commit().await?;
                            anyhow::Ok(applied)
                        }
                        .await;
                        match result {
                            Ok(Some(_)) => {
                                tracing::info!(
                                    trade_id = %trade.id,
                                    fee = %fee,
                                    notional = %notional,
                                    "sfd applied"
                                );
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    trade_id = %trade.id,
                                    "sfd skipped, trade closed mid-tick"
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    trade_id = %trade.id,
                                    error = %e,
                                    "failed to apply sfd fee"
                                );
                            }
                        }
                    }
                }
                lh += chrono::Duration::hours(1);
            }
            last_hour = Some(lh);
        }
    });

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
