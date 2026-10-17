use auto_trader_core::types::{Exchange, Signal};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_executor::risk_gate::{GateDecision, RejectReason};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use super::ExecutorLoopContext;

pub(super) async fn check_kill_switch(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    signal: &Signal,
    exchange: Exchange,
    now: DateTime<Utc>,
) -> bool {
    if is_halted_or_unreadable(ctx, pac, now).await {
        return false;
    }

    let day_start = auto_trader_executor::risk_gate::jst_day_start(now);
    let Some(day_net) = realized_net_today(ctx, pac, day_start).await else {
        return false;
    };
    let day_start_balance =
        compute_day_start_balance_net_of_todays_realized_pnl(pac.current_balance, day_net);
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

async fn is_halted_or_unreadable(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
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
            true
        }
        Ok(_) => false,
        Err(e) => {
            tracing::error!(
                account_name = %pac.name,
                account_id = %pac.id,
                error = %e,
                "failed to check kill switch halt status, skipping account (fail-closed)"
            );
            true
        }
    }
}

async fn realized_net_today(
    ctx: &ExecutorLoopContext,
    pac: &TradingAccount,
    day_start: DateTime<Utc>,
) -> Option<Decimal> {
    match auto_trader_db::trades::realized_net_since(&ctx.pool, pac.id, day_start).await {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::error!(
                account_name = %pac.name,
                account_id = %pac.id,
                error = %e,
                "failed to compute realized pnl for kill switch, skipping account (fail-closed)"
            );
            None
        }
    }
}

fn compute_day_start_balance_net_of_todays_realized_pnl(
    current_balance: Decimal,
    day_net: Decimal,
) -> Decimal {
    current_balance - day_net
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
