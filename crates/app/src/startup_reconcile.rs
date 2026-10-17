use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::types::{Direction, ExitReason};
use auto_trader_db::trades;
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_market::exchange_api::{ExchangeApi, StopOrderStatus};
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::{Notifier, NotifyEvent, SystemAlertEvent};
use rust_decimal::Decimal;
use sqlx::PgPool;

struct AccountScope<'a> {
    pool: &'a PgPool,
    account: &'a TradingAccount,
    api: &'a dyn ExchangeApi,
    price_store: &'a PriceStore,
    notifier: &'a Notifier,
}

/// Reconciles, for each live account of `accounts`, its open and closing trades with the positions on its exchange, and stops at the first account or trade that fails. An account with no such trades is left as is. For each pair and direction, the quantities of the trades are compared with the matching exchange positions in total. Then, per trade:
/// - an open trade with a matching exchange position is left open, and a system alert is sent through `notifier` if its stop order is gone;
/// - an open trade with no matching exchange position is closed as a stop loss hit at the executed price of its stop order, with the stop commission added to its fees, or else closed as reconciled at the mean of the latest bid and ask in `price_store`, or at its entry price if there is none;
/// - a closing trade with a matching exchange position is reset to open;
/// - a closing trade with no matching exchange position is closed as reconciled at the same price as above.
///
/// Each close also adds the trade to the daily summary; a failure of that update is logged as a warning and not returned.
///
/// # Errors
///
/// - Returns an error if the exchange of a live account cannot be parsed.
/// - Returns an error if `apis` has no API for the exchange of a live account.
/// - Returns an error if the open and closing trades of an account cannot be read.
/// - Returns an error if the positions of a pair cannot be read from the exchange in 3 attempts, waiting 2 and then 4 seconds between them.
/// - Returns an error if an exchange position of a pair with a positive size has the opposite or an unknown side, or if matching exchange positions exist and their total size differs from the total quantity of the trades; manual intervention is required in both cases.
/// - Returns an error if a trade has a status other than open or closing.
/// - Returns an error if the database query that resets or closes a trade, adds its fees, or reads the account type for the daily summary fails.
/// - Returns an error if a trade to be closed no longer exists or is already closed, or if a trade whose fees are added no longer exists.
/// - Returns an error if the stored row of a trade to be closed has an exchange, direction, status or exit reason that is not supported.
///
/// # Panics
///
/// Panics if the leverage of a trade that is closed is zero.
pub async fn reconcile_live_accounts_at_startup(
    pool: &PgPool,
    accounts: &[TradingAccount],
    apis: &HashMap<auto_trader_core::types::Exchange, Arc<dyn ExchangeApi>>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
) -> anyhow::Result<()> {
    for account in accounts.iter().filter(|a| a.account_type == "live") {
        let exchange: auto_trader_core::types::Exchange =
            account.exchange.parse().map_err(|e| {
                anyhow::anyhow!(
                    "failed to parse exchange for live account \"{}\": {e}",
                    account.name
                )
            })?;

        let Some(api) = apis.get(&exchange) else {
            anyhow::bail!(
                "failed to find a registered ExchangeApi for exchange \"{}\" used by account \"{}\"",
                account.exchange,
                account.name
            );
        };

        reconcile_one_account(pool, account, api.as_ref(), &price_store, &notifier).await?;
    }
    Ok(())
}

