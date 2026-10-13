use auto_trader_core::types::Exchange;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::creation::{
    REACTIVATE_ACCOUNT_OPERATION, build_one_live_per_exchange_conflict_message,
    translate_unique_violation, validate_leverage_for_exchange,
};
use super::model::{ACCOUNT_COLUMNS, AccountRow, TradingAccount};

const ATOMIC_DEACTIVATION_GUARD_WHERE: &str = "$5 IS DISTINCT FROM FALSE OR NOT active OR NOT EXISTS (\
     SELECT 1 FROM trades t WHERE t.account_id = $1 AND t.status IN ('open', 'closing'))";

pub async fn get(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    get_account(pool, id).await
}

pub async fn get_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    let row = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(TradingAccount::from))
}

pub async fn list_all(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

pub async fn list_active(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE active ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

pub async fn list_active_or_with_open_trades(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts ta
         WHERE ta.active
            OR EXISTS (
                SELECT 1 FROM trades t
                WHERE t.account_id = ta.id AND t.status IN ('open', 'closing')
            )
         ORDER BY ta.created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

pub async fn update_balance(pool: &PgPool, id: Uuid, new_balance: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trading_accounts SET current_balance = $2 WHERE id = $1")
        .bind(id)
        .bind(new_balance)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("failed to update balance for account: not found");
    }
    Ok(())
}

pub async fn get_halt(
    pool: &PgPool,
    id: Uuid,
) -> anyhow::Result<Option<(DateTime<Utc>, Option<String>)>> {
    let row: Option<(Option<DateTime<Utc>>, Option<String>)> =
        sqlx::query_as("SELECT halted_until, halt_reason FROM trading_accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(until, reason)| until.map(|u| (u, reason))))
}

pub async fn set_halt(
    pool: &PgPool,
    id: Uuid,
    until: DateTime<Utc>,
    reason: &str,
) -> anyhow::Result<()> {
    sqlx::query("UPDATE trading_accounts SET halted_until = $2, halt_reason = $3 WHERE id = $1")
        .bind(id)
        .bind(until)
        .bind(reason)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTradingAccount {
    pub name: Option<String>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug)]
pub enum UpdateAccountError {
    OpenTrades { account_id: Uuid, count: usize },
    Other(anyhow::Error),
}

impl std::fmt::Display for UpdateAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateAccountError::OpenTrades { count, .. } => write!(
                f,
                "invalid account: {count} open/closing trade(s) exist; close them first"
            ),
            UpdateAccountError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for UpdateAccountError {}

impl From<anyhow::Error> for UpdateAccountError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl From<sqlx::Error> for UpdateAccountError {
    fn from(e: sqlx::Error) -> Self {
        Self::Other(e.into())
    }
}

pub async fn update_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdateTradingAccount,
) -> Result<Option<TradingAccount>, UpdateAccountError> {
    let existing = if req.leverage.is_some() || req.active.is_some() {
        get_account(pool, id).await?
    } else {
        None
    };

    if let Some(new_leverage) = req.leverage
        && let Some(account) = &existing
    {
        let exchange_enum: Exchange = account.exchange.parse()?;
        if let Err(msg) = validate_leverage_for_exchange(exchange_enum, new_leverage) {
            return Err(UpdateAccountError::Other(anyhow::anyhow!(msg)));
        }
    }

    if let Some(new_active) = req.active
        && new_active
        && let Some(account) = &existing
        && account.account_type == "live"
    {
        reject_if_other_live_account_active(pool, &account.exchange, id).await?;
    }

    let sql = format!(
        r#"UPDATE trading_accounts SET
               name = COALESCE($2, name),
               leverage = COALESCE($3, leverage),
               strategy = COALESCE($4, strategy),
               active = COALESCE($5, active)
           WHERE id = $1
             AND ({ATOMIC_DEACTIVATION_GUARD_WHERE})
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(req.leverage)
        .bind(&req.strategy)
        .bind(req.active)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            UpdateAccountError::Other(match &existing {
                Some(account) => {
                    translate_unique_violation(e, REACTIVATE_ACCOUNT_OPERATION, &account.exchange)
                }
                None => e.into(),
            })
        })?;
    if let Some(r) = row {
        return Ok(Some(TradingAccount::from(r)));
    }

    match (&existing, req.active) {
        (None, _) => Ok(None),
        (Some(_), Some(false)) => {
            let unsettled = crate::trades::list_open_or_closing_by_account(pool, id)
                .await
                .map_err(UpdateAccountError::from)?;
            Err(UpdateAccountError::OpenTrades {
                account_id: id,
                count: unsettled.len(),
            })
        }
        (Some(_), _) => Ok(None),
    }
}

async fn reject_if_other_live_account_active(
    pool: &PgPool,
    exchange: &str,
    id: Uuid,
) -> Result<(), UpdateAccountError> {
    let existing_live: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM trading_accounts
         WHERE exchange = $1 AND account_type = 'live' AND active AND id <> $2
         LIMIT 1",
    )
    .bind(exchange)
    .bind(id)
    .fetch_optional(pool)
    .await?;
    if existing_live.is_some() {
        return Err(UpdateAccountError::Other(anyhow::anyhow!(
            build_one_live_per_exchange_conflict_message(REACTIVATE_ACCOUNT_OPERATION, exchange)
        )));
    }
    Ok(())
}

pub async fn delete_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM trading_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn recalculate_balance(pool: &PgPool, account_id: Uuid) -> anyhow::Result<Decimal> {
    let (active_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM trades WHERE account_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    if active_count > 0 {
        anyhow::bail!(
            "failed to recalculate balance for account: \
             {active_count} open/closing trade(s) exist"
        );
    }

    let (initial_balance,): (Decimal,) =
        sqlx::query_as("SELECT initial_balance FROM trading_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("failed to recalculate balance for account: account not found")
            })?;

    let (total_pnl, total_fees): (Option<Decimal>, Option<Decimal>) = sqlx::query_as(
        "SELECT SUM(pnl_amount), SUM(fees) FROM trades \
         WHERE account_id = $1 AND status = 'closed'",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;

    let total_pnl = total_pnl.unwrap_or(Decimal::ZERO);
    let total_fees = total_fees.unwrap_or(Decimal::ZERO);

    let new_balance = initial_balance + total_pnl - total_fees;

    update_balance(pool, account_id, new_balance).await?;

    Ok(new_balance)
}

pub async fn recalculate_all_balances(pool: &PgPool) -> anyhow::Result<Vec<(Uuid, Decimal)>> {
    let results: Vec<(Uuid, Decimal)> = sqlx::query_as(
        r#"WITH recalculated AS (
               SELECT
                   ta.id,
                   ta.initial_balance
                       + COALESCE(SUM(t.pnl_amount), 0)
                       - COALESCE(SUM(t.fees), 0) AS new_balance
               FROM trading_accounts ta
               LEFT JOIN trades t
                 ON t.account_id = ta.id
                AND t.status = 'closed'
               WHERE NOT EXISTS (
                   SELECT 1 FROM trades t2
                   WHERE t2.account_id = ta.id
                     AND t2.status IN ('open', 'closing')
               )
               GROUP BY ta.id, ta.initial_balance
           ),
           updated AS (
               UPDATE trading_accounts ta
                  SET current_balance = r.new_balance
                 FROM recalculated r
                WHERE ta.id = r.id
               RETURNING ta.id, ta.current_balance
           )
           SELECT id, current_balance FROM updated
           ORDER BY id"#,
    )
    .fetch_all(pool)
    .await?;
    Ok(results)
}

#[cfg(test)]
mod tests;
