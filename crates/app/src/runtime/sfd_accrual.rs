use std::sync::Arc;

use auto_trader::price_store;
use auto_trader_core::sfd;
use auto_trader_core::types::{Exchange, Pair, Trade};
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_market::price_store::FeedKey;
use chrono::{DateTime, Duration, DurationRound as _, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

const SFD_MAX_CATCH_UP_HOURS: i64 = 24;
const SFD_STALE_THRESHOLD_SECS: u64 = 300;

pub(crate) async fn run(pool: sqlx::PgPool, price_store: Arc<price_store::PriceStore>) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
    let mut last_hour: Option<DateTime<Utc>> = None;
    loop {
        interval.tick().await;
        last_hour = process_tick(&pool, &price_store, last_hour).await;
    }
}

async fn process_tick(
    pool: &sqlx::PgPool,
    price_store: &price_store::PriceStore,
    last_hour: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    let Some(current_hour) = truncate_to_hour(now) else {
        return last_hour;
    };
    let Some(lh) = last_hour else {
        return Some(current_hour);
    };
    if lh + Duration::hours(1) > current_hour {
        return last_hour;
    }

    let (lh, pending) = clamp_catch_up(lh, current_hour);

    let accounts =
        match auto_trader_db::trading_accounts::list_active_or_with_open_trades(pool).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "failed to list trading accounts for sfd accrual, retrying next tick"
                );
                return last_hour;
            }
        };
    if !has_paper_bitflyer_account(&accounts) {
        return Some(current_hour);
    }

    let open_target_count = match count_open_bitflyer_paper_fx_btc_jpy_trades(pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                "failed to count open fx_btc_jpy trades for sfd accrual, retrying next tick"
            );
            return last_hour;
        }
    };
    if open_target_count == 0 {
        return Some(current_hour);
    }

    let Some((fx_mid, spot_mid)) = fetch_fresh_mid_prices(price_store).await else {
        return last_hour;
    };

    Some(apply_pending_hours(pool, &accounts, pending, lh, fx_mid, spot_mid).await)
}

async fn count_open_bitflyer_paper_fx_btc_jpy_trades(pool: &sqlx::PgPool) -> anyhow::Result<i64> {
    sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM trades t
           JOIN trading_accounts a ON a.id = t.account_id
           WHERE t.pair = 'FX_BTC_JPY' AND t.status = 'open'
             AND a.account_type = 'paper' AND a.exchange = 'bitflyer_cfd'"#,
    )
    .fetch_one(pool)
    .await
    .map_err(anyhow::Error::from)
}

async fn fetch_fresh_mid_prices(
    price_store: &price_store::PriceStore,
) -> Option<(Decimal, Decimal)> {
    let fx_key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("FX_BTC_JPY"));
    let spot_key = FeedKey::new(Exchange::BitflyerCfd, Pair::new("BTC_JPY"));
    let fx_age = price_store.last_tick_age_for(&fx_key).await;
    let spot_age = price_store.last_tick_age_for(&spot_key).await;
    if !both_feeds_fresh(fx_age, spot_age) {
        tracing::warn!(
            fx_age_secs = ?fx_age,
            spot_age_secs = ?spot_age,
            stale_threshold_secs = SFD_STALE_THRESHOLD_SECS,
            "invalid sfd hourly tick: stale"
        );
        return None;
    }

    let fx_ba = price_store.latest_bid_ask(&fx_key).await;
    let spot_ba = price_store.latest_bid_ask(&spot_key).await;
    match (fx_ba, spot_ba) {
        (Some((b1, a1)), Some((b2, a2))) => Some((mid_price(b1, a1), mid_price(b2, a2))),
        _ => {
            tracing::warn!("invalid sfd hourly tick: missing bid/ask");
            None
        }
    }
}

async fn apply_pending_hours(
    pool: &sqlx::PgPool,
    accounts: &[TradingAccount],
    pending: i64,
    mut lh: DateTime<Utc>,
    fx_mid: Decimal,
    spot_mid: Decimal,
) -> DateTime<Utc> {
    for _ in 0..pending {
        for pac in accounts {
            apply_sfd_to_account(pool, pac, lh, fx_mid, spot_mid).await;
        }
        lh += Duration::hours(1);
    }
    lh
}

