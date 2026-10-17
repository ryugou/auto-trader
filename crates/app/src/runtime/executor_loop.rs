use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_trader::price_store;
use auto_trader_core::event::{SignalEvent, TradeAction, TradeEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::types::{Exchange, Signal, Trade};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::risk_gate::{GateDecision, RejectReason, eval_price_freshness};
use auto_trader_executor::trader::Trader as UnifiedTrader;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_notify::Notifier;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

pub(crate) struct ExecutorLoopContext {
    pub(crate) pool: sqlx::PgPool,
    pub(crate) trade_tx: mpsc::Sender<TradeEvent>,
    pub(crate) exchange_apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub(crate) price_store: Arc<price_store::PriceStore>,
    pub(crate) notifier: Arc<Notifier>,
    pub(crate) exchange_pairs: Arc<HashMap<Exchange, HashSet<String>>>,
    pub(crate) position_sizer: Arc<PositionSizer>,
    pub(crate) exchange_liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub(crate) live_forces_dry_run: bool,
    pub(crate) live_enabled: bool,
    pub(crate) price_freshness_secs: u64,
    pub(crate) daily_loss_limit_pct: Decimal,
    pub(crate) halt_hours: u64,
    pub(crate) knowledge_store: Option<Arc<dyn KnowledgeStore>>,
}

pub(crate) async fn run(ctx: ExecutorLoopContext, mut signal_rx: mpsc::Receiver<SignalEvent>) {
    while let Some(signal_event) = signal_rx.recv().await {
        process_signal(&ctx, &signal_event).await;
    }
}

async fn process_signal(ctx: &ExecutorLoopContext, signal_event: &SignalEvent) {
    let signal = &signal_event.signal;

    let active_accounts_for_new_dispatch =
        match auto_trader_db::trading_accounts::list_active(&ctx.pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to list active trading accounts, skipping signal dispatch"
                );
                return;
            }
        };

    let mut matched_strategy = false;
    let mut dispatched = false;
    for pac in &active_accounts_for_new_dispatch {
        if pac.strategy != signal.strategy_name {
            continue;
        }
        matched_strategy = true;
        if dispatch_to_account(ctx, pac, signal_event).await {
            dispatched = true;
        }
    }
    if !matched_strategy {
        tracing::warn!(
            strategy_name = %signal.strategy_name,
            "invalid signal: no matching trading account"
        );
    } else if !dispatched {
        tracing::debug!(
            strategy_name = %signal.strategy_name,
            "signal matched strategy on account(s) but no account passed pre-dispatch gates"
        );
    }
}

async fn dispatch_to_account(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal_event: &SignalEvent,
) -> bool {
    let signal = &signal_event.signal;
    let Some(exchange) = check_static_account_gates(ctx, pac, signal) else {
        return false;
    };

    let now = Utc::now();
    if !check_kill_switch(ctx, pac, signal, exchange, now).await {
        return false;
    }

    let dry_run =
        auto_trader::startup::effective_dry_run(&pac.account_type, ctx.live_forces_dry_run);
    let Some(api) = resolve_exchange_api(ctx, pac, exchange, dry_run) else {
        return false;
    };
    if !check_price_freshness(ctx, pac, exchange, signal).await {
        return false;
    }
    let Some(trader) = build_trader(ctx, pac, exchange, api, dry_run) else {
        return false;
    };

    match crate::reconfirm_account_active_before_marking_dispatched(&ctx.pool, pac).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                account_id = %pac.id,
                account_name = %pac.name,
                strategy_name = %signal.strategy_name,
                pair = %signal.pair,
                "invalid account: not active"
            );
            return false;
        }
        Err(e) => {
            tracing::error!(
                account_id = %pac.id,
                account_name = %pac.name,
                error = %e,
                "failed to re-check account state"
            );
            return false;
        }
    }

    execute_and_record(ctx, pac, signal_event, trader).await;
    true
}

