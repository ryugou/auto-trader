//! Trading account DB access.
//!
//! Unified trading account row (replaces the old `paper_accounts` table).
//! Backed by the `trading_accounts` table from migration 20260415000001.

use auto_trader_core::types::Exchange;
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Hard minimum initial balance for JPY-denominated accounts.
pub const MIN_INITIAL_BALANCE_JPY: Decimal = dec!(10000);

/// Normalize a currency code: trim ASCII whitespace + uppercase.
pub fn normalize_currency(currency: &str) -> String {
    currency
        .trim_matches([' ', '\t', '\n', '\r'])
        .to_ascii_uppercase()
}

/// Validate that a currency / initial_balance satisfies the minimum-balance rule.
pub fn validate_initial_balance(currency: &str, initial_balance: Decimal) -> Result<(), String> {
    if normalize_currency(currency) == "JPY" && initial_balance < MIN_INITIAL_BALANCE_JPY {
        return Err(format!(
            "initial_balance must be at least {MIN_INITIAL_BALANCE_JPY} JPY"
        ));
    }
    Ok(())
}

/// Unified account row (replaces PaperAccount).
#[derive(Debug, Clone, Serialize)]
pub struct TradingAccount {
    pub id: Uuid,
    pub name: String,
    pub account_type: String,
    pub exchange: String,
    pub strategy: String,
    pub initial_balance: Decimal,
    pub current_balance: Decimal,
    pub leverage: Decimal,
    pub currency: String,
    pub created_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: Uuid,
    name: String,
    account_type: String,
    exchange: String,
    strategy: String,
    initial_balance: Decimal,
    current_balance: Decimal,
    leverage: Decimal,
    currency: String,
    created_at: DateTime<Utc>,
}

impl From<AccountRow> for TradingAccount {
    fn from(r: AccountRow) -> Self {
        TradingAccount {
            id: r.id,
            name: r.name,
            account_type: r.account_type,
            exchange: r.exchange,
            strategy: r.strategy,
            initial_balance: r.initial_balance,
            current_balance: r.current_balance,
            leverage: r.leverage,
            currency: r.currency,
            created_at: r.created_at,
        }
    }
}

const ACCOUNT_COLUMNS: &str = "id, name, account_type, exchange, strategy, \
                                initial_balance, current_balance, leverage, currency, created_at";

/// Fetch a single account by id (alias for `get_account`).
pub async fn get(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    get_account(pool, id).await
}

/// Fetch a single account by id.
pub async fn get_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<TradingAccount>> {
    let row = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(TradingAccount::from))
}

/// List all accounts ordered by created_at.
pub async fn list_all(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

/// Update current_balance for an account.
pub async fn update_balance(pool: &PgPool, id: Uuid, new_balance: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE trading_accounts SET current_balance = $2 WHERE id = $1")
        .bind(id)
        .bind(new_balance)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("trading account {id} not found when updating balance");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// CRUD types and functions (for REST API)
// ---------------------------------------------------------------------------

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize)]
pub struct CreateTradingAccount {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    #[serde(default = "default_currency")]
    pub currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTradingAccount {
    pub name: Option<String>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
}

pub async fn create_account(
    pool: &PgPool,
    req: &CreateTradingAccount,
) -> anyhow::Result<TradingAccount> {
    // Defense in depth: `create_account` is callable from non-HTTP paths
    // (CLI, tests, future internal callers), and the HTTP deserializer does
    // not constrain this field. Reject invalid values here so a bad string
    // never reaches the DB CHECK constraint.
    if req.account_type != "paper" && req.account_type != "live" {
        anyhow::bail!(
            "invalid account_type '{}' (must be 'paper' or 'live')",
            req.account_type
        );
    }
    // exchange を正規化して大文字/小文字・余白の差異で unique 制約を回避できないようにする。
    let exchange = req.exchange.trim().to_ascii_lowercase();
    // Validate exchange matches the DB CHECK constraint pattern before INSERT.
    if exchange.is_empty()
        || !exchange
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        anyhow::bail!(
            "invalid exchange '{}': must be non-empty and contain only [a-z0-9_]",
            req.exchange
        );
    }
    // Reject unknown exchange names so misconfigured accounts never reach the DB.
    if let Err(e) = exchange.parse::<Exchange>() {
        anyhow::bail!("{e}");
    }
    // live 口座は同一 exchange に 1 件のみ許可 (bitFlyer API client が
    // singleton のため、複数行があると margin / collateral 共有で会計破綻する)。
    // 通常フローの早期失敗として SELECT で確認する。並行 INSERT が競合した場合は
    // DB 側の partial unique index (trading_accounts_one_live_per_exchange) が
    // 守る（Fix 6: INSERT エラーを friendly message に変換）。
    if req.account_type == "live" {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM trading_accounts
             WHERE exchange = $1 AND account_type = 'live'
             LIMIT 1",
        )
        .bind(&exchange)
        .fetch_optional(pool)
        .await?;
        if let Some((existing_id,)) = existing {
            anyhow::bail!(
                "live account for exchange '{}' already exists (id={}); only 1 live account per exchange is supported",
                exchange,
                existing_id
            );
        }
    }
    let currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&currency, req.initial_balance) {
        anyhow::bail!(msg);
    }
    let initial_balance = if currency == "JPY" {
        req.initial_balance
            .round_dp_with_strategy(0, RoundingStrategy::ToZero)
    } else {
        req.initial_balance
    };
    let id = Uuid::new_v4();
    let sql = format!(
        r#"INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                          initial_balance, current_balance, leverage, currency)
           VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8)
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(&req.account_type)
        .bind(&exchange)
        .bind(&req.strategy)
        .bind(initial_balance)
        .bind(req.leverage)
        .bind(&currency)
        .fetch_one(pool)
        .await
        .map_err(|e| -> anyhow::Error {
            // Concurrent inserts can race past the app-layer pre-check above.
            // The DB partial unique index is the real guard; translate its
            // unique_violation (23505) into a friendly error.
            if let sqlx::Error::Database(ref db_err) = e {
                match db_err.constraint() {
                    Some("trading_accounts_one_live_per_exchange") => {
                        return anyhow::anyhow!(
                            "live account for exchange '{}' already exists",
                            exchange
                        );
                    }
                    Some("trading_accounts_exchange_normalized") => {
                        return anyhow::anyhow!(
                            "invalid exchange '{}': must match ^[a-z0-9_]+$",
                            exchange
                        );
                    }
                    _ => {}
                }
            }
            e.into()
        })?;
    Ok(TradingAccount::from(row))
}