async fn apply_sfd_to_account(
    pool: &sqlx::PgPool,
    pac: &TradingAccount,
    lh: DateTime<Utc>,
    fx_mid: Decimal,
    spot_mid: Decimal,
) {
    if pac.account_type != "paper" {
        return;
    }
    let Some(exchange) = super::resolve_account_exchange(pac) else {
        return;
    };
    if exchange != Exchange::BitflyerCfd {
        return;
    }
    let open_trades = match auto_trader_db::trades::get_open_trades_by_account(pool, pac.id).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                account_name = %pac.name,
                error = %e,
                "failed to list open trades for sfd hourly"
            );
            return;
        }
    };
    for trade in &open_trades {
        apply_sfd_to_trade(pool, pac.id, trade, lh, fx_mid, spot_mid).await;
    }
}

async fn apply_sfd_to_trade(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    trade: &Trade,
    lh: DateTime<Utc>,
    fx_mid: Decimal,
    spot_mid: Decimal,
) {
    if !trade_is_subject_to_bitflyer_sfd(&trade.pair.0) {
        return;
    }
    let notional = trade.entry_price * trade.quantity;
    let fee = sfd::compute_hourly_sfd(sfd::SfdContext {
        fx_price: fx_mid,
        spot_price: spot_mid,
        position_notional: notional,
        direction: trade.direction,
    });
    if fee.is_zero() {
        return;
    }
    let event_at = lh + Duration::hours(1);
    if trade_was_not_yet_open_at_event_time(trade.entry_at, event_at) {
        return;
    }
    match apply_fee(pool, account_id, trade.id, fee, event_at).await {
        Ok(Some(_)) => {
            tracing::info!(
                trade_id = %trade.id,
                fee = %fee,
                notional = %notional,
                "sfd applied"
            );
        }
        Ok(None) => {
            tracing::debug!(
                trade_id = %trade.id,
                "sfd skipped, trade closed mid-tick"
            );
        }
        Err(e) => {
            tracing::error!(
                trade_id = %trade.id,
                error = %e,
                "failed to apply sfd fee"
            );
        }
    }
}

fn trade_is_subject_to_bitflyer_sfd(pair: &str) -> bool {
    pair == "FX_BTC_JPY"
}

fn trade_was_not_yet_open_at_event_time(entry_at: DateTime<Utc>, event_at: DateTime<Utc>) -> bool {
    entry_at > event_at
}

async fn apply_fee(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    trade_id: Uuid,
    fee: Decimal,
    event_at: DateTime<Utc>,
) -> anyhow::Result<Option<Decimal>> {
    let mut tx = pool.begin().await?;
    let applied =
        auto_trader_db::trades::apply_sfd_fee(&mut tx, account_id, trade_id, fee, event_at).await?;
    tx.commit().await?;
    Ok(applied)
}

fn clamp_catch_up(lh: DateTime<Utc>, current_hour: DateTime<Utc>) -> (DateTime<Utc>, i64) {
    let pending = (current_hour - lh).num_hours();
    if pending <= SFD_MAX_CATCH_UP_HOURS {
        return (lh, pending);
    }
    let dropped = pending - SFD_MAX_CATCH_UP_HOURS;
    tracing::warn!(
        missed_hours = pending,
        dropped_hours = dropped,
        max_catch_up = SFD_MAX_CATCH_UP_HOURS,
        remedy = "audit the balance, a long ws outage likely dropped older accruals",
        "invalid sfd catch-up: too many missed hours"
    );
    (
        current_hour - Duration::hours(SFD_MAX_CATCH_UP_HOURS),
        SFD_MAX_CATCH_UP_HOURS,
    )
}

fn has_paper_bitflyer_account(accounts: &[TradingAccount]) -> bool {
    accounts.iter().any(|pac| {
        pac.account_type == "paper"
            && super::exchange_from_str(&pac.exchange) == Some(Exchange::BitflyerCfd)
    })
}

fn both_feeds_fresh(fx_age: Option<u64>, spot_age: Option<u64>) -> bool {
    matches!(fx_age, Some(a) if a <= SFD_STALE_THRESHOLD_SECS)
        && matches!(spot_age, Some(a) if a <= SFD_STALE_THRESHOLD_SECS)
}

fn mid_price(bid: Decimal, ask: Decimal) -> Decimal {
    (bid + ask) / Decimal::from(2)
}

fn truncate_to_hour(now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match now.duration_trunc(Duration::hours(1)) {
        Ok(truncated) => Some(truncated),
        Err(e) => {
            tracing::error!(
                error = %e,
                now = %now,
                "invalid sfd hourly tick: cannot truncate to hour"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests;
