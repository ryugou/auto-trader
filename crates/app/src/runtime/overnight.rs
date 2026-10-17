use std::sync::Arc;

use auto_trader_core::config::GmoFxSwapConfig;
use auto_trader_core::types::{Exchange, Trade};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_notify::Notifier;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

const BITFLYER_OVERNIGHT_FEE_RATE: Decimal = rust_decimal_macros::dec!(0.0004);

pub(crate) async fn run(pool: sqlx::PgPool, swap_config: GmoFxSwapConfig, notifier: Arc<Notifier>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    let mut last_date = Utc::now().date_naive();
    loop {
        interval.tick().await;
        let today = Utc::now().date_naive();
        if today != last_date && process_date_change(&pool, &swap_config, &notifier, today).await {
            last_date = today;
        }
    }
}

async fn process_date_change(
    pool: &sqlx::PgPool,
    swap_config: &GmoFxSwapConfig,
    notifier: &Arc<Notifier>,
    today: NaiveDate,
) -> bool {
    let accounts =
        match auto_trader_db::trading_accounts::list_active_or_with_open_trades(pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to list trading accounts for overnight fee, retrying next tick"
                );
                return false;
            }
        };

    send_swap_freshness_alert(swap_config, today, &accounts, notifier);

    let event_at = today.and_time(chrono::NaiveTime::MIN).and_utc();
    for pac in &accounts {
        apply_fees_to_account(pool, swap_config, event_at, pac).await;
    }
    true
}

fn send_swap_freshness_alert(
    swap_config: &GmoFxSwapConfig,
    today: NaiveDate,
    accounts: &[TradingAccount],
    notifier: &Arc<Notifier>,
) {
    let has_gmo_paper = accounts
        .iter()
        .any(|a| a.exchange == "gmo_fx" && a.account_type == "paper");
    let Some(body) =
        auto_trader::swap_freshness::swap_freshness_alert(swap_config, today, has_gmo_paper)
    else {
        return;
    };
    let ev = auto_trader_notify::NotifyEvent::SystemAlert(auto_trader_notify::SystemAlertEvent {
        title: "swap rates freshness".to_string(),
        account_name: "(config)".to_string(),
        exchange: Exchange::GmoFx,
        body,
    });
    let notifier = notifier.clone();
    tokio::spawn(async move {
        if let Err(e) = notifier.send(ev).await {
            tracing::warn!(error = %e, "failed to send swap freshness alert");
        }
    });
}

fn resolve_overnight_fee_exchange(pac: &TradingAccount) -> Option<Exchange> {
    if pac.account_type != "paper" {
        return None;
    }
    let exchange = super::resolve_account_exchange(pac)?;
    if !overnight_fee_applies_to_exchange(exchange) {
        return None;
    }
    Some(exchange)
}

fn overnight_fee_applies_to_exchange(exchange: Exchange) -> bool {
    match exchange {
        Exchange::BitflyerCfd | Exchange::GmoFx => true,
        Exchange::Oanda => false,
    }
}

async fn apply_fees_to_account(
    pool: &sqlx::PgPool,
    swap_config: &GmoFxSwapConfig,
    event_at: DateTime<Utc>,
    pac: &TradingAccount,
) {
    let Some(exchange) = resolve_overnight_fee_exchange(pac) else {
        return;
    };
    let open_trades = match auto_trader_db::trades::get_open_trades_by_account(pool, pac.id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                account_name = %pac.name,
                error = %e,
                "failed to list open trades for overnight fee/swap"
            );
            return;
        }
    };
    let mut applied_count: usize = 0;
    for trade in &open_trades {
        if apply_fee_to_trade(pool, exchange, swap_config, event_at, pac.id, trade).await {
            applied_count += 1;
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

async fn apply_fee_to_trade(
    pool: &sqlx::PgPool,
    exchange: Exchange,
    swap_config: &GmoFxSwapConfig,
    event_at: DateTime<Utc>,
    account_id: Uuid,
    trade: &Trade,
) -> bool {
    let Some(fee) = compute_fee(exchange, trade, swap_config) else {
        return false;
    };
    if fee.is_zero() {
        return false;
    }
    match apply_fee(pool, exchange, account_id, trade.id, fee, event_at).await {
        Ok(Some(_new_balance)) => true,
        Ok(None) => {
            tracing::debug!(
                trade_id = %trade.id,
                "overnight fee/swap apply skipped, trade closed mid-tick"
            );
            false
        }
        Err(e) => {
            tracing::error!(
                trade_id = %trade.id,
                error = %e,
                "failed to apply overnight fee or swap"
            );
            false
        }
    }
}

fn compute_fee(
    exchange: Exchange,
    trade: &Trade,
    swap_config: &GmoFxSwapConfig,
) -> Option<Decimal> {
    match exchange {
        Exchange::BitflyerCfd => Some(
            (trade.entry_price * trade.quantity * BITFLYER_OVERNIGHT_FEE_RATE)
                .round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero),
        ),
        Exchange::GmoFx => {
            let Some(rate) = swap_config.rates.get(trade.pair.0.as_str()) else {
                tracing::debug!(
                    pair = %trade.pair,
                    trade_id = %trade.id,
                    "no swap rate configured for pair, skipping"
                );
                return None;
            };
            Some(auto_trader_core::swap::compute_daily_swap(
                *rate,
                trade.direction,
                trade.quantity,
            ))
        }
        Exchange::Oanda => unreachable!("filtered above"),
    }
}

async fn apply_fee(
    pool: &sqlx::PgPool,
    exchange: Exchange,
    account_id: Uuid,
    trade_id: Uuid,
    fee: Decimal,
    event_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    let mut tx = pool.begin().await?;
    let applied = match exchange {
        Exchange::BitflyerCfd => {
            auto_trader_db::trades::apply_overnight_fee(&mut tx, account_id, trade_id, fee).await?
        }
        Exchange::GmoFx => {
            auto_trader_db::trades::apply_swap_fee(&mut tx, account_id, trade_id, fee, event_at)
                .await?
        }
        Exchange::Oanda => unreachable!("filtered above"),
    };
    tx.commit().await?;
    Ok(applied)
}

#[cfg(test)]
mod tests;