pub async fn update_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdateTradingAccount,
) -> anyhow::Result<Option<TradingAccount>> {
    let sql = format!(
        r#"UPDATE trading_accounts SET
               name = COALESCE($2, name),
               leverage = COALESCE($3, leverage),
               strategy = COALESCE($4, strategy)
           WHERE id = $1
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let row = sqlx::query_as::<_, AccountRow>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(req.leverage)
        .bind(&req.strategy)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(TradingAccount::from))
}

pub async fn delete_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM trading_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Recalculate `current_balance` for a single account from the event source
/// of truth (the `trades` table).
///
/// Formula: `current_balance = initial_balance + SUM(pnl_amount) - SUM(fees)`
/// where only **closed** trades are considered.
///
/// This is useful when the cached `current_balance` column may have drifted
/// due to bugs, manual DB edits, or replayed events, and you want to rebuild
/// it from scratch.
///
/// **Precondition:** the account should have no open or closing trades when
/// this is called. Open-trade margin locks and accrued fees (e.g. overnight
/// fees) are not included in the aggregation — only closed-trade PnL and
/// fees are summed.
pub async fn recalculate_balance(pool: &PgPool, account_id: Uuid) -> anyhow::Result<Decimal> {
    // 0. Enforce precondition: no open/closing trades.
    let (active_count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM trades WHERE account_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    if active_count > 0 {
        anyhow::bail!(
            "cannot recalculate balance for account {account_id}: \
             {active_count} open/closing trade(s) exist"
        );
    }

    // 1. Fetch initial_balance
    let (initial_balance,): (Decimal,) = sqlx::query_as(
        "SELECT initial_balance FROM trading_accounts WHERE id = $1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("trading account {account_id} not found"))?;

    // 2. Aggregate closed trades
    let (total_pnl, total_fees): (Option<Decimal>, Option<Decimal>) = sqlx::query_as(
        "SELECT SUM(pnl_amount), SUM(fees) FROM trades \
         WHERE account_id = $1 AND status = 'closed'",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;

    let total_pnl = total_pnl.unwrap_or(Decimal::ZERO);
    let total_fees = total_fees.unwrap_or(Decimal::ZERO);

    // 3. Calculate new balance
    let new_balance = initial_balance + total_pnl - total_fees;

    // 4. Persist
    update_balance(pool, account_id, new_balance).await?;

    Ok(new_balance)
}

/// Recalculate `current_balance` for **all** trading accounts in a single
/// set-based SQL statement (no N+1 round-trips).
///
/// This is the "full rebuild" variant — useful after migrations or bulk
/// data corrections when you want to ensure every account's cached balance
/// matches the trades ledger.
///
/// **Precondition:** accounts should have no open or closing trades (see
/// [`recalculate_balance`] for details).
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
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn live_req(exchange: &str) -> CreateTradingAccount {
        CreateTradingAccount {
            name: format!("live-{exchange}"),
            exchange: exchange.to_string(),
            initial_balance: dec!(50000),
            leverage: dec!(1),
            strategy: "bb_mean_revert_v1".to_string(),
            account_type: "live".to_string(),
            currency: "JPY".to_string(),
        }
    }

    /// A second live INSERT for the same exchange must be rejected by the
    /// app-layer pre-check (mirrors the DB partial unique index).
    #[sqlx::test(migrations = "../../migrations")]
    async fn duplicate_live_insert_same_exchange_fails(pool: sqlx::PgPool) {
        let req = live_req("bitflyer_cfd");
        create_account(&pool, &req).await.expect("first insert ok");
        let err = create_account(&pool, &req)
            .await
            .expect_err("second live insert should fail");
        assert!(
            err.to_string().contains("already exists"),
            "unexpected error: {err}"
        );
    }

    /// A live account for a different exchange must be allowed (independent
    /// collateral pools).
    #[sqlx::test(migrations = "../../migrations")]
    async fn live_insert_different_exchange_succeeds(pool: sqlx::PgPool) {
        create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("first exchange ok");
        create_account(&pool, &live_req("oanda"))
            .await
            .expect("different exchange should succeed");
    }

    /// An unknown exchange name must be rejected before reaching the DB.
    #[sqlx::test(migrations = "../../migrations")]
    async fn create_account_rejects_unknown_exchange(pool: sqlx::PgPool) {
        let req = CreateTradingAccount {
            name: "bad-exchange".to_string(),
            exchange: "unknown".to_string(),
            initial_balance: dec!(50000),
            leverage: dec!(1),
            strategy: "bb_mean_revert_v1".to_string(),
            account_type: "paper".to_string(),
            currency: "JPY".to_string(),
        };
        let err = create_account(&pool, &req)
            .await
            .expect_err("unknown exchange should be rejected");
        assert!(
            err.to_string().contains("unknown exchange 'unknown'"),
            "unexpected error: {err}"
        );
    }
}
