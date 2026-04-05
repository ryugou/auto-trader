mod position_monitor;

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
use uuid::Uuid;

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

    // Paper traders: positions are in-memory only (process lifetime = session).
    // On restart, DB may have status='open' trades from previous session.
    // These are NOT restored — the app starts fresh. If position persistence is
    // needed, load open trades from DB on startup and reconstruct PaperTrader state.
    // For now, stale DB records can be cleaned up manually or ignored.
    let paper_trader = Arc::new(PaperTrader::new(
        Exchange::Oanda,
        Decimal::from(100_000),
        Decimal::from(25),
        None,
    ));

    // Paper accounts (crypto) — upsert to DB for FK integrity
    // Each account is bound to exactly one strategy.
    let paper_accounts: Vec<(String, String, Arc<PaperTrader>)> = {
        let mut accounts = Vec::new();
        for pac in &config.paper_accounts {
            let id = Uuid::new_v4();
            let db_id = auto_trader_db::paper_accounts::upsert_paper_account(
                &pool, id, &pac.name, &pac.exchange,
                pac.initial_balance, pac.leverage, &pac.currency,
            ).await?;
            let exchange = match pac.exchange.as_str() {
                "bitflyer_cfd" => Exchange::BitflyerCfd,
                _ => Exchange::Oanda,
            };
            let trader = Arc::new(PaperTrader::new(
                exchange,
                pac.initial_balance,
                pac.leverage,
                Some(db_id),
            ));
            tracing::info!(
                "paper account: {} (id={}, strategy={}, balance={}, leverage={})",
                pac.name, db_id, pac.strategy, pac.initial_balance, pac.leverage
            );
            accounts.push((pac.name.clone(), pac.strategy.clone(), trader));
        }
        accounts
    };

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
    let fx_monitor_handle = if let Some(monitor) = fx_monitor {
        Some(tokio::spawn(async move {
            if let Err(e) = monitor.run().await {
                tracing::error!("FX monitor error: {e}");
            }
        }))
    } else {
        None
    };

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

    // Task: Position monitor — FX (price -> SL/TP check -> close)
    let pos_monitor_executor = paper_trader.clone();
    let pos_monitor_trade_tx = trade_tx.clone();
    let pos_monitor_handle = tokio::spawn(async move {
        position_monitor::run_position_monitor(
            pos_monitor_executor,
            price_monitor_rx,
            pos_monitor_trade_tx,
        ).await;
    });

    // Task: Position monitors — crypto (one per paper_account)
    let mut crypto_price_senders: Vec<(String, mpsc::Sender<PriceEvent>)> = Vec::new();
    for (name, _strategy, trader) in &paper_accounts {
        let (crypto_price_tx, crypto_price_rx) = mpsc::channel::<PriceEvent>(256);
        let monitor_trader = trader.clone();
        let monitor_trade_tx = trade_tx.clone();
        crypto_price_senders.push((name.clone(), crypto_price_tx));
        tokio::spawn(async move {
            position_monitor::run_position_monitor(
                monitor_trader,
                crypto_price_rx,
                monitor_trade_tx,
            ).await;
        });
    }

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
                            if event.exchange == auto_trader_core::types::Exchange::Oanda {
                                if price_monitor_tx.send(event.clone()).await.is_err() {
                                    tracing::warn!("FX position monitor channel closed");
                                }
                            }
                            // Forward to crypto position monitors (crypto events only)
                            if event.exchange == auto_trader_core::types::Exchange::BitflyerCfd {
                                for (name, tx) in &crypto_price_senders {
                                    if tx.send(event.clone()).await.is_err() {
                                        tracing::warn!("crypto position monitor closed for {name}");
                                    }
                                }
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
    // Enforces 1-pair-1-position per strategy at execution time
    // FX signals → paper_trader, crypto signals → all paper_accounts (with PositionSizer)
    let executor = paper_trader.clone();
    let executor_accounts = paper_accounts.clone();
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

            if is_crypto {
                // Crypto: dispatch signal only to the paper_account bound to this strategy.
                for (name, strategy_name, account) in &executor_accounts {
                    if strategy_name != &signal.strategy_name {
                        continue;
                    }
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

                    let balance = account.balance().await;
                    let leverage = account.leverage();
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
                                tracing::error!("trade channel send failed (trade may not be recorded): {e}");
                            }
                        }
                        Err(e) => tracing::error!("execute error for account {}: {e}", name),
                    }
                }
            } else {
                // FX: existing paper_trader
                let positions = executor.open_positions().await.unwrap_or_default();
                let has_position = positions.iter().any(|p| {
                    p.trade.strategy_name == signal.strategy_name
                        && p.trade.pair == signal.pair
                });
                if has_position {
                    tracing::debug!(
                        "skipping signal: {} already has open position for {}",
                        signal.strategy_name, signal.pair
                    );
                    continue;
                }
                match executor.execute(signal).await {
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
                            tracing::error!("trade channel send failed (trade may not be recorded): {e}");
                        }
                    }
                    Err(e) => tracing::error!("execute error: {e}"),
                }
            }
        }
    });

    // Task: Trade recorder (trade -> DB)
    let recorder_pool = pool.clone();
    let recorder_handle = tokio::spawn(async move {
        while let Some(trade_event) = trade_rx.recv().await {
            match trade_event.action {
                TradeAction::Opened => {
                    if let Err(e) = auto_trader_db::trades::insert_trade(
                        &recorder_pool,
                        &trade_event.trade,
                    ).await {
                        tracing::error!("record trade error: {e}");
                    }
                }
                TradeAction::Closed { .. } => {
                    let t = &trade_event.trade;
                    if let (Some(exit_price), Some(exit_at), Some(pnl_amount), Some(exit_reason)) =
                        (t.exit_price, t.exit_at, t.pnl_amount, t.exit_reason)
                    {
                        if let Err(e) = auto_trader_db::trades::update_trade_closed(
                            &recorder_pool, t.id, exit_price, exit_at, t.pnl_pips, pnl_amount, exit_reason, t.fees,
                        ).await {
                            tracing::error!("update trade error: {e}");
                        }
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
    // Note: Positions are in-memory only — on restart they are lost, so startup
    // backfill is not meaningful. If positions are persisted to DB in the future,
    // track last_fee_date per account and backfill missed days on startup.
    let overnight_accounts = paper_accounts.clone();
    let overnight_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let fee_rate = Decimal::new(4, 4); // 0.0004 = 0.04%
        let mut last_date = chrono::Utc::now().date_naive();
        loop {
            interval.tick().await;
            let today = chrono::Utc::now().date_naive();
            if today != last_date {
                for (name, _strategy, trader) in &overnight_accounts {
                    let fees = trader.apply_overnight_fees(fee_rate).await;
                    if fees > Decimal::ZERO {
                        tracing::info!("overnight fee applied: {} = {} JPY", name, fees);
                    }
                }
                last_date = today;
            }
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

    // Wait for downstream tasks to drain (max 5 seconds)
    let drain_timeout = tokio::time::Duration::from_secs(5);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = engine_handle.await;
        let _ = pos_monitor_handle.await;
        let _ = executor_handle.await;
        let _ = recorder_handle.await;
    }).await;

    tracing::info!("shutdown complete");
    Ok(())
}
