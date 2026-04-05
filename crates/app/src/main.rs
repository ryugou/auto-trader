mod position_monitor;

use auto_trader_core::config::AppConfig;
use auto_trader_core::event::{PriceEvent, SignalEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::Pair;
use auto_trader_db::pool::create_pool;
use auto_trader_executor::paper::PaperTrader;
use auto_trader_market::monitor::MarketMonitor;
use auto_trader_market::oanda::OandaClient;
use auto_trader_strategy::engine::StrategyEngine;
use auto_trader_strategy::trend_follow::TrendFollowV1;
use rust_decimal::Decimal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

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

                let gemini_api_key = std::env::var("GEMINI_API_KEY")
                    .expect("GEMINI_API_KEY must be set for swing_llm strategy");
                let gemini_config = config.gemini.as_ref()
                    .expect("gemini config required for swing_llm");

                let vp_config = &config.vegapunk;
                let vp_client = auto_trader_vegapunk::client::VegapunkClient::connect(
                    &vp_config.endpoint, &vp_config.schema,
                ).await?;

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

    // Task: Market monitor
    let monitor_handle = tokio::spawn(async move {
        if let Err(e) = monitor.run().await {
            tracing::error!("monitor error: {e}");
        }
    });

    // Task: Strategy engine (price -> signal) + forward to position monitor
    let engine_handle = tokio::spawn(async move {
        while let Some(event) = price_rx.recv().await {
            // Forward to position monitor
            if price_monitor_tx.send(event.clone()).await.is_err() {
                tracing::warn!("position monitor channel closed, SL/TP monitoring stopped");
            }
            engine.on_price(&event).await;
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
                    let _ = trade_tx_clone.send(TradeEvent {
                        trade,
                        action: TradeAction::Opened,
                    }).await;
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
                        let mode_str = serde_json::to_string(&t.mode).unwrap_or_default();
                        let mode_str = mode_str.trim_matches('"');
                        let win = if pnl_pips > Decimal::ZERO { 1 } else { 0 };
                        let _ = auto_trader_db::summary::upsert_daily_summary(
                            &recorder_pool, date, &t.strategy_name, &t.pair.0,
                            mode_str, 1, win, pnl_amount,
                        ).await;
                    }
                }
            }
        }
    });

    // Task: Daily batch (max_drawdown calculation at UTC 0:00)
    let daily_pool = pool.clone();
    let daily_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        let mut last_date = chrono::Utc::now().date_naive();
        loop {
            interval.tick().await;
            let today = chrono::Utc::now().date_naive();
            if today != last_date {
                // Date changed — calculate yesterday's max_drawdown
                tracing::info!("running daily batch for {last_date}");
                if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                    &daily_pool, last_date,
                ).await {
                    tracing::error!("daily batch failed: {e}");
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
    monitor_handle.abort();

    // Wait for downstream tasks to drain (max 5 seconds)
    let drain_timeout = tokio::time::Duration::from_secs(5);
    let _ = tokio::time::timeout(drain_timeout, async {
        let _ = engine_handle.await;
        let _ = pos_monitor_handle.await;
        let _ = executor_handle.await;
        let _ = recorder_handle.await;
        let _ = daily_handle.await;
    }).await;

    tracing::info!("shutdown complete");
    Ok(())
}
