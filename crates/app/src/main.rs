mod api;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Exchange, Pair};
use auto_trader_db::pool::create_pool;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_strategy::trend_follow::TrendFollowV1;
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

fn exchange_from_str(s: &str) -> Exchange {
    match s {
        "bitflyer_cfd" => Exchange::BitflyerCfd,
        _ => Exchange::Oanda,
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

    let config_path = std::env::var("CONFIG_PATH")
        .unwrap_or_else(|_| "config/default.toml".to_string());
    let config = AppConfig::load(&PathBuf::from(&config_path))?;
    tracing::info!("config loaded from {config_path}");

    // Database
    let pool = create_pool(&config.database.url).await?;
    tracing::info!("database connected");

    // Channels — price は position_monitor にも配信するため 2 本
    let (price_tx, mut price_rx) = mpsc::channel::<PriceEvent>(256);
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(256);
    let (signal_tx, mut signal_rx) = mpsc::channel::<SignalEvent>(256);
    let (trade_tx, mut trade_rx) = mpsc::channel::<TradeEvent>(256);

    // FX market monitor (optional — skipped if no FX pairs or OANDA_API_KEY not set)
    let fx_pairs: Vec<Pair> = if !config.pairs.active.is_empty() {
        config.pairs.active.iter().map(|s| Pair::new(s)).collect()
    } else {
        config.pairs.fx.iter().map(|s| Pair::new(s)).collect()
    };
    let fx_monitor: Option<MarketMonitor> = if !fx_pairs.is_empty() {
        match (std::env::var("OANDA_API_KEY"), config.oanda.as_ref()) {
            (Ok(api_key), Some(oanda_config)) if !api_key.trim().is_empty() => {
                let account_id = std::env::var("OANDA_ACCOUNT_ID")
                    .unwrap_or_else(|_| oanda_config.account_id.clone());
                let oanda = OandaClient::new(&oanda_config.api_url, &account_id, &api_key)?;
                Some(MarketMonitor::new(oanda, fx_pairs, config.monitor.interval_secs, price_tx.clone())
                    .with_db(pool.clone()))
            }
            _ => {
                tracing::info!("OANDA not configured or API key not set, FX monitor disabled");
                None
            }
        }
    } else {
        tracing::info!("no FX pairs configured, FX monitor disabled");
        None
    };

    // Warn if FX strategies are enabled but FX monitor is not running
    if fx_monitor.is_none() {
        let has_fx_strategy = config.strategies.iter().any(|s| {
            s.enabled && (s.name.starts_with("trend_follow") || s.name.starts_with("swing_llm"))
        });
        if has_fx_strategy {
            tracing::warn!(
                "FX strategies are enabled but FX monitor is not running (OANDA not configured). \
                 These strategies will not receive price data."
            );
        }
    }

    // Vegapunk: single gRPC channel with optional Bearer token auth
    let vegapunk_auth_token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();
    let vegapunk_base: Option<auto_trader_vegapunk::client::VegapunkClient> =
        match auto_trader_vegapunk::client::VegapunkClient::connect(
            &config.vegapunk.endpoint, &config.vegapunk.schema,
            vegapunk_auth_token.as_deref(),
        ).await {
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
    for sc in &config.strategies {
        if !sc.enabled {
            continue;
        }
        match sc.name.as_str() {
            name if name.starts_with("trend_follow") => {
                let ma_short = sc.params.get("ma_short")
                    .and_then(|v| v.as_integer()).unwrap_or(20) as usize;
                let ma_long = sc.params.get("ma_long")
                    .and_then(|v| v.as_integer()).unwrap_or(50) as usize;
                let rsi_thresh = sc.params.get("rsi_threshold")
                    .and_then(|v| v.as_integer()).unwrap_or(70);
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(TrendFollowV1::new(
                        sc.name.clone(),
                        ma_short,
                        ma_long,
                        Decimal::from(rsi_thresh),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("swing_llm") => {
                let holding_days_max = sc.params.get("holding_days_max")
                    .and_then(|v| v.as_integer()).unwrap_or(14) as u32;
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();

                let gemini_api_key = match std::env::var("GEMINI_API_KEY") {
                    Ok(value) if !value.trim().is_empty() => value,
                    _ => {
                        tracing::warn!("GEMINI_API_KEY not set or empty, skipping strategy: {}", sc.name);
                        continue;
                    }
                };
                let gemini_config = match config.gemini.as_ref() {
                    Some(c) => c,
                    None => {
                        tracing::warn!("gemini config missing, skipping strategy: {}", sc.name);
                        continue;
                    }
                };

                // Clone from shared Vegapunk channel (no new TCP connection)
                let vp_client = match &vegapunk_base {
                    Some(base) => auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(
                        base, &config.vegapunk.schema,
                    ),
                    None => {
                        tracing::warn!("vegapunk unavailable, skipping strategy: {}", sc.name);
                        continue;
                    }
                };

                engine.add_strategy(
                    Box::new(auto_trader_strategy::swing_llm::SwingLLMv1::new(
                        sc.name.clone(),
                        pairs,
                        holding_days_max,
                        vp_client,
                        gemini_config.api_url.clone(),
                        gemini_api_key,
                        gemini_config.model.clone(),
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            name if name.starts_with("crypto_trend") => {
                let ma_short = sc.params.get("ma_short")
                    .and_then(|v| v.as_integer()).unwrap_or(8) as usize;
                let ma_long = sc.params.get("ma_long")
                    .and_then(|v| v.as_integer()).unwrap_or(21) as usize;
                let rsi_thresh = sc.params.get("rsi_threshold")
                    .and_then(|v| v.as_integer()).unwrap_or(75);
                let pairs = sc.pairs.iter().map(|s| Pair::new(s)).collect();
                engine.add_strategy(
                    Box::new(auto_trader_strategy::crypto_trend::CryptoTrendV1::new(
                        sc.name.clone(),
                        ma_short,
                        ma_long,
                        Decimal::from(rsi_thresh),
                        pairs,
                    )),
                    sc.mode.clone(),
                );
                tracing::info!("strategy registered: {} (mode={})", sc.name, sc.mode);
            }
            other => {
                tracing::warn!("unknown strategy: {other}, skipping");
            }
        }
    }

    // Collect actually registered strategy names for paper_account validation.
    // Held as owned Strings so we can freely move it into async tasks.
    let registered_strategies: Vec<String> = engine
        .registered_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    // Paper accounts live in the database and are the source of truth. We do
    // NOT take a startup snapshot — every task (executor, position monitor,
    // overnight fees) re-reads the current account list from the DB so that
    // additions/updates/deletions via the REST API are picked up immediately.
    //
    // Note: FX (OANDA) paper trading is currently disabled at the executor
    // level. If you want FX paper trading, create an FX paper_account in the
    // database — the same pipeline will pick it up automatically once the
    // executor gate is relaxed.
    //
    // Log the accounts currently present at startup for visibility only.
    match auto_trader_db::paper_accounts::list_paper_accounts(&pool).await {
        Ok(db_accounts) => {
            if db_accounts.is_empty() {
                tracing::info!("no paper accounts found in DB at startup");
            }
            for pac in &db_accounts {
                tracing::info!(
                    "paper account: {} (id={}, exchange={}, strategy={}, balance={} (initial={}), leverage={})",
                    pac.name, pac.id, pac.exchange, pac.strategy,
                    pac.current_balance, pac.initial_balance, pac.leverage
                );
                if !registered_strategies.iter().any(|s| s == &pac.strategy) {
                    tracing::warn!(
                        "paper account '{}' references strategy '{}' which is not registered; signals for this strategy will be skipped",
                        pac.name, pac.strategy
                    );
                }
            }
        }
        Err(e) => {
            tracing::error!("failed to list paper accounts at startup: {e}");
        }
    }


    // PositionSizer for crypto
    let min_order_sizes: HashMap<Pair, Decimal> = config
        .pair_config
        .iter()
        .map(|(k, v)| (Pair::new(k), v.min_order_size))
        .collect();
    let risk_rate = config
        .position_sizing
        .as_ref()
        .map(|ps| {
            if ps.method != "risk_based" {
                tracing::warn!("position_sizing.method='{}' is not supported, using risk_based", ps.method);
            }
            ps.risk_rate
        })
        .unwrap_or(Decimal::new(2, 2)); // default 0.02
    let position_sizer = Arc::new(PositionSizer::new(risk_rate, min_order_sizes));

    let vegapunk_client_exec: Option<Arc<Mutex<auto_trader_vegapunk::client::VegapunkClient>>> =
        vegapunk_base.as_ref().map(|base| {
            Arc::new(Mutex::new(
                auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(base, &config.vegapunk.schema),
            ))
        });
    let vegapunk_client_recorder = vegapunk_client_exec.clone();

    // Task: FX Market monitor (optional)
    let fx_monitor_handle = fx_monitor.map(|monitor| {
        tokio::spawn(async move {
            if let Err(e) = monitor.run().await {
                tracing::error!("FX monitor error: {e}");
            }
        })
    });

    // bitFlyer monitor (crypto)
    let bitflyer_handle = if let Some(bf_config) = &config.bitflyer {
        let crypto_pairs: Vec<Pair> = config
            .pairs
            .crypto
            .as_ref()
            .map(|v| v.iter().map(|s| Pair::new(s)).collect())
            .unwrap_or_default();
        if !crypto_pairs.is_empty() {
            let bf_monitor = auto_trader_market::bitflyer::BitflyerMonitor::new(
                &bf_config.ws_url,
                crypto_pairs,
                "M5",
                price_tx.clone(),
            )
            .with_db(pool.clone());
            Some(tokio::spawn(async move {
                if let Err(e) = bf_monitor.run().await {
                    tracing::error!("bitflyer monitor error: {e}");
                }
            }))
        } else {
            None
        }
    } else {
        None
    };

    // Task: Macro analyst (news -> summarize -> broadcast to strategies)
    let (macro_tx, _) = tokio::sync::broadcast::channel::<auto_trader_core::strategy::MacroUpdate>(16);
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
                ).with_db(pool.clone());

                // Clone from shared Vegapunk channel for macro event ingestion
                if let Some(base) = &vegapunk_base {
                    let vp_for_macro = auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(
                        base, &config.vegapunk.schema,
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

    // FX position monitor removed: FX paper trading is currently disabled.
    // Drain the forwarded FX price channel so senders do not block.
    let mut price_monitor_rx = price_monitor_rx;
    let pos_monitor_handle = tokio::spawn(async move {
        while price_monitor_rx.recv().await.is_some() {}
    });

    // Task: Crypto position monitor — single task, DB-driven.
    //
    // Rather than holding per-account PaperTrader instances, we re-read the
    // open-trade list from the DB on every price tick. This makes the monitor
    // automatically track account additions/removals done via the REST API.
    let (crypto_price_tx, mut crypto_price_rx) = mpsc::channel::<PriceEvent>(256);
    let crypto_monitor_pool = pool.clone();
    let crypto_monitor_trade_tx = trade_tx.clone();
    let crypto_monitor_handle = tokio::spawn(async move {
        while let Some(event) = crypto_price_rx.recv().await {
            let current_price = event.candle.close;
            let open_trades = match auto_trader_db::trades::list_open_with_account_name(
                &crypto_monitor_pool,
            ).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("crypto monitor: failed to list open trades: {e}");
                    continue;
                }
            };
            for owned in open_trades {
                let trade = owned.trade;
                if trade.exchange != Exchange::BitflyerCfd || trade.pair != event.pair {
                    continue;
                }
                let Some(account_id) = trade.paper_account_id else {
                    continue;
                };
                let exit_reason = match trade.direction {
                    Direction::Long => {
                        if current_price <= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if current_price >= trade.take_profit {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                    Direction::Short => {
                        if current_price >= trade.stop_loss {
                            Some(auto_trader_core::types::ExitReason::SlHit)
                        } else if current_price <= trade.take_profit {
                            Some(auto_trader_core::types::ExitReason::TpHit)
                        } else {
                            None
                        }
                    }
                };
                if let Some(reason) = exit_reason {
                    let exit_price = match reason {
                        auto_trader_core::types::ExitReason::SlHit => trade.stop_loss,
                        auto_trader_core::types::ExitReason::TpHit => trade.take_profit,
                        _ => current_price,
                    };
                    let trader = PaperTrader::new(
                        crypto_monitor_pool.clone(),
                        Exchange::BitflyerCfd,
                        account_id,
                    );
                    match trader
                        .close_position(&trade.id.to_string(), reason, exit_price)
                        .await
                    {
                        Ok(closed_trade) => {
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
                            // Forward to FX position monitor (FX events only)
                            if event.exchange == auto_trader_core::types::Exchange::Oanda
                                && price_monitor_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("FX position monitor channel closed");
                            }
                            // Forward to the single crypto position monitor (crypto events only)
                            if event.exchange == auto_trader_core::types::Exchange::BitflyerCfd
                                && crypto_price_tx.send(event.clone()).await.is_err()
                            {
                                tracing::warn!("crypto position monitor channel closed");
                            }
                            engine.on_price(&event).await;
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

    // Task: Signal executor (signal -> trade)
    // Enforces 1-pair-1-position per strategy per account at execution time.
    // Only crypto paper_accounts are supported — FX paper trading is disabled.
    //
    // Account list is re-read from the DB on every signal so REST API changes
    // (add/update/delete paper_accounts) are picked up without restart.
    let executor_pool = pool.clone();
    let executor_sizer = position_sizer.clone();
    let trade_tx_clone = trade_tx.clone();
    let crypto_pairs_set: Vec<String> = config
        .pairs
        .crypto
        .clone()
        .unwrap_or_default();
    let executor_handle = tokio::spawn(async move {
        while let Some(signal_event) = signal_rx.recv().await {
            let signal = &signal_event.signal;
            let is_crypto = crypto_pairs_set.iter().any(|p| p == &signal.pair.0);

            if !is_crypto {
                tracing::debug!(
                    "ignoring non-crypto signal: {} {} (FX paper trading disabled)",
                    signal.strategy_name, signal.pair
                );
                continue;
            }

            // Re-read accounts from the DB for each signal.
            let db_accounts = match auto_trader_db::paper_accounts::list_paper_accounts(
                &executor_pool,
            )
            .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("executor: failed to list paper accounts: {e}");
                    continue;
                }
            };

            // Crypto: dispatch signal only to paper_accounts bound to this strategy.
            let mut matched = false;
            for pac in &db_accounts {
                if pac.strategy != signal.strategy_name {
                    continue;
                }
                // Only dispatch to crypto accounts here.
                let exchange = exchange_from_str(&pac.exchange);
                if exchange != Exchange::BitflyerCfd {
                    continue;
                }
                let account = PaperTrader::new(executor_pool.clone(), exchange, pac.id);
                let name = &pac.name;
                matched = true;
                let positions = account.open_positions().await.unwrap_or_default();
                let has_position = positions.iter().any(|p| {
                    p.trade.strategy_name == signal.strategy_name
                        && p.trade.pair == signal.pair
                });
                if has_position {
                    tracing::debug!(
                        "skipping signal: {} already has open position for {} in account {}",
                        signal.strategy_name, signal.pair, name
                    );
                    continue;
                }

                // Use values from the fresh DB read above rather than round-tripping.
                let balance = pac.current_balance;
                let leverage = pac.leverage;
                let quantity = executor_sizer.calculate_quantity(
                    &signal.pair,
                    balance,
                    signal.entry_price,
                    signal.stop_loss,
                    leverage,
                );
                let Some(qty) = quantity else {
                    tracing::info!(
                        "position sizing rejected signal for account {}: {} {}",
                        name, signal.strategy_name, signal.pair
                    );
                    continue;
                };

                match account.execute_with_quantity(signal, qty).await {
                    Ok(trade) => {
                        if let Some(vp) = vegapunk_client_exec.clone() {
                            let trade_clone = trade.clone();
                            tokio::spawn(async move {
                                let mut vp = vp.lock().await;
                                let direction_str = match trade_clone.direction {
                                    Direction::Long => "ロング",
                                    Direction::Short => "ショート",
                                };
                                let text = format!(
                                    "[{}] {} {} 判断。trade_id: {}。エントリー価格: {}。qty: {}。SL: {}、TP: {}。戦略: {}",
                                    trade_clone.exchange, trade_clone.pair, direction_str,
                                    trade_clone.id, trade_clone.entry_price,
                                    trade_clone.quantity.map(|q| q.to_string()).unwrap_or_default(),
                                    trade_clone.stop_loss, trade_clone.take_profit,
                                    trade_clone.strategy_name
                                );
                                let channel = format!("{}-trades", trade_clone.pair.0.to_lowercase());
                                let timestamp = chrono::Utc::now().to_rfc3339();
                                if let Err(e) = vp.ingest_raw(&text, "trade_signal", &channel, &timestamp).await {
                                    tracing::warn!("vegapunk ingest failed for trade open: {e}");
                                }
                            });
                        }
                        if let Err(e) = trade_tx_clone.send(TradeEvent {
                            trade,
                            action: TradeAction::Opened,
                        }).await {
                            tracing::error!("trade channel send failed: {e}");
                        }
                    }
                    Err(e) => tracing::error!("execute error for account {}: {e}", name),
                }
            }
            if !matched {
                tracing::warn!(
                    "crypto signal from '{}' had no matching paper_account",
                    signal.strategy_name
                );
            }
        }
    });

    // Task: Trade recorder — handles side effects after PaperTrader has already
    // persisted the trade to the DB. Responsibilities:
    //   - Upsert daily_summary on close
    //   - Fire-and-forget Vegapunk ingestion on close
    // Note: trade INSERT/UPDATE and balance changes are owned by PaperTrader.
    let recorder_pool = pool.clone();
    let recorder_handle = tokio::spawn(async move {
        while let Some(trade_event) = trade_rx.recv().await {
            match trade_event.action {
                TradeAction::Opened => {
                    // Nothing to record: PaperTrader already inserted the trade.
                }
                TradeAction::Closed { .. } => {
                    let t = &trade_event.trade;
                    if let (Some(_exit_price), Some(exit_at), Some(pnl_amount), Some(exit_reason)) =
                        (t.exit_price, t.exit_at, t.pnl_amount, t.exit_reason)
                    {
                        // Upsert daily summary
                        let date = exit_at.date_naive();
                        let mode_str = t.mode.as_str();
                        let win = if pnl_amount > Decimal::ZERO { 1 } else { 0 };
                        if let Err(e) = auto_trader_db::summary::upsert_daily_summary(
                            &recorder_pool, date, &t.strategy_name, &t.pair.0,
                            mode_str, t.exchange.as_str(), t.paper_account_id,
                            1, win, pnl_amount,
                        ).await {
                            tracing::error!("upsert daily summary error: {e}");
                        }
                        // Fire-and-forget Vegapunk ingestion (don't block DB recording)
                        if let Some(vp) = vegapunk_client_recorder.clone() {
                            let t = t.clone();
                            tokio::spawn(async move {
                                let mut vp = vp.lock().await;
                                let direction_str = match t.direction {
                                    Direction::Long => "ロング",
                                    Direction::Short => "ショート",
                                };
                                let holding = exit_at.signed_duration_since(t.entry_at);
                                let pnl_display = t.pnl_pips
                                    .map(|p| format!("{p} pips"))
                                    .unwrap_or_else(|| format!("{pnl_amount} JPY"));
                                let text = format!(
                                    "[{}] {} {} 決済。trade_id: {}。{:?}。PnL: {}。保有時間: {}秒。戦略: {}",
                                    t.exchange, t.pair, direction_str,
                                    t.id, exit_reason,
                                    pnl_display,
                                    holding.num_seconds(),
                                    t.strategy_name,
                                );
                                let channel = format!("{}-trades", t.pair.0.to_lowercase());
                                let timestamp = exit_at.to_rfc3339();
                                if let Err(e) = vp.ingest_raw(&text, "trade_result", &channel, &timestamp).await {
                                    tracing::warn!("vegapunk ingest failed for trade close: {e}");
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
            if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                &daily_pool, d,
            ).await {
                tracing::error!("daily batch backfill failed for {d}: {e}");
            }
        }

        let mut last_date = today;
        loop {
            interval.tick().await;
            let now_date = chrono::Utc::now().date_naive();
            if now_date != last_date {
                tracing::info!("running daily batch for {last_date}");
                if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                    &daily_pool, last_date,
                ).await {
                    tracing::error!("daily batch failed: {e}");
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
                let accounts = match auto_trader_db::paper_accounts::list_paper_accounts(
                    &overnight_pool,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("overnight fee: failed to list paper accounts: {e}");
                        last_date = today;
                        continue;
                    }
                };
                for pac in accounts {
                    let exchange = exchange_from_str(&pac.exchange);
                    if exchange != Exchange::BitflyerCfd {
                        continue;
                    }
                    let trader = PaperTrader::new(overnight_pool.clone(), exchange, pac.id);
                    match trader.apply_overnight_fees(fee_rate).await {
                        Ok(fees) if fees > Decimal::ZERO => {
                            tracing::info!(
                                "overnight fee applied: {} = {} JPY",
                                pac.name, fees
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::error!("overnight fee failed for {}: {e}", pac.name)
                        }
                    }
                }
                last_date = today;
            }
        }
    });

    // REST API server
    let api_state = api::AppState { pool: pool.clone() };
    let api_handle = tokio::spawn(async move {
        let app = api::router(api_state);
        // Bind to 0.0.0.0 only when API_TOKEN is set (authenticated mode).
        // Otherwise bind to 127.0.0.1 to prevent unauthenticated external access.
        let bind_addr = if std::env::var("API_TOKEN").is_ok() {
            "0.0.0.0:3001"
        } else {
            "127.0.0.1:3001"
        };
        let listener = tokio::net::TcpListener::bind(bind_addr).await
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
    if let Some(h) = fx_monitor_handle {
        h.abort();
    }
    if let Some(h) = bitflyer_handle {
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
        let _ = executor_handle.await;
        let _ = recorder_handle.await;
    }).await;

    tracing::info!("shutdown complete");
    Ok(())
}