async fn get_positions_with_retry(
    api: &dyn ExchangeApi,
    pair: &str,
    account_name: &str,
) -> anyhow::Result<Vec<auto_trader_market::bitflyer_private::ExchangePosition>> {
    const MAX_ATTEMPTS_TO_ABSORB_API_BLIPS: u32 = 3;
    let mut delay = std::time::Duration::from_secs(2);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS_TO_ABSORB_API_BLIPS {
        match api.get_positions(pair).await {
            Ok(ps) => return Ok(ps),
            Err(e) => {
                tracing::warn!(
                    pair = %pair,
                    attempt = attempt,
                    max_attempts = MAX_ATTEMPTS_TO_ABSORB_API_BLIPS,
                    account_name = %account_name,
                    error = %e,
                    "failed to get_positions"
                );
                last_err = Some(e);
                if attempt < MAX_ATTEMPTS_TO_ABSORB_API_BLIPS {
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("failed to get_positions: retries exhausted")))
}

async fn reconcile_one_account(
    pool: &PgPool,
    account: &TradingAccount,
    api: &dyn ExchangeApi,
    price_store: &PriceStore,
    notifier: &Notifier,
) -> anyhow::Result<()> {
    let scope = AccountScope {
        pool,
        account,
        api,
        price_store,
        notifier,
    };

    let db_trades = trades::list_open_or_closing_by_account(pool, account.id).await?;
    if db_trades.is_empty() {
        tracing::info!(account_name = %account.name, "no open or closing trades");
        return Ok(());
    }

    let pairs = collect_distinct_pairs(&db_trades);
    let exchange_positions = fetch_exchange_positions(api, &account.name, &pairs).await?;
    let db_groups = group_trades_for_aggregate_comparison(&db_trades);

    for ((pair, direction), (db_total_qty, trades)) in &db_groups {
        let pair_positions = exchange_positions.get(pair).cloned().unwrap_or_default();
        let (matching_exchange_size, exchange_has_conflicting) =
            sum_matching_exchange_size(&pair_positions, direction);
        reject_aggregate_mismatch(
            &account.name,
            pair,
            direction,
            *db_total_qty,
            matching_exchange_size,
            exchange_has_conflicting,
        )?;

        let exchange_has_matching = matching_exchange_size > Decimal::ZERO;
        for trade in trades {
            reconcile_trade(&scope, trade, exchange_has_matching).await?;
        }
    }
    Ok(())
}

fn collect_distinct_pairs(db_trades: &[auto_trader_core::types::Trade]) -> Vec<String> {
    let mut pairs_set = std::collections::HashSet::new();
    for t in db_trades {
        pairs_set.insert(t.pair.0.clone());
    }
    pairs_set.into_iter().collect()
}

async fn fetch_exchange_positions(
    api: &dyn ExchangeApi,
    account_name: &str,
    pairs: &[String],
) -> anyhow::Result<HashMap<String, Vec<auto_trader_market::bitflyer_private::ExchangePosition>>> {
    let fetches = pairs.iter().map(|pair| {
        let account_name = account_name.to_string();
        async move {
            let ps = get_positions_with_retry(api, pair, &account_name)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to get_positions: retries exhausted for account \"{account_name}\" pair \"{pair}\": {e}"
                    )
                })?;
            Ok::<_, anyhow::Error>((pair.clone(), ps))
        }
    });
    let results = futures_util::future::try_join_all(fetches).await?;
    Ok(results.into_iter().collect())
}

fn sum_matching_exchange_size(
    pair_positions: &[auto_trader_market::bitflyer_private::ExchangePosition],
    direction: &Direction,
) -> (Decimal, bool) {
    let mut exchange_has_conflicting = false;
    let mut matching_exchange_size = Decimal::ZERO;
    for p in pair_positions {
        if p.size <= Decimal::ZERO {
            continue;
        }
        match matches_direction(&p.side, direction) {
            Some(true) => matching_exchange_size += p.size,
            Some(false) => exchange_has_conflicting = true,
            None => {
                tracing::warn!(
                    side = %p.side,
                    product_code = %p.product_code,
                    "unknown side, treating as conflict"
                );
                exchange_has_conflicting = true;
            }
        }
    }
    (matching_exchange_size, exchange_has_conflicting)
}

fn reject_aggregate_mismatch(
    account_name: &str,
    pair: &str,
    direction: &Direction,
    db_total_qty: Decimal,
    matching_exchange_size: Decimal,
    exchange_has_conflicting: bool,
) -> anyhow::Result<()> {
    if exchange_has_conflicting {
        anyhow::bail!(
            "failed to reconcile: conflicting exchange position for pair \"{pair}\" direction {direction:?} on account \"{}\"; manual intervention required",
            account_name
        );
    }

    if matching_exchange_size > Decimal::ZERO && matching_exchange_size != db_total_qty {
        anyhow::bail!(
            "failed to reconcile: size mismatch for pair \"{pair}\" direction {direction:?} (db_total={db_total_qty}, exchange={matching_exchange_size}) on account \"{}\"; manual intervention required",
            account_name
        );
    }

    Ok(())
}