fn check_static_account_gates(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal: &Signal,
) -> Option<Exchange> {
    let exchange = match crate::exchange_from_str(&pac.exchange) {
        Some(e) => e,
        None => {
            tracing::warn!(
                account_name = %pac.name,
                account_id = %pac.id,
                exchange = %pac.exchange,
                "unsupported exchange"
            );
            return None;
        }
    };
    if !auto_trader::startup::fx_new_dispatch_allowed(&signal.strategy_name, exchange) {
        tracing::warn!(
            account_name = %pac.name,
            account_id = %pac.id,
            strategy_name = %signal.strategy_name,
            expected_exchange = %auto_trader_strategy::fx_new::FX_NEW_EXCHANGE,
            account_exchange = %exchange,
            "invalid exchange: mismatch with fx_new"
        );
        return None;
    }
    if !ctx
        .exchange_pairs
        .get(&exchange)
        .is_some_and(|pairs| pairs.contains(&signal.pair.0))
    {
        tracing::debug!(
            pair = %signal.pair.0,
            exchange = %exchange,
            account_name = %pac.name,
            "unsupported pair"
        );
        return None;
    }
    if crate::live_order_blocked_when_live_disabled_leaving_close_paths_unaffected(
        &pac.account_type,
        ctx.live_enabled,
    ) {
        tracing::warn!(
            account_name = %pac.name,
            account_id = %pac.id,
            "live trading disabled via [live].enabled=false config, skipping signal for live \
             account"
        );
        return None;
    }
    Some(exchange)
}

async fn check_kill_switch(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal: &Signal,
    exchange: Exchange,
    now: DateTime<Utc>,
) -> bool {
    match auto_trader_db::trading_accounts::get_halt(&ctx.pool, pac.id).await {
        Ok(Some((until, reason))) if until > now => {
            tracing::warn!(
                account_name = %pac.name,
                account_id = %pac.id,
                halted_until = %until,
                halt_reason = %reason.as_deref().unwrap_or("no reason"),
                "account halted by kill switch, skipping entry"
            );
            return false;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(
                account_name = %pac.name,
                account_id = %pac.id,
                error = %e,
                "failed to check kill switch halt status, skipping account (fail-closed)"
            );
            return false;
        }
    }

    let day_start = auto_trader_executor::risk_gate::jst_day_start(now);
    let day_net =
        match auto_trader_db::trades::realized_net_since(&ctx.pool, pac.id, day_start).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    account_name = %pac.name,
                    account_id = %pac.id,
                    error = %e,
                    "failed to compute realized pnl for kill switch, skipping account (fail-closed)"
                );
                return false;
            }
        };
    let day_start_balance =
        crate::compute_day_start_balance_net_of_todays_realized_pnl(pac.current_balance, day_net);
    if let GateDecision::Reject(reason) = auto_trader_executor::risk_gate::eval_daily_loss(
        day_net,
        day_start_balance,
        ctx.daily_loss_limit_pct,
    ) {
        let until = now + chrono::Duration::hours(ctx.halt_hours as i64);
        let halt_reason = format!(
            "daily loss limit: day_net={day_net} start_balance={day_start_balance} limit_pct={}",
            ctx.daily_loss_limit_pct
        );
        apply_kill_switch_halt(ctx, pac, signal, exchange, until, halt_reason, reason).await;
        return false;
    }
    true
}

async fn apply_kill_switch_halt(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal: &Signal,
    exchange: Exchange,
    until: DateTime<Utc>,
    halt_reason: String,
    gate_reason: RejectReason,
) {
    tracing::warn!(
        account_name = %pac.name,
        account_id = %pac.id,
        halted_until = %until,
        halt_reason = %halt_reason,
        gate_reason = ?gate_reason,
        "kill switch halting account due to daily loss limit"
    );
    if let Err(e) =
        auto_trader_db::trading_accounts::set_halt(&ctx.pool, pac.id, until, &halt_reason).await
    {
        tracing::error!(
            account_name = %pac.name,
            account_id = %pac.id,
            error = %e,
            "failed to persist kill switch halt"
        );
    }
    let notifier = ctx.notifier.clone();
    let account_name = pac.name.clone();
    let strategy_name = signal.strategy_name.clone();
    let pair = signal.pair.clone();
    tokio::spawn(async move {
        let ev =
            auto_trader_notify::NotifyEvent::OrderFailed(auto_trader_notify::OrderFailedEvent {
                account_name,
                exchange,
                strategy_name,
                pair,
                reason: format!("Kill Switch 発火 (新規エントリー停止): {halt_reason}"),
            });
        if let Err(e) = notifier.send(ev).await {
            tracing::warn!(error = %e, "failed to send kill switch halt notification");
        }
    });
}

