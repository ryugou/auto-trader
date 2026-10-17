use std::collections::HashMap;

use auto_trader::closer::{CloseContext, close_trade};
use auto_trader::liquidation::{LiquidationContext, detect_liquidation_targets};
use auto_trader::margin_alert::detect_margin_alerts;
use auto_trader::startup::effective_dry_run;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Direction, ExitReason};
use auto_trader_db::trades::OpenTradeWithAccount;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use tokio::sync::mpsc;

use super::resolve_trade_account;

type MarginAlertCooldowns = HashMap<(uuid::Uuid, &'static str), std::time::Instant>;

pub(crate) async fn run(
    close_ctx: CloseContext,
    live_forces_dry_run: bool,
    mut price_rx: mpsc::Receiver<PriceEvent>,
) {
    let liq_ctx = LiquidationContext {
        pool: close_ctx.pool.clone(),
        price_store: close_ctx.price_store.clone(),
        exchange_liquidation_levels: close_ctx.liquidation_levels.clone(),
        live_forces_dry_run,
    };
    let mut margin_alert_last_sent_persisted_across_ticks: MarginAlertCooldowns = HashMap::new();
    while let Some(event) = price_rx.recv().await {
        process_price_event(
            &close_ctx,
            &liq_ctx,
            live_forces_dry_run,
            &mut margin_alert_last_sent_persisted_across_ticks,
            &event,
        )
        .await;
    }
    tracing::info!("price channel closed, stopping crypto position monitor");
}

async fn process_price_event(
    close_ctx: &CloseContext,
    liq_ctx: &LiquidationContext,
    live_forces_dry_run: bool,
    margin_alert_last_sent_persisted_across_ticks: &mut MarginAlertCooldowns,
    event: &PriceEvent,
) {
    let current_price = event.candle.close;
    let open_trades =
        match auto_trader_db::trades::list_open_with_account_name(&close_ctx.pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "failed to list open trades for crypto monitor");
                return;
            }
        };

    close_liquidation_targets(close_ctx, liq_ctx, &open_trades, event, current_price).await;
    send_margin_alerts(
        close_ctx,
        liq_ctx,
        &open_trades,
        event,
        margin_alert_last_sent_persisted_across_ticks,
    )
    .await;

    for owned in open_trades {
        close_trade_if_exit_due(close_ctx, live_forces_dry_run, owned, event, current_price).await;
    }
}

async fn close_liquidation_targets(
    close_ctx: &CloseContext,
    liq_ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
    current_price: Decimal,
) {
    let liq_targets = detect_liquidation_targets(liq_ctx, open_trades, event).await;
    for (account_id, trade_ids) in liq_targets {
        for trade_id in trade_ids {
            let Some(owned) = open_trades.iter().find(|t| t.trade.id == trade_id) else {
                continue;
            };
            let account_name = owned
                .account_name
                .clone()
                .unwrap_or_else(|| account_id.to_string());
            let account_type = owned
                .account_type
                .clone()
                .unwrap_or_else(|| "paper".to_string());
            let close_as_dry_run_for_paper_only_liquidation = true;
            close_trade(
                close_ctx,
                &owned.trade,
                account_name,
                account_type,
                close_as_dry_run_for_paper_only_liquidation,
                ExitReason::Liquidation,
                current_price,
            )
            .await;
        }
    }
}

async fn send_margin_alerts(
    close_ctx: &CloseContext,
    liq_ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
    margin_alert_last_sent_persisted_across_ticks: &mut MarginAlertCooldowns,
) {
    let margin_alerts = detect_margin_alerts(liq_ctx, open_trades, event).await;
    for alert in margin_alerts {
        let key = (alert.account_id, alert.level.as_str());
        let now = std::time::Instant::now();
        let should_send = margin_alert_last_sent_persisted_across_ticks
            .get(&key)
            .is_none_or(|last| now.duration_since(*last) > std::time::Duration::from_secs(1800));
        if !should_send {
            continue;
        }
        margin_alert_last_sent_persisted_across_ticks.insert(key, now);
        let ev =
            auto_trader_notify::NotifyEvent::SystemAlert(auto_trader_notify::SystemAlertEvent {
                title: format!("margin {}", alert.level.as_str()),
                account_name: alert.account_name.clone(),
                exchange: event.exchange,
                body: format!(
                    "maintenance ratio {} approaching liquidation level {} \
                     (live account — exchange will liquidate below threshold)",
                    alert.ratio.round_dp(4),
                    alert.threshold
                ),
            });
        let notifier = close_ctx.notifier.clone();
        tokio::spawn(async move {
            if let Err(e) = notifier.send(ev).await {
                tracing::warn!(error = %e, "failed to send margin alert notification");
            }
        });
    }
}

async fn close_trade_if_exit_due(
    close_ctx: &CloseContext,
    live_forces_dry_run: bool,
    owned: OpenTradeWithAccount,
    event: &PriceEvent,
    current_price: Decimal,
) {
    let trade = &owned.trade;
    if trade.exchange != event.exchange || trade.pair != event.pair {
        return;
    }
    let Some(account) = resolve_trade_account(&owned) else {
        return;
    };
    let dry_run = effective_dry_run(&account.account_type, live_forces_dry_run);
    let now = Utc::now();
    let Some(reason) = detect_exit_reason(
        trade.direction,
        current_price,
        trade.stop_loss,
        trade.take_profit,
        trade.max_hold_until,
        now,
    ) else {
        return;
    };
    close_trade(
        close_ctx,
        trade,
        account.account_name,
        account.account_type,
        dry_run,
        reason,
        current_price,
    )
    .await;
}

fn detect_exit_reason(
    direction: Direction,
    current_price: Decimal,
    stop_loss: Decimal,
    take_profit: Option<Decimal>,
    max_hold_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<ExitReason> {
    let time_limit_hit = max_hold_until.is_some_and(|deadline| now >= deadline);
    let hit = match direction {
        Direction::Long => {
            if current_price <= stop_loss {
                Some(ExitReason::SlHit)
            } else if take_profit.is_some_and(|tp| current_price >= tp) {
                Some(ExitReason::TpHit)
            } else {
                None
            }
        }
        Direction::Short => {
            if current_price >= stop_loss {
                Some(ExitReason::SlHit)
            } else if take_profit.is_some_and(|tp| current_price <= tp) {
                Some(ExitReason::TpHit)
            } else {
                None
            }
        }
    };
    hit.or(time_limit_hit.then_some(ExitReason::StrategyTimeLimit))
}

#[cfg(test)]
mod tests;