async fn reconcile_trade(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
    exchange_has_matching: bool,
) -> anyhow::Result<()> {
    match (trade.status.as_str(), exchange_has_matching) {
        ("open", true) => reconcile_open_matching(scope, trade).await,
        ("open", false) => reconcile_open_orphan(scope, trade).await,
        ("closing", true) => reconcile_closing_matching(scope, trade).await,
        ("closing", false) => reconcile_closing_orphan(scope, trade).await,
        (other, _) => anyhow::bail!(
            "unsupported trade_status for trade \"{}\": {other}",
            trade.id
        ),
    }
}

async fn reconcile_open_matching(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    if let Some(stop_id) = &trade.stop_order_id {
        alert_if_protective_stop_is_gone(scope.api, scope.account, trade, stop_id, scope.notifier)
            .await;
    } else {
        tracing::info!(trade_id = %trade.id, "consistent, db open and exchange open");
    }
    Ok(())
}

async fn reconcile_open_orphan(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    if let Some((price, commission)) = stop_executed_fill(scope.api, trade).await {
        close_as_sl_hit(scope.pool, trade, price, commission).await
    } else {
        close_as_open_orphan(scope, trade).await
    }
}

async fn close_as_sl_hit(
    pool: &PgPool,
    trade: &auto_trader_core::types::Trade,
    price: Decimal,
    commission: Decimal,
) -> anyhow::Result<()> {
    tracing::info!(
        trade_id = %trade.id,
        price = %price,
        "closed by exchange stop, recording as sl_hit"
    );
    close_reconciled_with(pool, trade, price, ExitReason::SlHit, commission).await
}

async fn close_as_open_orphan(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    tracing::warn!(
        trade_id = %trade.id,
        "db open but exchange has no matching position, closing with best-effort exit price"
    );
    tracing::warn!(trade_id = %trade.id, reason = "orphan", "force-closed");
    force_close_at_best_effort_price(scope.pool, scope.price_store, trade).await
}

async fn reconcile_closing_matching(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    tracing::warn!(
        trade_id = %trade.id,
        "db closing but exchange position still open, resetting to open for retry by normal close monitor"
    );
    trades::release_close_lock(scope.pool, trade.id).await
}

async fn reconcile_closing_orphan(
    scope: &AccountScope<'_>,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    tracing::warn!(
        trade_id = %trade.id,
        "db closing and exchange shows no position, completing phase3 with best-effort exit price"
    );
    tracing::warn!(trade_id = %trade.id, reason = "phase3", "force-closed");
    force_close_at_best_effort_price(scope.pool, scope.price_store, trade).await
}

async fn force_close_at_best_effort_price(
    pool: &PgPool,
    price_store: &PriceStore,
    trade: &auto_trader_core::types::Trade,
) -> anyhow::Result<()> {
    let exit_price = best_effort_exit_price(price_store, trade).await;
    close_reconciled_with(
        pool,
        trade,
        exit_price,
        ExitReason::Reconciled,
        Decimal::ZERO,
    )
    .await
}

fn group_trades_for_aggregate_comparison(
    db_trades: &[auto_trader_core::types::Trade],
) -> HashMap<(String, Direction), (Decimal, Vec<&auto_trader_core::types::Trade>)> {
    let mut db_groups: HashMap<
        (String, Direction),
        (Decimal, Vec<&auto_trader_core::types::Trade>),
    > = HashMap::new();
    for trade in db_trades {
        let key = (trade.pair.0.clone(), trade.direction);
        let entry = db_groups.entry(key).or_insert((Decimal::ZERO, Vec::new()));
        entry.0 += trade.quantity;
        entry.1.push(trade);
    }
    db_groups
}

async fn alert_if_protective_stop_is_gone(
    api: &dyn ExchangeApi,
    account: &TradingAccount,
    trade: &auto_trader_core::types::Trade,
    stop_id: &str,
    notifier: &Notifier,
) {
    match api.stop_order_status(&trade.pair.0, stop_id).await {
        Ok(StopOrderStatus::Gone) => {
            notify_stop_order_lost(notifier, account, trade, stop_id).await;
        }
        Ok(_) => {
            tracing::info!(trade_id = %trade.id, "consistent, db open and exchange open with stop present");
        }
        Err(e) => {
            tracing::warn!(trade_id = %trade.id, error = %e, "failed to check stop_order_status");
        }
    }
}