fn resolve_exchange_api(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    exchange: Exchange,
    dry_run: bool,
) -> Option<Arc<dyn ExchangeApi>> {
    match ctx.exchange_apis.get(&exchange) {
        Some(a) => Some(a.clone()),
        None => {
            if !dry_run {
                tracing::warn!(
                    exchange = %exchange,
                    account_name = %pac.name,
                    account_id = %pac.id,
                    "unsupported exchange"
                );
                return None;
            }
            Some(Arc::new(
                auto_trader_market::null_exchange_api::NullExchangeApi,
            ))
        }
    }
}

async fn check_price_freshness(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    exchange: Exchange,
    signal: &Signal,
) -> bool {
    let feed_key = price_store::FeedKey::new(exchange, signal.pair.clone());
    let last_tick_age = ctx
        .price_store
        .last_tick_age_for(&feed_key)
        .await
        .unwrap_or(u64::MAX);
    match eval_price_freshness(ctx.price_freshness_secs, last_tick_age) {
        GateDecision::Pass => true,
        GateDecision::Reject(reason) => {
            tracing::warn!(
                account_name = %pac.name,
                gate_reason = ?reason,
                "invalid signal: rejected by freshness gate"
            );
            false
        }
    }
}

fn build_trader(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    exchange: Exchange,
    api: Arc<dyn ExchangeApi>,
    dry_run: bool,
) -> Option<UnifiedTrader> {
    let liquidation_margin_level = auto_trader::startup::liquidation_level_or_log(
        &ctx.exchange_liquidation_levels,
        exchange,
        || format!("signal for account {} (id={})", pac.name, pac.id),
    )?;
    Some(UnifiedTrader::new(
        ctx.pool.clone(),
        exchange,
        pac.id,
        pac.name.clone(),
        api,
        ctx.price_store.clone(),
        ctx.notifier.clone(),
        ctx.position_sizer.clone(),
        liquidation_margin_level,
        dry_run,
    ))
}

async fn execute_and_record(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal_event: &SignalEvent,
    trader: UnifiedTrader,
) {
    let signal = &signal_event.signal;

    let positions = trader.open_positions().await.unwrap_or_default();
    let has_position = positions
        .iter()
        .any(|p| p.trade.strategy_name == signal.strategy_name && p.trade.pair == signal.pair);
    if has_position {
        tracing::debug!(
            strategy_name = %signal.strategy_name,
            pair = %signal.pair,
            account_name = %pac.name,
            "signal skipped, account already has open position"
        );
        return;
    }

    match trader.execute(signal).await {
        Ok(trade) => {
            tokio::spawn(record_trade_open_side_effects(
                ctx.pool.clone(),
                ctx.knowledge_store.clone(),
                trade.clone(),
                signal_event.indicators.clone(),
                signal.allocation_pct,
            ));
            if let Err(e) = ctx
                .trade_tx
                .send(TradeEvent {
                    trade,
                    action: TradeAction::Opened,
                    account_type: Some(pac.account_type.clone()),
                })
                .await
            {
                tracing::error!(error = %e, "failed to send trade event to trade channel");
            }
        }
        Err(e) => {
            tracing::error!(account_name = %pac.name, error = %e, "failed to execute signal");
        }
    }
}

async fn record_trade_open_side_effects(
    pool: sqlx::PgPool,
    knowledge_store: Option<Arc<dyn KnowledgeStore>>,
    trade: Trade,
    indicators: HashMap<String, Decimal>,
    allocation_pct: Decimal,
) {
    let mut ind_map = indicators
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::json!(v.to_string())))
        .collect::<serde_json::Map<_, _>>();
    ind_map.insert(
        "regime".to_string(),
        serde_json::Value::String(crate::regime::classify(&indicators).as_str().to_string()),
    );
    let ind_json = serde_json::Value::Object(ind_map);
    if let Err(e) = sqlx::query("UPDATE trades SET entry_indicators = $1 WHERE id = $2")
        .bind(&ind_json)
        .bind(trade.id)
        .execute(&pool)
        .await
    {
        tracing::warn!(error = %e, "failed to save entry_indicators");
    }

    if let Some(store) = knowledge_store
        && let Err(e) = store
            .record_trade_open(&trade, &indicators, Some(allocation_pct))
            .await
    {
        tracing::warn!(
            error = %e,
            "failed to record trade open in knowledge store"
        );
    }
}
