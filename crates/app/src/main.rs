mod enriched_ingest;
mod regime;
mod startup_reconcile;
mod weekly_batch;
mod wilson;

use auto_trader::api;
use auto_trader::price_store;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeAction, TradeEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_db::pool::create_pool;
use auto_trader_executor::trader::Trader as UnifiedTrader;
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::market_feed::MarketFeed;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_market::oanda_private::OandaPrivateApi;
use auto_trader_notify::Notifier;
use auto_trader_strategy::engine::StrategyEngine;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

use auto_trader_executor::risk_gate::{GateDecision, eval_price_freshness};

fn exchange_from_str(s: &str) -> Option<Exchange> {
    s.parse().ok()
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
    tracing::info!("config loaded from {config_path}");
    if let Some(bf) = config.bitflyer.as_mut() {
        bf.api_key = std::env::var("BITFLYER_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        bf.api_secret = std::env::var("BITFLYER_API_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
    }

    // Database
    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    // BitflyerPrivateApi — used by UnifiedTrader for live order placement.
    // Always constructed so Trader can be initialized uniformly;
    // dry_run=true accounts never call into the API.
    let bitflyer_api: Arc<BitflyerPrivateApi> =
        Arc::new(if let Some(bf) = config.bitflyer.as_ref() {
            let api_key = bf.api_key.clone().unwrap_or_default();
            let api_secret = bf.api_secret.clone().unwrap_or_default();
            BitflyerPrivateApi::new(api_key, api_secret)
        } else {
            BitflyerPrivateApi::new(String::new(), String::new())
        });

    // Exchange API registry — maps Exchange variant to its private-API client.
    // Adding a new exchange = impl ExchangeApi for NewClient + insert here.
    let mut exchange_apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
    exchange_apis.insert(Exchange::BitflyerCfd, bitflyer_api.clone());

    // Resolve OANDA account_id from env (trimmed, non-empty) → config fallback.
    // Used by both the ExchangeApi registry and the FX market monitor so
    // they share identical resolution semantics.
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

    // Resolve OANDA api_key from env (trimmed, non-empty).
    // Used by both the ExchangeApi registry and the FX market monitor so
    // they share identical resolution semantics.
    fn resolve_oanda_api_key() -> Option<String> {
        std::env::var("OANDA_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    // OANDA ExchangeApi — registered when OANDA_API_KEY is set and either
    // OANDA_ACCOUNT_ID env var or [oanda].account_id in config is provided.
    // If absent, Oanda trading_accounts are simply skipped at dispatch
    // (same behavior as any exchange whose client isn't in the registry).
    let oanda_account_id = resolve_oanda_account_id(&config);
    let oanda_api_key = resolve_oanda_api_key();
    if let (Some(account_id), Some(api_key), Some(oanda_config)) =
        (oanda_account_id, oanda_api_key, config.oanda.as_ref())
    {
        let oanda_api: Arc<dyn ExchangeApi> = Arc::new(OandaPrivateApi::new(
            oanda_config.api_url.clone(),
            account_id,
            api_key,
        ));
        exchange_apis.insert(Exchange::Oanda, oanda_api);
        tracing::info!("OANDA ExchangeApi registered");
    } else {
        tracing::info!(
            "OANDA ExchangeApi not registered \
             (requires OANDA_API_KEY env + account_id (OANDA_ACCOUNT_ID env or [oanda].account_id config) + [oanda].api_url in config)"
        );
    }

    let exchange_apis = Arc::new(exchange_apis);

    // Notifier — Slack Webhook for operator alerts.
    let slack_webhook_url = std::env::var("SLACK_WEBHOOK_URL").ok();
    let notifier: Arc<Notifier> = Arc::new(Notifier::new(slack_webhook_url));

    // Build the expected feed list — (exchange, pair) tuples this
    // process is configured to monitor. The health endpoint uses
    // this list to distinguish "intentionally disabled" from
    // "expected but missing", so OANDA without an API key never
    // shows up as a stale alarm. Populated inside each monitor's
    // setup block so it naturally tracks whatever feeds this
    // process actually launches.
    let mut expected_feeds: Vec<price_store::FeedKey> = Vec::new();

    // Channels — price は position_monitor にも配信するため 2 本
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, mut signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, mut trade_rx) = mpsc::channel::<TradeEvent>(256);

    // Single source of truth for the timeframe used by each market monitor
    // AND its corresponding strategy warmup loader. Sharing the same constant
    // here prevents drift between live polling/streaming and warmup history.
    const CRYPTO_TIMEFRAME: &str = "M5";
    const H1_TIMEFRAME: &str = "H1";
    const FX_TIMEFRAME: &str = "M5";
    const WARMUP_LIMIT: i64 = 200;

    // FX market monitor (optional — skipped if no FX pairs or OANDA_API_KEY not set)
    let fx_pairs: Vec<Pair> = if !config.pairs.active.is_empty() {
        config.pairs.active.iter().map(|s| Pair::new(s)).collect()
    } else {
        config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
    };
    // Capture before fx_pairs is potentially moved into the OANDA monitor.
    let fx_pairs_configured = !fx_pairs.is_empty();
    let fx_monitor: Option<MarketMonitor> = if !fx_pairs.is_empty() {
        if let (Some(api_key), Some(oanda_config)) =
            (resolve_oanda_api_key(), config.oanda.as_ref())
        {
            match resolve_oanda_account_id(&config) {
                Some(account_id) => {
                    // Register this monitor's pairs as expected feeds now that
                    // we've confirmed FX monitor will actually start.
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
                        "FX monitor: no OANDA account_id available (should have been caught earlier)"
                    );
                    None
                }
            }
        } else {
            tracing::info!("OANDA not configured or API key not set, FX monitor disabled");
            None
        }
    } else {
        tracing::info!("no FX pairs configured, FX monitor disabled");
        None
    };

    // Warn if FX strategies are enabled but NO FX price source will run.
    // Price data can come from either the OANDA monitor or the GMO FX feed
    // (registered whenever fx_pairs is non-empty), so only warn when both
    // are absent.
    if fx_monitor.is_none() && !fx_pairs_configured {
        let has_fx_strategy = config
            .strategies
            .iter()
            .any(|s| s.enabled && s.name.starts_with("swing_llm"));
        if has_fx_strategy {
            tracing::warn!(
                "FX strategies are enabled but no FX price source is running \
                 (neither OANDA nor GMO FX feed configured). \
                 These strategies will not receive price data."
            );
        }
    }

    // Vegapunk: single gRPC channel with optional Bearer token auth
    let vegapunk_auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
    let vegapunk_base: Option<auto_trader_vegapunk::client::VegapunkClient> =
        match auto_trader_vegapunk::client::VegapunkClient::connect(
            &config.vegapunk.endpoint,
            &config.vegapunk.schema,
            vegapunk_auth_token.as_deref(),
        )
        .await
        {
            Ok(client) => {
                tracing::info!("vegapunk connected: {}", config.vegapunk.endpoint);
                Some(client)
            }
            Err(e) => {
                tracing::warn!("vegapunk unavailable (continuing without): {e}");
                None
            }
        };

    // Strategy engine
    let mut engine = StrategyEngine::new(signal_tx);
    auto_trader::startup::register_strategies(
        &mut engine,
        &config.strategies,
        &pool,
        &vegapunk_base,
        &config.vegapunk.schema,
        config.gemini.as_ref(),
    )
    .await;

    // Drift check: every strategy actually registered with the engine MUST
    // exist in the catalog table so the UI dropdown / API validation are
    // consistent with what the runtime can serve. Strategies present in the
    // catalog but not registered are fine (the catalog can describe more
    // than what's currently enabled in config).
    match auto_trader_db::strategies::list_strategy_names(&pool).await {
        Ok(catalog_names) => {
            let catalog_set: std::collections::HashSet<&str> =
                catalog_names.iter().map(|s| s.as_str()).collect();
            for name in engine.registered_names() {
                if !catalog_set.contains(name) {
                    tracing::warn!(
                        "strategy '{name}' is registered but missing from the strategies catalog table — UI dropdown and API validation will reject paper accounts that reference it. Add a row to the strategies table (see migrations/20260407000003_strategies.sql)."
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!("strategy catalog drift check failed: {e}");
        }
    }

    // Warm up strategies and the bitflyer indicator cache from DB so they
    // don't sit cold for ma_long_period × timeframe minutes after every
    // restart. We load each (exchange, pair, timeframe) once and feed both
    // consumers from the same `Vec<Candle>` to keep indicator state and live
    // stream consistent.
    //
    // CRYPTO_TIMEFRAME / FX_TIMEFRAME / WARMUP_LIMIT are declared at the top
    // of main() so the same value is used both here and when constructing
    // the live monitors — preventing warmup/live drift.

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
                // get_candles returns DESC; reverse to ASC (oldest first) so
                // both indicator cache and strategy history are seeded in
                // chronological order.
                Ok(candles) => candles.into_iter().rev().collect(),
                Err(e) => {
                    tracing::warn!("warmup load failed for {exchange} {pair} {timeframe}: {e}");
                    Vec::new()
                }
            }
        }

        // Crypto: bitflyer_cfd
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
            // Feed bitflyer indicator cache: highs, lows, and closes.
            let highs: Vec<Decimal> = candles.iter().map(|c| c.high).collect();
            let lows: Vec<Decimal> = candles.iter().map(|c| c.low).collect();
            let closes: Vec<Decimal> = candles.iter().map(|c| c.close).collect();
            bitflyer_highs_seed.insert(pair.0.clone(), highs);
            bitflyer_lows_seed.insert(pair.0.clone(), lows);
            bitflyer_closes_seed.insert(pair.0.clone(), closes);

            // Feed strategy engine: PriceEvent so indicator history populates.
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
                "strategy warmup: fed {n} bitflyer_cfd {CRYPTO_TIMEFRAME} candles for {}",
                pair.0
            );
        }

        // Crypto: bitflyer_cfd — 1H candles for Donchian / Squeeze strategies.
        // These strategies filter by timeframe so M5 warmup events above are
        // already silently ignored by them; the H1 events below are silently
        // ignored by bb_mean_revert. Loading both ensures every strategy starts
        // with a warm indicator cache regardless of its timeframe preference.
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
                "strategy warmup: fed {n} bitflyer_cfd {H1_TIMEFRAME} candles for {}",
                pair.0
            );
        }

        // FX: Oanda. fx_pairs is moved into MarketMonitor::new earlier, so
        // re-derive the list from config the same way (`active` legacy field
        // takes precedence over `fx`).
        let fx_pairs_for_warmup: Vec<Pair> = if !config.pairs.active.is_empty() {
            config.pairs.active.iter().map(|s| Pair::new(s)).collect()
        } else {
            config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
        };
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
                "strategy warmup: fed {n} oanda {FX_TIMEFRAME} candles for {}",
                pair.0
            );
        }

        // FX: GmoFx. Load M5 candle history so strategies start with a warm
        // indicator cache after restart (mirrors the OANDA warmup above).
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
                "strategy warmup: fed {n} gmo_fx {FX_TIMEFRAME} candles for {}",
                pair.0
            );
        }

        // FX: GmoFx — H1 candles for Donchian/Squeeze strategies.
        // Mirrors the bitFlyer H1 warmup above. Without this, Donchian/Squeeze
        // on GmoFx would wait ~55 hours for H1 candles to accumulate after restart.
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
                "strategy warmup: fed {n} gmo_fx {H1_TIMEFRAME} candles for {}",
                pair.0
            );
        }
    }

    // Collect actually registered strategy names for trading_account validation.
    // Held as owned Strings so we can freely move it into async tasks.
    let registered_strategies: Vec<String> = engine
        .registered_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    // Trading accounts live in the database and are the source of truth. We do
    // NOT take a startup snapshot — every task (executor, position monitor,
    // overnight fees) re-reads the current account list from the DB so that
    // additions/updates/deletions via the REST API are picked up immediately.
    //
    // Note: FX (OANDA) paper trading is currently disabled at the executor
    // level. If you want FX paper trading, create an FX trading_account in the
    // database — the same pipeline will pick it up automatically once the
    // executor gate is relaxed.
    //
    // Log the accounts currently present at startup for visibility only.
    // Fatal if the DB query fails — we cannot validate live-safety preconditions
    // without this snapshot, so refusing to start is the correct behaviour.
    let db_accounts = match auto_trader_db::trading_accounts::list_all(&pool).await {
        Ok(v) => v,
        Err(e) => {
            anyhow::bail!(
                "failed to list trading accounts at startup: {e} — cannot validate live-trading preconditions, refusing to start"
            );
        }
    };
    if db_accounts.is_empty() {
        tracing::info!("no trading accounts found in DB at startup");
    }
    for pac in &db_accounts {
        tracing::info!(
            "trading account: {} (id={}, type={}, exchange={}, strategy={}, balance={} (initial={}), leverage={})",
            pac.name,
            pac.id,
            pac.account_type,
            pac.exchange,
            pac.strategy,
            pac.current_balance,
            pac.initial_balance,
            pac.leverage
        );
        if !registered_strategies.iter().any(|s| s == &pac.strategy) {
            tracing::warn!(
                "trading account '{}' references strategy '{}' which is not registered; signals for this strategy will be skipped",
                pac.name,
                pac.strategy
            );
        }
    }

    // `LIVE_DRY_RUN` env overrides `[live].dry_run` config.
    // Trim whitespace and lowercase before matching so " True\n" is valid.
    // Unknown values fall back to [live].dry_run and emit a warning.
    let live_cfg_dry_run = config.live.as_ref().is_some_and(|l| l.dry_run);
    let live_forces_dry_run: bool = match std::env::var("LIVE_DRY_RUN").ok() {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                tracing::warn!(
                    "invalid LIVE_DRY_RUN value {:?} (expected 1/true/yes/on/0/false/no/off); falling back to [live].dry_run={}",
                    raw,
                    live_cfg_dry_run
                );
                live_cfg_dry_run
            }
        },
        None => live_cfg_dry_run,
    };
    // Runtime gate: [live].enabled=false means live accounts are skipped at
    // open and close paths even if they were added after startup via REST.
    let live_enabled = config.live.as_ref().is_some_and(|l| l.enabled);

    if let Some(ps) = config.position_sizing.as_ref() {
        tracing::info!(
            "position_sizing config (method='{}', risk_rate={}) is ignored — sizing is now per-strategy via Signal.allocation_pct",
            ps.method,
            ps.risk_rate
        );
    }

    // pair_configs for UnifiedTrader (min_order_size etc.)
    let pair_configs: Arc<HashMap<String, auto_trader_core::config::PairConfig>> =
        Arc::new(config.pair_config.clone());

    // Per-exchange liquidation margin levels — required for any active account.
    // Fail-closed: if config lacks an entry for an exchange used by an active
    // account, abort startup before any trading task spawns.
    let exchange_liquidation_levels: Arc<HashMap<auto_trader_core::types::Exchange, Decimal>> =
        Arc::new(auto_trader::startup::resolve_exchange_liquidation_levels(&pool, &config).await?);

    // Pre-compute the PositionSizer once at startup and share via Arc.
    // Per-tick reconstruction (every SL/TP check, every strategy exit, every
    // signal dispatch) was wasting per-iteration allocations + hashing.
    let shared_position_sizer: Arc<auto_trader_executor::position_sizer::PositionSizer> = {
        let min_order_sizes: HashMap<Pair, Decimal> = pair_configs
            .iter()
            .map(|(k, v)| (Pair::new(k), v.min_order_size))
            .collect();
        Arc::new(auto_trader_executor::position_sizer::PositionSizer::new(
            min_order_sizes,
        ))
    };

    // Freshness threshold for entry signals. Only the price_freshness_secs
    // field from RiskConfig is used post-revert.
    let price_freshness_secs: u64 = config
        .risk
        .as_ref()
        .map(|r| r.price_freshness_secs)
        .unwrap_or(60);

    let vegapunk_client_exec: Option<Arc<Mutex<auto_trader_vegapunk::client::VegapunkClient>>> =
        vegapunk_base.as_ref().map(|base| {
            Arc::new(Mutex::new(
                auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(
                    base,
                    &config.vegapunk.schema,
                ),
            ))
        });
    let vegapunk_client_recorder = vegapunk_client_exec.clone();

    // Build the market-feed registry — one entry per exchange that is
    // configured and has credentials. Adding a new exchange's price feed
    // = impl MarketFeed for NewFeed + insert here.
    let mut feeds: HashMap<Exchange, Box<dyn MarketFeed>> = HashMap::new();

    // OANDA feed (optional)
    if let Some(fx_monitor) = fx_monitor {
        feeds.insert(Exchange::Oanda, Box::new(fx_monitor));
    }

    // bitFlyer feed (optional)
    if let Some(bf_config) = &config.bitflyer {
        let crypto_pairs: Vec<Pair> = config
            .pairs
            .crypto
            .as_ref()
            .map(|v| v.iter().map(|s| Pair::new(s)).collect())
            .unwrap_or_default();
        if !crypto_pairs.is_empty() {
            // Register crypto pairs as expected feeds before
            // BitflyerMonitor takes ownership.
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

    // GMO Coin FX feed — always registered when FX pairs are configured.
    // GMO FX feed: always registered when FX pairs are configured.
    // Uses the Public REST API (no auth required) to poll ticker prices every
    // 5 seconds, building M5 + H1 candles via CandleBuilder.
    // Even if OANDA is also active, GMO uses Exchange::GmoFx (separate key)
    // so PriceEvents don't duplicate — strategies filter by account.exchange.
    {
        let gmo_fx_pairs: Vec<Pair> = if !config.pairs.active.is_empty() {
            config.pairs.active.iter().map(|s| Pair::new(s)).collect()
        } else {
            config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
        };
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
            tracing::info!("GMO FX feed registered for {} pair(s)", gmo_fx_pairs.len());
        } else {
            tracing::info!("no FX pairs configured, GMO FX feed disabled");
        }
    }
    // Build the price store once all monitor setup has had a chance
    // to populate `expected_feeds`. The store is shared between the
    // raw-tick drain task (which writes every websocket tick into
    // it) and the API server (which reads snapshots + health from
    // it). Note: the engine task does NOT write into the store
    // anymore, because it only sees M5-aggregated PriceEvents which
    // are too coarse for the 60s freshness threshold.
    // Build a lookup from exchange → set of pair names so the signal
    // executor can verify that a signal's pair actually belongs to the
    // account's exchange before dispatching. Without this, a BitflyerCfd
    // FX_BTC_JPY signal would match a GmoFx account running the same
    // strategy name, then fail with "no price available".
    let exchange_pairs: Arc<HashMap<Exchange, HashSet<String>>> = {
        let mut map: HashMap<Exchange, HashSet<String>> = HashMap::new();
        for fk in &expected_feeds {
            map.entry(fk.exchange).or_default().insert(fk.pair.0.clone());
        }
        Arc::new(map)
    };

    let price_store = price_store::PriceStore::new(expected_feeds);

    // Spawn all market feeds via the unified MarketFeed registry.
    // Each feed manages its own connection lifecycle; price_store and
    // price_tx are passed at run-time so feeds write ticks directly
    // (no intermediate raw-tick channel needed).
    // Collect handles so we can abort them on shutdown, mirroring the
    // old fx_monitor_handle / bitflyer_handle abort semantics.
    // Box<dyn MarketFeed> encodes single ownership; feeds are consumed
    // by the for loop and moved into each spawned task.
    let mut feed_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for (exchange, feed) in feeds {
        let feed_price_store = price_store.clone();
        let feed_price_tx = price_tx.clone();
        let exchange_label = exchange;
        let handle = tokio::spawn(async move {
            tracing::info!("starting market feed for {:?}", exchange_label);
            if let Err(e) = feed.run(feed_price_store, feed_price_tx).await {
                tracing::error!(
                    "market feed for {:?} exited with error: {e}",
                    exchange_label
                );
            }
        });
        feed_handles.push(handle);
    }

    // Task: Macro analyst (news -> summarize -> broadcast to strategies)
    let (macro_tx, _) =
        tokio::sync::broadcast::channel::<auto_trader_core::strategy::MacroUpdate>(16);
    let macro_rx = macro_tx.subscribe();
    let macro_analyst_handle = if config.macro_analyst.as_ref().is_some_and(|m| m.enabled) {
        let mac = config.macro_analyst.as_ref().unwrap();
        let gemini_api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => {
                tracing::warn!("GEMINI_API_KEY not set or empty, disabling macro analyst");
                None
            }
        };
        let gemini_config = config.gemini.as_ref();
        match (gemini_api_key, gemini_config) {
            (Some(_), None) | (None, _) => {
                tracing::info!("macro analyst: missing GEMINI_API_KEY or gemini config, skipping");
                None
            }
            (Some(api_key), Some(gemini_config)) => {
                let mut analyst = auto_trader_macro_analyst::analyst::MacroAnalyst::new(
                    mac.news_sources.clone(),
                    &gemini_config.api_url,
                    &api_key,
                    &gemini_config.model,
                )
                .with_db(pool.clone());

                // Clone from shared Vegapunk channel for macro event ingestion
                if let Some(base) = &vegapunk_base {
                    let vp_for_macro =
                        auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(
                            base,
                            &config.vegapunk.schema,
                        );
                    analyst = analyst.with_vegapunk(vp_for_macro);
                }

                let news_interval = std::time::Duration::from_secs(mac.news_interval_secs);
                let macro_tx_clone = macro_tx.clone();
                Some(tokio::spawn(async move {
                    if let Err(e) = analyst.run(macro_tx_clone, news_interval).await {
                        tracing::error!("macro analyst error: {e}");
                    }
                }))
            }
        }
    } else {
        tracing::info!("macro analyst disabled");
        None
    };

    // IMPORTANT: before spawning monitor/executor tasks, reconcile any live
    // DB rows that might have drifted during last shutdown. This is a one-time
    // recovery, not periodic. Unconditional: paper-only setups no-op; live
    // accounts with no exchange API correctly bail.
    startup_reconcile::reconcile_live_accounts_at_startup(
        &pool,
        &db_accounts,
        &exchange_apis,
        price_store.clone(),
    )
    .await
    .map_err(|e| {
        anyhow::anyhow!(
            "startup reconcile failed: {e}; refusing to start with potentially inconsistent state"
        )
    })?;

    // FX position monitor removed: FX paper trading is currently disabled.
    // Drain the forwarded FX price channel so senders do not block.
    let mut price_monitor_rx = price_monitor_rx;
    let pos_monitor_handle =
        tokio::spawn(async move { while price_monitor_rx.recv().await.is_some() {} });

    // Task: Crypto position monitor — single task, DB-driven.
    //
    // Rather than holding per-account Trader instances, we re-read the
    // open-trade list from the DB on every price tick. This makes the monitor
    // automatically track account additions/removals done via the REST API.
    let (crypto_price_tx, mut crypto_price_rx) = mpsc::channel::<PriceEvent>(256);
    let crypto_monitor_pool = pool.clone();
    let crypto_monitor_trade_tx = trade_tx.clone();
    let crypto_monitor_exchange_apis = exchange_apis.clone();
    let crypto_monitor_price_store = price_store.clone();
    let crypto_monitor_notifier = notifier.clone();
    let crypto_monitor_position_sizer = shared_position_sizer.clone();
    let crypto_monitor_exchange_liquidation_levels = exchange_liquidation_levels.clone();
    let crypto_monitor_live_forces_dry_run = live_forces_dry_run;
    let crypto_monitor_handle = tokio::spawn(async move {
        while let Some(event) = crypto_price_rx.recv().await {
            let current_price = event.candle.close;
            let open_trades =
                match auto_trader_db::trades::list_open_with_account_name(&crypto_monitor_pool)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("crypto monitor: failed to list open trades: {e}");
                        continue;
                    }
                };
            for owned in open_trades {
                let trade = owned.trade;
                // Match by exchange + pair so GmoFx trades are monitored against
                // GmoFx price events, and BitflyerCfd trades against BitflyerCfd events.
                if trade.exchange != event.exchange || trade.pair != event.pair {
                    continue;
                }
                let account_id = trade.account_id;
                // account_type comes from the JOIN in list_open_with_account_name —
                // no extra DB round-trip needed. A missing JOIN result means the
                // account row was deleted; skip rather than silently degrade to
                // paper mode (same rationale as the old get_account path).
                let account_type = match owned.account_type.as_deref() {
                    Some(t) => t.to_owned(),
                    None => {
                        tracing::warn!(
                            "skipping close for trade {}: account {} not found in JOIN result",
                            trade.id,
                            account_id
                        );
                        continue;
                    }
                };
                let account_name = owned.account_name.unwrap_or_else(|| account_id.to_string());
                // [live].enabled=false blocks NEW live orders only, not existing-position
                // close paths. A position opened when enabled=true must remain closable
                // even after the operator toggles enabled=false for safety reasons.
                let dry_run = account_type == "paper" || crypto_monitor_live_forces_dry_run;
                // Time-based fail-safe — strategies that wrote a
                // `max_hold_until` get force-closed at the current price
                // when the wall clock passes the deadline. Tagged with
                // the dedicated StrategyTimeLimit ExitReason so analytics
                // can attribute these closes correctly.
                let now = chrono::Utc::now();
                let time_limit_hit = trade.max_hold_until.is_some_and(|deadline| now >= deadline);

                let mut exit_reason = match trade.direction {
                    Direction::Long => {
                        if current_price <= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if trade.take_profit.is_some_and(|tp| current_price >= tp) {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                    Direction::Short => {
                        if current_price >= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if trade.take_profit.is_some_and(|tp| current_price <= tp) {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                };
                if exit_reason.is_none() && time_limit_hit {
                    exit_reason = Some(auto_trader_core::types::ExitReason::StrategyTimeLimit);
                }
                if let Some(reason) = exit_reason {
                    // Live accounts require a real ExchangeApi. Paper/dry_run accounts fill
                    // from PriceStore and never call API methods, so NullExchangeApi is safe
                    // when no real implementation exists yet (e.g. GMO Coin FX).
                    let api: std::sync::Arc<dyn auto_trader_market::exchange_api::ExchangeApi> =
                        match crypto_monitor_exchange_apis.get(&trade.exchange) {
                            Some(a) => a.clone(),
                            None => {
                                if !dry_run {
                                    tracing::warn!(
                                        "no ExchangeApi registered for exchange {:?}; skipping close for live trade {}",
                                        trade.exchange,
                                        trade.id
                                    );
                                    continue;
                                }
                                std::sync::Arc::new(
                                    auto_trader_market::null_exchange_api::NullExchangeApi,
                                )
                            }
                        };
                    let liquidation_margin_level = match auto_trader::startup::liquidation_level_or_log(
                        &crypto_monitor_exchange_liquidation_levels,
                        trade.exchange,
                        || format!("close trade {}", trade.id),
                    ) {
                        Some(y) => y,
                        None => continue,
                    };
                    let trader = UnifiedTrader::new(
                        crypto_monitor_pool.clone(),
                        trade.exchange,
                        account_id,
                        account_name,
                        api,
                        crypto_monitor_price_store.clone(),
                        crypto_monitor_notifier.clone(),
                        crypto_monitor_position_sizer.clone(),
                        liquidation_margin_level,
                        dry_run,
                    );
                    match trader.close_position(&trade.id.to_string(), reason).await {
                        Ok(closed_trade) => {
                            let exit_price = closed_trade.exit_price.unwrap_or(current_price);
                            tracing::info!(
                                "position closed: {} {} {:?} at {} ({:?})",
                                closed_trade.strategy_name,
                                closed_trade.pair,
                                closed_trade.direction,
                                exit_price,
                                reason
                            );
                            if let Err(e) = crypto_monitor_trade_tx
                                .send(TradeEvent {
                                    trade: closed_trade,
                                    action: TradeAction::Closed {
                                        exit_price,
                                        exit_reason: reason,
                                    },
                                    account_type: Some(account_type.clone()),
                                })
                                .await
                            {
                                tracing::error!(
                                    "trade channel send failed for position close: {e}"
                                );
                            }
                        }
                        Err(e) => {
                            // Concurrent close losers land here — log at debug.
                            tracing::debug!(
                                "close_position skipped/failed for trade {}: {e}",
                                trade.id
                            );
                        }
                    }
                }
            }
        }
        tracing::info!("crypto position monitor: price channel closed, stopping");
    });

    // Channel for strategy-driven exit signals (trailing stops, indicator
    // reversals, etc.) emitted from `Strategy::on_open_positions`. The
    // strategy engine task pushes onto this channel and a dedicated
    // executor task drains it. Buffer is small because exits per tick are
    // bounded by the number of open positions.
    //
    // IMPORTANT: `exit_tx` is moved into the engine task below so that
    // when the engine task exits (price channel closed) the sender drops
    // and the executor task's `recv()` returns None, allowing graceful
    // shutdown to complete inside the drain timeout.
    let (exit_tx, mut exit_rx) = mpsc::channel::<auto_trader_core::strategy::ExitSignal>(64);
    let engine_pool = pool.clone();
    // Engine still updates PriceStore from the (M5-cadence) candle
    // PriceEvent path. For bitflyer this is redundant with the
    // raw-tick drain (latest write wins, both are cheap), but for
    // any other exchange that does NOT have a raw-tick sink wired
    // up — currently OANDA — this is the only path that keeps the
    // store populated at all. Removing it again would silently
    // break OANDA's market-feed health and Positions display the
    // moment OANDA is re-enabled.
    let price_store_for_engine = price_store.clone();

    // Task: Strategy engine (price -> signal) + forward to position monitors
    // Also receives macro updates from broadcast channel.
    // Design note: select! is unbiased here. Macro updates arrive at ~30min intervals,
    // so starvation of the price path is not a practical concern in Phase 0.
    let engine_handle = tokio::spawn(async move {
        let mut macro_rx = macro_rx;
        loop {
            tokio::select! {
                price = price_rx.recv() => {
                    match price {
                        Some(event) => {
                            // Update PriceStore from the candle path.
                            // For exchanges with no raw-tick sink
                            // (currently OANDA) this is the only
                            // refresh source. For bitflyer it is
                            // redundant with the raw-tick drain;
                            // both are cheap and the latest write
                            // wins.
                            price_store_for_engine
                                .update(
                                    price_store::FeedKey::new(
                                        event.exchange,
                                        event.pair.clone(),
                                    ),
                                    price_store::LatestTick {
                                        price: event.candle.close,
                                        best_bid: event.candle.best_bid,
                                        best_ask: event.candle.best_ask,
                                        ts: event.timestamp,
                                    },
                                )
                                .await;
                            // Forward to FX position monitor drain (legacy channel; kept so
                            // senders don't block — actual FX monitor is not yet implemented).
                            if event.exchange == auto_trader_core::types::Exchange::Oanda
                                && price_monitor_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("FX position monitor channel closed");
                            }
                            // Forward to the unified position monitor (crypto + GmoFx).
                            // The monitor filters by trade.exchange == event.exchange so
                            // BitflyerCfd and GmoFx trades are each checked against the
                            // correct price feed.
                            if (event.exchange == auto_trader_core::types::Exchange::BitflyerCfd
                                || event.exchange == auto_trader_core::types::Exchange::GmoFx)
                                && crypto_price_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("position monitor channel closed");
                            }

                            // Build per-strategy open-position view from the
                            // DB so strategies can decide trailing/indicator
                            // exits. We use a per-pair query (filtered in
                            // SQL by exchange/pair) so this stays cheap
                            // as the open-trade table grows. The crypto
                            // position monitor still does its own scan
                            // for SL/TP — those run in different tasks
                            // with different needs (per-strategy grouping
                            // here, account-level joins there).
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
                                    tracing::warn!("engine: failed to list open trades for on_open_positions: {e}");
                                    std::collections::HashMap::new()
                                }
                            };

                            let exits = engine
                                .on_price_with_positions(&event, &positions_by_strategy)
                                .await;
                            for exit in exits {
                                if exit_tx.send(exit).await.is_err() {
                                    tracing::warn!("exit channel closed, dropping strategy exit signal");
                                }
                            }
                        }
                        None => break, // price channel closed
                    }
                }
                macro_update = macro_rx.recv() => {
                    match macro_update {
                        Ok(update) => engine.on_macro_update(&update),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("macro broadcast lagged, skipped {n} updates");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            tracing::info!("macro broadcast channel closed");
                        }
                    }
                }
            }
        }
    });

    // Task: Strategy-driven exit executor. Pulls ExitSignals from the
    // engine task and force-closes the referenced trades via UnifiedTrader.
    // The fixed SL/TP path in the crypto position monitor still runs
    // independently — these two paths are complementary.
    //
    // After a successful close we ALSO push a TradeEvent::Closed onto
    // `trade_tx` so the recorder task (daily_summary upsert + Vegapunk
    // ingest) sees the close, just like SL/TP closes do. Without this
    // forwarding, strategy-driven closes would silently disappear from
    // the daily summary and the analytics dashboard.
    let exit_pool = pool.clone();
    let exit_trade_tx = trade_tx.clone();
    let exit_exchange_apis = exchange_apis.clone();
    let exit_price_store = price_store.clone();
    let exit_notifier = notifier.clone();
    let exit_position_sizer = shared_position_sizer.clone();
    let exit_exchange_liquidation_levels = exchange_liquidation_levels.clone();
    let exit_live_forces_dry_run = live_forces_dry_run;
    let exit_executor_handle = tokio::spawn(async move {
        while let Some(exit) = exit_rx.recv().await {
            // Look up the trade joined with account info in one query so we
            // avoid a separate get_account() call (N+1 elimination).
            let owned = match auto_trader_db::trades::get_open_trade_with_account(
                &exit_pool,
                exit.trade_id,
            )
            .await
            {
                Ok(Some(r)) => r,
                Ok(None) => {
                    // Either missing or already closed (SL/TP fired in the same tick).
                    tracing::debug!(
                        "exit signal references missing/closed trade {}",
                        exit.trade_id
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!("exit executor: failed to load trade {}: {e}", exit.trade_id);
                    continue;
                }
            };
            let trade = owned.trade;
            let account_id = trade.account_id;
            let account_type = match owned.account_type.as_deref() {
                Some(t) => t.to_owned(),
                None => {
                    tracing::warn!(
                        "skipping strategy exit for trade {}: account {} not found in JOIN result",
                        trade.id,
                        account_id
                    );
                    continue;
                }
            };
            let account_name = owned.account_name.unwrap_or_else(|| account_id.to_string());
            // [live].enabled=false blocks NEW live orders only, not existing-position
            // close paths. A position opened when enabled=true must remain closable
            // even after the operator toggles enabled=false for safety reasons.
            let dry_run = account_type == "paper" || exit_live_forces_dry_run;
            // Live accounts require a real ExchangeApi. Paper/dry_run accounts fill
            // from PriceStore and never call API methods, so a NullExchangeApi stub
            // is safe when no real implementation exists yet (e.g. GMO Coin FX).
            let api: std::sync::Arc<dyn auto_trader_market::exchange_api::ExchangeApi> =
                match exit_exchange_apis.get(&trade.exchange) {
                    Some(a) => a.clone(),
                    None => {
                        if !dry_run {
                            tracing::warn!(
                                "no ExchangeApi registered for exchange {:?}; skipping exit for live trade {}",
                                trade.exchange,
                                exit.trade_id
                            );
                            continue;
                        }
                        std::sync::Arc::new(auto_trader_market::null_exchange_api::NullExchangeApi)
                    }
                };
            let liquidation_margin_level = match auto_trader::startup::liquidation_level_or_log(
                &exit_exchange_liquidation_levels,
                trade.exchange,
                || format!("strategy exit on trade {}", trade.id),
            ) {
                Some(y) => y,
                None => continue,
            };
            let trader = UnifiedTrader::new(
                exit_pool.clone(),
                trade.exchange,
                account_id,
                account_name,
                api.clone(),
                exit_price_store.clone(),
                exit_notifier.clone(),
                exit_position_sizer.clone(),
                liquidation_margin_level,
                dry_run,
            );
            // Map the strategy-specific reason onto the ExitReason enum
            // so the trade row carries true attribution
            // (StrategyMeanReached / StrategyTrailingChannel / …) instead
            // of being squashed to Manual.
            let reason = exit.reason.to_exit_reason();
            match trader.close_position(&trade.id.to_string(), reason).await {
                Ok(closed) => {
                    tracing::info!(
                        "strategy exit: {} {} {:?} reason={} entry={} exit={}",
                        closed.strategy_name,
                        closed.pair,
                        closed.direction,
                        exit.reason.as_tag(),
                        closed.entry_price,
                        closed.exit_price.unwrap_or_default()
                    );
                    // Forward to recorder so daily_summary / Vegapunk
                    // ingest pick up this close just like SL/TP closes.
                    let exit_price = closed.exit_price.unwrap_or(exit.close_price);
                    if exit_trade_tx
                        .send(auto_trader_core::event::TradeEvent {
                            trade: closed,
                            action: auto_trader_core::event::TradeAction::Closed {
                                exit_price,
                                exit_reason: reason,
                            },
                            account_type: Some(account_type.clone()),
                        })
                        .await
                        .is_err()
                    {
                        tracing::warn!("trade channel closed, strategy exit not recorded");
                    }
                }
                Err(e) => {
                    tracing::warn!("strategy exit: failed to close trade {}: {e}", trade.id);
                }
            }
        }
    });

    // Task: Signal executor (signal -> trade)
    // Enforces 1-pair-1-position per strategy per account at execution time.
    // Both paper and live accounts (trading_accounts) are supported.
    //
    // Account list is re-read from the DB on every signal so REST API changes
    // (add/update/delete trading_accounts) are picked up without restart.
    let executor_pool = pool.clone();
    let trade_tx_clone = trade_tx.clone();
    let executor_exchange_apis = exchange_apis.clone();
    let executor_price_store = price_store.clone();
    let executor_notifier = notifier.clone();
    let executor_exchange_pairs = Arc::clone(&exchange_pairs);
    let executor_position_sizer = shared_position_sizer.clone();
    let executor_exchange_liquidation_levels = exchange_liquidation_levels.clone();
    let executor_live_forces_dry_run = live_forces_dry_run;
    let executor_live_enabled = live_enabled;
    let executor_price_freshness_secs = price_freshness_secs;
    let executor_handle = tokio::spawn(async move {
        while let Some(signal_event) = signal_rx.recv().await {
            let signal = &signal_event.signal;

            // Re-read accounts from the DB for each signal.
            let db_accounts = match auto_trader_db::trading_accounts::list_all(&executor_pool).await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("executor: failed to list trading accounts: {e}");
                    continue;
                }
            };

            // Dispatch signal to all accounts bound to this strategy (any exchange).
            let mut matched_strategy = false;
            let mut dispatched = false;
            for pac in &db_accounts {
                if pac.strategy != signal.strategy_name {
                    continue;
                }
                matched_strategy = true;
                let exchange = match exchange_from_str(&pac.exchange) {
                    Some(e) => e,
                    None => {
                        tracing::warn!(
                            "skipping account {} ({}): unknown exchange '{}'",
                            pac.name,
                            pac.id,
                            pac.exchange
                        );
                        continue;
                    }
                };
                // Guard: signal.pair must belong to this account's exchange.
                // Without this, a BitflyerCfd FX_BTC_JPY signal would match
                // a GmoFx account running the same strategy, then fail with
                // "no price available for FX_BTC_JPY".
                if !executor_exchange_pairs
                    .get(&exchange)
                    .is_some_and(|pairs| pairs.contains(&signal.pair.0))
                {
                    tracing::debug!(
                        "skipping signal: pair {} not available on exchange {:?} for account {}",
                        signal.pair.0,
                        exchange,
                        pac.name
                    );
                    continue;
                }
                // [live].enabled=false なら live 口座の signal を拒否し、発注経路に入れない。
                // 起動時 gate と belt-and-suspenders。runtime に REST で live 行が
                // 追加された場合も発注を防ぐ。
                if pac.account_type == "live" && !executor_live_enabled {
                    tracing::warn!(
                        "skipping signal for live account {} ({}): [live].enabled=false",
                        pac.name,
                        pac.id
                    );
                    continue;
                }
                let dry_run = pac.account_type == "paper" || executor_live_forces_dry_run;
                // Registry lookup — live accounts require a real ExchangeApi.
                // Paper/dry_run accounts fill from PriceStore and never call
                // API methods, so a NullExchangeApi stub is safe to use when
                // no real implementation exists yet (e.g. GMO Coin FX).
                let api: std::sync::Arc<dyn auto_trader_market::exchange_api::ExchangeApi> =
                    match executor_exchange_apis.get(&exchange) {
                        Some(a) => a.clone(),
                        None => {
                            if !dry_run {
                                tracing::warn!(
                                    "no ExchangeApi registered for exchange {:?}; skipping live account {} ({})",
                                    exchange,
                                    pac.name,
                                    pac.id
                                );
                                continue;
                            }
                            // dry_run: use a stub that errors on any call.
                            // Trader must not call it — if it does, the error
                            // surfaces immediately rather than silently using
                            // the wrong exchange's API.
                            std::sync::Arc::new(
                                auto_trader_market::null_exchange_api::NullExchangeApi,
                            )
                        }
                    };

                // Freshness gate: reject stale-tick signals at entry.
                // Use exchange-aware lookup so a fresh tick on a different
                // exchange cannot mask staleness on this account's exchange.
                let feed_key = price_store::FeedKey::new(exchange, signal.pair.clone());
                let last_tick_age = executor_price_store
                    .last_tick_age_for(&feed_key)
                    .await
                    .unwrap_or(u64::MAX);
                match eval_price_freshness(executor_price_freshness_secs, last_tick_age) {
                    GateDecision::Pass => {}
                    GateDecision::Reject(reason) => {
                        tracing::warn!(
                            "freshness gate rejected signal for {}: {:?}",
                            pac.name,
                            reason
                        );
                        continue;
                    }
                }

                let liquidation_margin_level = match auto_trader::startup::liquidation_level_or_log(
                    &executor_exchange_liquidation_levels,
                    exchange,
                    || format!("signal for account {} (id={})", pac.name, pac.id),
                ) {
                    Some(y) => y,
                    None => continue,
                };
                let trader = UnifiedTrader::new(
                    executor_pool.clone(),
                    exchange,
                    pac.id,
                    pac.name.clone(),
                    api.clone(),
                    executor_price_store.clone(),
                    executor_notifier.clone(),
                    executor_position_sizer.clone(),
                    liquidation_margin_level,
                    dry_run,
                );
                let name = pac.name.clone();
                dispatched = true;
                let positions = trader.open_positions().await.unwrap_or_default();
                let has_position = positions.iter().any(|p| {
                    p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair
                });
                if has_position {
                    tracing::debug!(
                        "skipping signal: {} already has open position for {} in account {}",
                        signal.strategy_name,
                        signal.pair,
                        name
                    );
                    continue;
                }

                match trader.execute(signal).await {
                    Ok(trade) => {
                        if let Some(vp) = vegapunk_client_exec.clone() {
                            let trade_clone = trade.clone();
                            let indicators_clone = signal_event.indicators.clone();
                            let alloc_pct = signal.allocation_pct;
                            let exec_pool = executor_pool.clone();
                            tokio::spawn(async move {
                                // Save entry_indicators to DB (with regime classification)
                                let mut ind_map = indicators_clone
                                    .iter()
                                    .map(|(k, v)| (k.clone(), serde_json::json!(v.to_string())))
                                    .collect::<serde_json::Map<_, _>>();
                                ind_map.insert(
                                    "regime".to_string(),
                                    serde_json::Value::String(
                                        crate::regime::classify(&indicators_clone)
                                            .as_str()
                                            .to_string(),
                                    ),
                                );
                                let ind_json = serde_json::Value::Object(ind_map);
                                if let Err(e) = sqlx::query(
                                    "UPDATE trades SET entry_indicators = $1 WHERE id = $2",
                                )
                                .bind(&ind_json)
                                .bind(trade_clone.id)
                                .execute(&exec_pool)
                                .await
                                {
                                    tracing::warn!("failed to save entry_indicators: {e}");
                                }

                                let mut vp = vp.lock().await;
                                let text = crate::enriched_ingest::format_trade_open(
                                    &trade_clone,
                                    &indicators_clone,
                                    Some(alloc_pct),
                                );
                                let channel =
                                    format!("{}-trades", trade_clone.pair.0.to_lowercase());
                                let timestamp = chrono::Utc::now().to_rfc3339();
                                if let Err(e) = vp
                                    .ingest_raw(&text, "trade_signal", &channel, &timestamp)
                                    .await
                                {
                                    tracing::warn!("vegapunk ingest failed for trade open: {e}");
                                }
                            });
                        }
                        if let Err(e) = trade_tx_clone
                            .send(TradeEvent {
                                trade,
                                action: TradeAction::Opened,
                                account_type: Some(pac.account_type.clone()),
                            })
                            .await
                        {
                            tracing::error!("trade channel send failed: {e}");
                        }
                    }
                    Err(e) => tracing::error!("execute error for account {}: {e}", name),
                }
            }
            if !matched_strategy {
                tracing::warn!(
                    "signal from '{}' had no matching trading account",
                    signal.strategy_name
                );
            } else if !dispatched {
                tracing::debug!(
                    "signal from '{}' matched strategy on account(s) but no account passed pre-dispatch gates",
                    signal.strategy_name
                );
            }
        }
    });

    // Task: Trade recorder — handles side effects after UnifiedTrader has already
    // persisted the trade to the DB. Responsibilities:
    //   - Upsert daily_summary on close
    //   - Fire-and-forget Vegapunk ingestion on close
    // Note: trade INSERT/UPDATE and balance changes are owned by UnifiedTrader.
    let recorder_pool = pool.clone();
    let recorder_handle = tokio::spawn(async move {
        while let Some(trade_event) = trade_rx.recv().await {
            match trade_event.action {
                TradeAction::Opened => {
                    // Nothing to record: UnifiedTrader already inserted the trade.
                }
                TradeAction::Closed { .. } => {
                    let t = &trade_event.trade;
                    if let (
                        Some(_exit_price),
                        Some(exit_at),
                        Some(pnl_amount),
                        Some(_exit_reason),
                    ) = (t.exit_price, t.exit_at, t.pnl_amount, t.exit_reason)
                    {
                        // Upsert daily summary
                        let date = exit_at.date_naive();
                        let win = if pnl_amount > Decimal::ZERO { 1 } else { 0 };
                        let account_id = t.account_id;
                        let account_type = trade_event
                            .account_type
                            .as_deref()
                            .unwrap_or("paper");
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
                            tracing::error!("upsert daily summary error: {e}");
                        }
                        // Fire-and-forget Vegapunk ingestion (don't block DB recording)
                        if let Some(vp) = vegapunk_client_recorder.clone() {
                            let t = t.clone();
                            let close_pool = recorder_pool.clone();
                            tokio::spawn(async move {
                                // Fetch entry_indicators from DB
                                let entry_ind: Option<serde_json::Value> = sqlx::query_scalar(
                                    "SELECT entry_indicators FROM trades WHERE id = $1",
                                )
                                .bind(t.id)
                                .fetch_optional(&close_pool)
                                .await
                                .unwrap_or(None)
                                .flatten();

                                // Fetch account balance context from trading_accounts
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

                                let text = crate::enriched_ingest::format_trade_close(
                                    &t,
                                    entry_ind.as_ref(),
                                    bal,
                                    init,
                                );
                                let channel = format!("{}-trades", t.pair.0.to_lowercase());
                                let timestamp = t
                                    .exit_at
                                    .map(|e| e.to_rfc3339())
                                    .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

                                let mut vp = vp.lock().await;
                                if let Err(e) = vp
                                    .ingest_raw(&text, "trade_result", &channel, &timestamp)
                                    .await
                                {
                                    tracing::warn!("vegapunk ingest failed for trade close: {e}");
                                }

                                // Auto-feedback if this trade had a Vegapunk search attached
                                let search_id: Option<uuid::Uuid> = sqlx::query_scalar(
                                    "SELECT vegapunk_search_id FROM trades WHERE id = $1",
                                )
                                .bind(t.id)
                                .fetch_optional(&close_pool)
                                .await
                                .unwrap_or(None)
                                .flatten();

                                if let Some(sid) = search_id {
                                    let sid = sid.to_string();
                                    let rating =
                                        crate::enriched_ingest::compute_feedback_rating(&t);
                                    let net_pnl = t.pnl_amount.unwrap_or_default() - t.fees;
                                    let regime = entry_ind
                                        .as_ref()
                                        .and_then(|i| i.get("regime"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    let comment = format!("PnL: {net_pnl}, regime: {regime}");
                                    if let Err(e) = vp.feedback(&sid, rating, &comment).await {
                                        tracing::warn!("vegapunk feedback failed: {e}");
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }
    });

    // Task: Daily batch (max_drawdown calculation at UTC 0:00)
    // On startup, idempotently recompute last 7 days to cover any missed batches.
    // update_daily_max_drawdown is safe to re-run (overwrites max_drawdown).
    let vegapunk_client_daily: Option<Arc<Mutex<auto_trader_vegapunk::client::VegapunkClient>>> =
        vegapunk_base.as_ref().map(|base| {
            Arc::new(Mutex::new(
                auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(
                    base,
                    &config.vegapunk.schema,
                ),
            ))
        });
    let vegapunk_client_weekly = vegapunk_client_daily.clone();
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

        // Idempotently recompute recent days to cover missed batches.
        // Configurable via monitor.backfill_days (default: 7).
        let backfill_days: i64 = config.monitor.backfill_days.unwrap_or(7) as i64;
        for i in (1..=backfill_days).rev() {
            let d = today - chrono::Duration::days(i);
            tracing::info!("daily batch startup backfill: {d}");
            if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(&daily_pool, d).await
            {
                tracing::error!("daily batch backfill failed for {d}: {e}");
            }
        }

        // Purge notifications that have been read for more than 30
        // days. Unread notifications are kept forever.
        match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
            Ok(n) if n > 0 => tracing::info!("purged {n} old read notifications"),
            Ok(_) => {}
            Err(e) => tracing::warn!("failed to purge old read notifications: {e}"),
        }

        let mut last_date = today;
        loop {
            interval.tick().await;
            let now_date = chrono::Utc::now().date_naive();
            if now_date != last_date {
                tracing::info!("running daily batch for {last_date}");
                if let Err(e) =
                    auto_trader_db::summary::update_daily_max_drawdown(&daily_pool, last_date).await
                {
                    tracing::error!("daily batch failed: {e}");
                }
                match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
                    Ok(n) if n > 0 => tracing::info!("purged {n} old read notifications"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("failed to purge old read notifications: {e}"),
                }

                // Daily: Vegapunk Merge for community detection consolidation
                if let Some(vp) = vegapunk_client_daily.clone() {
                    tokio::spawn(async move {
                        let mut client = vp.lock().await;
                        if let Err(e) = client.merge().await {
                            tracing::warn!("daily vegapunk merge failed: {e}");
                        } else {
                            tracing::info!("daily vegapunk merge completed");
                        }
                    });
                }

                // Weekly (Sunday JST): run evolution batch
                // `weekday()` requires the `Datelike` trait to be in scope.
                use chrono::Datelike as _;
                let jst_weekday = (chrono::Utc::now() + chrono::Duration::hours(9)).weekday();
                if jst_weekday == chrono::Weekday::Sun
                    && let Err(e) = crate::weekly_batch::run(
                        &daily_pool,
                        vegapunk_client_weekly.as_ref(),
                        &gemini_api_url,
                        &gemini_api_key,
                        &gemini_model,
                    )
                    .await
                {
                    tracing::error!("weekly evolution batch failed: {e}");
                }

                last_date = now_date;
            }
        }
    });

    // Task: Overnight fee (crypto paper accounts)
    // Apply 0.04%/day fee to open positions at UTC 0:00.
    // Since positions now live in the DB, this correctly applies fees to all
    // outstanding positions across restarts. The account list is re-read from
    // the DB at every tick so REST API changes are reflected immediately.
    let overnight_pool = pool.clone();
    let overnight_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let fee_rate = Decimal::new(4, 4); // 0.0004 = 0.04%
        let mut last_date = chrono::Utc::now().date_naive();
        loop {
            interval.tick().await;
            let today = chrono::Utc::now().date_naive();
            if today != last_date {
                // Apply overnight fees only to paper accounts (live accounts
                // pay fees directly to the exchange; we don't deduct them here).
                let accounts =
                    match auto_trader_db::trading_accounts::list_all(&overnight_pool).await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!("overnight fee: failed to list trading accounts: {e}");
                            last_date = today;
                            continue;
                        }
                    };
                for pac in accounts {
                    // Only paper accounts get overnight fees applied in-app.
                    if pac.account_type != "paper" {
                        continue;
                    }
                    let exchange = match exchange_from_str(&pac.exchange) {
                        Some(e) => e,
                        None => {
                            tracing::warn!(
                                "overnight fee: skipping account {} ({}): unknown exchange '{}'",
                                pac.name,
                                pac.id,
                                pac.exchange
                            );
                            continue;
                        }
                    };
                    if exchange != Exchange::BitflyerCfd {
                        continue;
                    }
                    // Compute fee for each open trade: fee = entry_price * quantity * fee_rate
                    let open_trades = match auto_trader_db::trades::get_open_trades_by_account(
                        &overnight_pool,
                        pac.id,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::error!(
                                "overnight fee: failed to list open trades for {}: {e}",
                                pac.name
                            );
                            continue;
                        }
                    };
                    let mut total_fee = Decimal::ZERO;
                    for trade in &open_trades {
                        let fee = (trade.entry_price * trade.quantity * fee_rate)
                            .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);
                        if fee.is_zero() {
                            continue;
                        }
                        // Apply overnight fee atomically: trades.fees CAS +
                        // balance deduction + account_events insert in one tx.
                        // apply_overnight_fee returns Ok(None) if the trade
                        // closed between the open-list fetch above and this
                        // tx (the CAS on `status='open'` skips it cleanly so
                        // a closing trade never gets double-charged).
                        let result = async {
                            let mut tx = overnight_pool.begin().await?;
                            let applied = auto_trader_db::trades::apply_overnight_fee(
                                &mut tx, pac.id, trade.id, fee,
                            )
                            .await?;
                            tx.commit().await?;
                            anyhow::Ok(applied)
                        }
                        .await;
                        match result {
                            Ok(Some(_new_balance)) => {
                                total_fee += fee;
                            }
                            Ok(None) => {
                                tracing::debug!(
                                    "overnight fee: skipping trade {} — closed before fee tx",
                                    trade.id
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "overnight fee: apply_overnight_fee failed for trade {}: {e}",
                                    trade.id
                                );
                            }
                        }
                    }
                    if total_fee > Decimal::ZERO {
                        tracing::info!("overnight fee applied: {} = {} JPY", pac.name, total_fee);
                    }
                }
                last_date = today;
            }
        }
    });

    // REST API server
    let api_state = api::AppState {
        pool: pool.clone(),
        price_store: price_store.clone(),
        exchange_liquidation_levels: exchange_liquidation_levels.clone(),
    };
    let api_handle = tokio::spawn(async move {
        let app = api::router(api_state);
        // Always bind to 0.0.0.0. Host-level access control is handled by
        // docker-compose port mapping (127.0.0.1:3001:3001) or firewall.
        let bind_addr = "0.0.0.0:3001";
        let listener = tokio::net::TcpListener::bind(bind_addr)
            .await
            .expect("failed to bind API server");
        tracing::info!("API server listening on {bind_addr}");
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("API server error: {e}");
        }
    });

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down... draining channels");

    // Drop senders to signal downstream tasks to finish
    drop(price_tx);
    drop(trade_tx); // allow recorder to drain and exit
    // Abort feed tasks so they release their price_tx clones and let
    // the engine see a closed channel → clean exit.
    for h in feed_handles {
        h.abort();
    }
    overnight_handle.abort();
    daily_handle.abort(); // infinite loop — must abort explicitly
    if let Some(h) = macro_analyst_handle {
        h.abort();
    }
    api_handle.abort();

    // Wait for downstream tasks to drain (max 5 seconds)
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