async fn notify_stop_order_lost(
    notifier: &Notifier,
    account: &TradingAccount,
    trade: &auto_trader_core::types::Trade,
    stop_id: &str,
) {
    tracing::error!(
        trade_id = %trade.id,
        stop_id = %stop_id,
        "protective stop order is gone, position is unprotected"
    );
    let ev = NotifyEvent::SystemAlert(SystemAlertEvent {
        title: "stop order lost".to_string(),
        account_name: account.name.clone(),
        exchange: trade.exchange,
        body: format!(
            "trade {} on {} has an open exchange position but its SL \
             stop order ({stop_id}) is gone (canceled/expired). The \
             position is unprotected — replace the stop or close manually.",
            trade.id, trade.pair
        ),
    });
    if let Err(e) = notifier.send(ev).await {
        tracing::warn!(error = %e, "failed to send stop-order-lost alert");
    }
}

async fn best_effort_exit_price(
    price_store: &PriceStore,
    trade: &auto_trader_core::types::Trade,
) -> Decimal {
    let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
    match price_store.latest_bid_ask(&feed_key).await {
        Some((bid, ask)) => (bid + ask) / Decimal::from(2),
        None => {
            tracing::warn!(
                exchange = %trade.exchange,
                pair = %trade.pair,
                trade_id = %trade.id,
                "no price_store data, using entry_price as exit_price (approximate)"
            );
            trade.entry_price
        }
    }
}

async fn stop_executed_fill(
    api: &dyn ExchangeApi,
    trade: &auto_trader_core::types::Trade,
) -> Option<(Decimal, Decimal)> {
    let stop_id = trade.stop_order_id.as_ref()?;
    match api.stop_order_status(&trade.pair.0, stop_id).await {
        Ok(StopOrderStatus::Executed { price, commission }) => Some((price, commission)),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(trade_id = %trade.id, error = %e, "failed to check stop_order_status");
            None
        }
    }
}

async fn close_reconciled_with(
    pool: &PgPool,
    trade: &auto_trader_core::types::Trade,
    exit_price: Decimal,
    exit_reason: ExitReason,
    extra_fees: Decimal,
) -> anyhow::Result<()> {
    let pnl = match trade.direction {
        Direction::Long => (exit_price - trade.entry_price) * trade.quantity,
        Direction::Short => (trade.entry_price - exit_price) * trade.quantity,
    };
    let pnl = pnl.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::ToZero);

    if extra_fees != Decimal::ZERO {
        trades::add_fees(pool, trade.id, extra_fees).await?;
    }

    trades::close_trade_reconciled(pool, trade.id, exit_price, pnl, exit_reason).await?;

    backfill_daily_summary_for_reconciled_close(pool, trade, pnl).await?;

    Ok(())
}

async fn backfill_daily_summary_for_reconciled_close(
    pool: &PgPool,
    trade: &auto_trader_core::types::Trade,
    pnl: Decimal,
) -> anyhow::Result<()> {
    let exit_date = chrono::Utc::now().date_naive();
    let win = if pnl > Decimal::ZERO { 1 } else { 0 };
    let account_type: String =
        sqlx::query_scalar("SELECT account_type FROM trading_accounts WHERE id = $1")
            .bind(trade.account_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or_else(|| "paper".to_string());

    if let Err(e) = auto_trader_db::summary::upsert_daily_summary(
        pool,
        exit_date,
        &trade.strategy_name,
        &trade.pair.0,
        &account_type,
        trade.exchange.as_str(),
        Some(trade.account_id),
        1,
        win,
        pnl,
    )
    .await
    {
        tracing::warn!(trade_id = %trade.id, error = %e, "failed to update daily_summary");
    }

    Ok(())
}

fn matches_direction(side: &str, direction: &Direction) -> Option<bool> {
    match side.trim().to_ascii_uppercase().as_str() {
        "BUY" => Some(*direction == Direction::Long),
        "SELL" => Some(*direction == Direction::Short),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
