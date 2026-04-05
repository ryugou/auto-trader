mod position_monitor;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, Pair};
use auto_trader_db::pool::create_pool;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_strategy::trend_follow::TrendFollowV1;
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

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

    // OANDA client
    let api_key = std::env::var("OANDA_API_KEY")
        .expect("OANDA_API_KEY must be set");
    let account_id = std::env::var("OANDA_ACCOUNT_ID")
        .unwrap_or_else(|_| config.oanda.account_id.clone());
    let oanda = OandaClient::new(&config.oanda.api_url, &account_id, &api_key)?;

    // Market monitor
    let pairs: Vec<Pair> = config.pairs.active.iter().map(|s| Pair::new(s)).collect();
    let monitor = MarketMonitor::new(oanda, pairs, config.monitor.interval_secs, price_tx.clone())
        .with_db(pool.clone());

    // Vegapunk: single gRPC channel, multiple clients share it
    let vegapunk_base: Option<auto_trader_vegapunk::client::VegapunkClient> =
        match auto_trader_vegapunk::client::VegapunkClient::connect(
            &config.vegapunk.endpoint, &config.vegapunk.schema
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
                let gemini_config = config.gemini.as_ref()
                    .expect("gemini config required for swing_llm");

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
            other => {
                tracing::warn!("unknown strategy: {other}, skipping");
            }
        }
    }

    // Paper trader
    let paper_trader = Arc::new(PaperTrader::new(
        Decimal::from(100_000),
        Decimal::from(25),
    ));

    let vegapunk_client_exec: Option<Arc<Mutex<auto_trader_vegapunk::client::VegapunkClient>>> =
        vegapunk_base.as_ref().map(|base| {
            Arc::new(Mutex::new(
                auto_trader_vegapunk::client::VegapunkClient::clone_from_channel(base, &config.vegapunk.schema),
            ))
        });
    let vegapunk_client_recorder = vegapunk_client_exec.clone();

    // Task: Market monitor
    let monitor_handle = tokio::spawn(async move {
        if let Err(e) = monitor.run().await {
            tracing::error!("monitor error: {e}");
        }
    });

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

    // Task: Strategy engine (price -> signal) + forward to position monitor
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
                            if price_monitor_tx.send(event.clone()).await.is_err() {
                                tracing::warn!("position monitor channel closed, SL/TP monitoring stopped");
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

    // Task: Position monitor (price -> SL/TP check -> close)
    let pos_monitor_executor = paper_trader.clone();
    let pos_monitor_trade_tx = trade_tx.clone();
    let pos_monitor_handle = tokio::spawn(async move {
        position_monitor::run_position_monitor(
            pos_monitor_executor,
            price_monitor_rx,
            pos_monitor_trade_tx,
        ).await;
    });

    // Task: Signal executor (signal -> trade)
    // Enforces 1-pair-1-position per strategy at execution time
    let executor = paper_trader.clone();
    let trade_tx_clone = trade_tx.clone();
    let executor_handle = tokio::spawn(async move {
        while let Some(signal_event) = signal_rx.recv().await {
            let signal = &signal_event.signal;
            // Check 1-pair-1-position constraint per strategy
            let positions = executor.open_positions().await.unwrap_or_default();
            let has_position = positions.iter().any(|p| {
                p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair
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
                    // Fire-and-forget Vegapunk ingestion (don't block trade execution)
                    if let Some(vp) = vegapunk_client_exec.clone() {
                        let trade_clone = trade.clone();
                        tokio::spawn(async move {
                            let mut vp = vp.lock().await;
                            let direction_str = match trade_clone.direction {
                                Direction::Long => "ロング",
                                Direction::Short => "ショート",
                            };
                            let text = format!(
                                "{} {} 判断。trade_id: {}。エントリー価格: {}。SL: {}、TP: {}。戦略: {}",
                                trade_clone.pair, direction_str,
                                trade_clone.id, trade_clone.entry_price, trade_clone.stop_loss, trade_clone.take_profit,
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
                    if let (Some(exit_price), Some(exit_at), Some(pnl_pips), Some(pnl_amount), Some(exit_reason)) =
                        (t.exit_price, t.exit_at, t.pnl_pips, t.pnl_amount, t.exit_reason)
                    {
                        if let Err(e) = auto_trader_db::trades::update_trade_closed(
                            &recorder_pool, t.id, exit_price, exit_at, pnl_pips, pnl_amount, exit_reason,
                        ).await {
                            tracing::error!("update trade error: {e}");
                        }
                        // Upsert daily summary
                        let date = exit_at.date_naive();
                        let mode_str = t.mode.as_str();
                        let win = if pnl_pips > Decimal::ZERO { 1 } else { 0 };
                        if let Err(e) = auto_trader_db::summary::upsert_daily_summary(
                            &recorder_pool, date, &t.strategy_name, &t.pair.0,
                            mode_str, 1, win, pnl_amount,
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
                                let text = format!(
                                    "{} {} 決済。trade_id: {}。{:?}。PnL: {} pips。保有時間: {}秒。戦略: {}",
                                    t.pair, direction_str,
                                    t.id, exit_reason,
                                    pnl_pips,
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

    tracing::info!("auto-trader running. Press Ctrl+C to stop.");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down... draining channels");

    // Drop senders to signal downstream tasks to finish
    drop(price_tx);
    drop(trade_tx); // allow recorder to drain and exit
    monitor_handle.abort();
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
