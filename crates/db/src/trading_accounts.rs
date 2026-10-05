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

/// Validate that `leverage` does not exceed the regulatory cap for `exchange`.
///
/// Caps reflect Japan FSA retail-account limits. Exhaustive `match` on
/// `Exchange` so adding a new variant fails compilation until a cap is
/// registered here.
///
/// Returns `Err(message)` suitable for surfacing as a 400 from the accounts API.
pub fn validate_leverage_for_exchange(exchange: Exchange, leverage: Decimal) -> Result<(), String> {
    let cap = match exchange {
        Exchange::BitflyerCfd => dec!(2),
        Exchange::GmoFx => dec!(25),
        // OANDA retail FX caps are not enforced here today; the account
        // shape is being deprecated and no new live OANDA accounts are
        // expected.
        Exchange::Oanda => return Ok(()),
    };
    if leverage > cap {
        Err(format!(
            "leverage {leverage} exceeds regulatory cap {cap} for {exchange}"
        ))
    } else {
        Ok(())
    }
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
    pub active: bool,
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
    active: bool,
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
            active: r.active,
        }
    }
}

const ACCOUNT_COLUMNS: &str = "id, name, account_type, exchange, strategy, \
                                initial_balance, current_balance, leverage, currency, created_at, active";

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

/// List every trading account regardless of `active`, ordered by created_at
/// ascending.
///
/// This is the unfiltered view: display/aggregation call sites that need to
/// show the full roster (e.g. the accounts API with `include_inactive=true`)
/// should use this. Call sites that decide where money moves must use
/// `list_active` or `list_active_or_with_open_trades` instead — see their
/// docs for which one applies.
pub async fn list_all(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

/// List only active accounts (`active = TRUE`), ordered by created_at ascending.
///
/// Use for call sites that decide *new* trading activity: matching a signal
/// to an account, picking accounts to register at startup, or any "trade on
/// this account" decision. A retired (inactive) account must never receive a
/// new entry, so it must never appear in this list. `get` / `get_account`
/// return an account regardless of its `active` flag.
pub async fn list_active(pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    let rows = sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {ACCOUNT_COLUMNS} FROM trading_accounts WHERE active ORDER BY created_at ASC"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(TradingAccount::from).collect())
}

/// List active accounts, plus any inactive (retired) account that still has
/// an unsettled trade (`trades.status IN ('open', 'closing')`), ordered by
/// created_at ascending. "Unsettled" includes `closing` (exit order placed,
/// not yet confirmed filled) alongside `open` — a position mid-close still
/// needs fees applied and margin monitored exactly like a fully open one.
///
/// Use for call sites that service *existing* positions rather than decide
/// new ones: overnight/swap fee accrual, SFD accrual, startup reconcile,
/// balance-drift detection, the liquidation-level startup gate, and margin
/// monitoring. Retiring an account (`active = FALSE`) stops new entries but
/// must not orphan a position still open or closing on it — those need fees
/// applied, margin monitored, and a liquidation level resolved exactly like
/// an active account's. Once the account has no open/closing trades left,
/// it naturally drops out of this list on its own (no separate cleanup
/// needed).
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

/// Kill Switch の halt 状態を読む。halted_until が NULL なら None。
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

/// Kill Switch を作動させる。解除は halted_until 経過を待つか、運用者が
/// SQL で halted_until を NULL にする。
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
    /// Retire (`false`) or reinstate (`true`) the account. `None` leaves the
    /// current value untouched. See `update_account` for the guards applied
    /// on each transition.
    ///
    /// Reinstating an account (`false` → `true`) makes it eligible for new
    /// trades starting from the next signal. The HTTP layer
    /// (`accounts::update`) re-validates the account's exchange against the
    /// process-startup-resolved liquidation-level map before calling
    /// `update_account` (same guard `create` applies); this function itself
    /// does not, so a non-HTTP caller bypassing the API is responsible for
    /// that check. One input is resolved once at process startup and is
    /// *not* recomputed by either path:
    /// - `fx_new` leverage/exchange assumption checks
    ///   (`startup::check_fx_new_account_assumptions`): these run exactly
    ///   once at startup, against the accounts that were active at that
    ///   moment. A reinstated `fx_new*` account is never re-checked, so a
    ///   leverage/exchange mismatch is not flagged again until the process
    ///   restarts.
    pub active: Option<bool>,
}

/// Translate a Postgres constraint violation from an INSERT/UPDATE on
/// `trading_accounts` into an `anyhow::Error` whose top-level message names
/// the violated constraint, while keeping `e` as the error source (via
/// `anyhow::Error::new(e).context(..)` rather than `anyhow::anyhow!(..)`) so
/// `ApiError::from(anyhow::Error)` can still find the original
/// `sqlx::Error::Database` by walking `anyhow::Error::chain()` and match on
/// its Postgres error code. Constraints not recognized here are returned
/// unmodified (`e.into()`); a generic fallback ("duplicate name") still
/// applies at the API layer if `name` ever gains a unique constraint.
fn translate_unique_violation(e: sqlx::Error, exchange: &str) -> anyhow::Error {
    let constraint = match &e {
        sqlx::Error::Database(db_err) => db_err.constraint().map(str::to_string),
        _ => None,
    };
    let message = match constraint.as_deref() {
        Some("trading_accounts_one_live_per_exchange") => Some(format!(
            "live account for exchange '{exchange}' already exists; \
             only 1 active live account per exchange is supported"
        )),
        Some("trading_accounts_exchange_normalized") => Some(format!(
            "invalid exchange '{exchange}': must match ^[a-z0-9_]+$"
        )),
        _ => None,
    };
    match message {
        Some(message) => anyhow::Error::new(e).context(message),
        None => e.into(),
    }
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
    // live 口座は同一 exchange に active な行が 1 件のみ許可 (bitFlyer API
    // client が singleton のため、複数行があると margin / collateral 共有で
    // 会計破綻する)。退役済み (active = FALSE) の live 口座は対象外 — 取引
    // 履歴を残したまま退役させた口座が、同じ exchange への新しい live 口座
    // 作成を永久に妨げてはならない。通常フローの早期失敗として SELECT で
    // 確認する。並行 INSERT が競合した場合は DB 側の partial unique index
    // (trading_accounts_one_live_per_exchange、WHERE account_type = 'live'
    // AND active) が守る（Fix 6: INSERT エラーを friendly message に変換）。
    if req.account_type == "live" {
        let existing: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM trading_accounts
             WHERE exchange = $1 AND account_type = 'live' AND active
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
    let exchange_enum: Exchange = exchange.parse()?;
    if let Err(msg) = validate_leverage_for_exchange(exchange_enum, req.leverage) {
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
        // Concurrent inserts can race past the app-layer pre-check above.
        // The DB partial unique index is the real guard.
        .map_err(|e| translate_unique_violation(e, &exchange))?;
    Ok(TradingAccount::from(row))
}

pub async fn update_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdateTradingAccount,
) -> anyhow::Result<Option<TradingAccount>> {
    // Fetch the existing row once when any validation needs the account's
    // current state (exchange for the leverage cap / live-reactivation
    // check, account_type for the live-reactivation check). Skipped on
    // no-op updates to avoid an extra query.
    let existing = if req.leverage.is_some() || req.active.is_some() {
        get_account(pool, id).await?
    } else {
        None
    };

    // Validate leverage against the account's exchange before UPDATE.
    if let Some(new_leverage) = req.leverage
        && let Some(account) = &existing
    {
        let exchange_enum: Exchange = account.exchange.parse()?;
        if let Err(msg) = validate_leverage_for_exchange(exchange_enum, new_leverage) {
            anyhow::bail!(msg);
        }
    }

    // Reinstating a live account must not collide with another already-active
    // live account on the same exchange — same reason `create_account`
    // rejects a second live row per exchange (shared singleton API client).
    //
    // The deactivation guard (reject retiring an account that still has an
    // open/closing trade) is *not* checked here as a separate query — it is
    // folded into the UPDATE's WHERE clause below so the check and the write
    // happen in one statement, closing the window where a committed trade
    // could appear between a standalone check and a follow-up UPDATE. It does
    // not see a concurrent transaction's *uncommitted* trade INSERT (READ
    // COMMITTED; the trades FK lock does not conflict with an `active`
    // UPDATE), so that residual case can leave an open trade on a retired
    // account — `list_active_or_with_open_trades` keeps such accounts covered
    // for fee application, margin monitoring and reconcile.
    if let Some(new_active) = req.active
        && new_active
        && let Some(account) = &existing
        && account.account_type == "live"
    {
        let existing_live: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM trading_accounts
             WHERE exchange = $1 AND account_type = 'live' AND active AND id <> $2
             LIMIT 1",
        )
        .bind(&account.exchange)
        .bind(id)
        .fetch_optional(pool)
        .await?;
        if let Some((other_id,)) = existing_live {
            anyhow::bail!(
                "cannot reactivate account {id}: live account for exchange '{}' already active (id={}); only 1 active live account per exchange is supported",
                account.exchange,
                other_id
            );
        }
    }

    // `$5 IS DISTINCT FROM FALSE` is true for both NULL (active untouched)
    // and TRUE (reactivating), so the NOT EXISTS guard only ever constrains
    // the row when `req.active == Some(false)` (retiring).
    let sql = format!(
        r#"UPDATE trading_accounts SET
               name = COALESCE($2, name),
               leverage = COALESCE($3, leverage),
               strategy = COALESCE($4, strategy),
               active = COALESCE($5, active)
           WHERE id = $1
             AND ($5 IS DISTINCT FROM FALSE OR NOT EXISTS (
                 SELECT 1 FROM trades t
                 WHERE t.account_id = $1 AND t.status IN ('open', 'closing')
             ))
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
        .map_err(|e| match &existing {
            Some(account) => translate_unique_violation(e, &account.exchange),
            // A name/strategy-only update (no `active`/`leverage` change)
            // never touches the columns the one-live-per-exchange /
            // exchange-normalized constraints guard, so no path here can
            // trigger either — leave the error untranslated.
            None => e.into(),
        })?;
    if let Some(r) = row {
        return Ok(Some(TradingAccount::from(r)));
    }

    // 0 rows is ambiguous between "no account with this id" and "the
    // deactivation guard blocked the write" — both look identical from
    // `rows_affected`. Disambiguate using `existing`, fetched before the
    // UPDATE ran.
    match (&existing, req.active) {
        (None, _) => Ok(None),
        (Some(_), Some(false)) => {
            let unsettled = crate::trades::list_open_or_closing_by_account(pool, id).await?;
            anyhow::bail!(
                "cannot deactivate account {id}: {} open/closing trade(s) exist; close them first",
                unsettled.len()
            );
        }
        // Every other `req.active` value makes the WHERE guard a no-op, so
        // 0 rows here would mean the row was deleted between the `existing`
        // fetch and the UPDATE above — not expected from any caller in this
        // codebase, but fall back to "not found" rather than panicking.
        (Some(_), _) => Ok(None),
    }
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
    let (initial_balance,): (Decimal,) =
        sqlx::query_as("SELECT initial_balance FROM trading_accounts WHERE id = $1")
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
/// Accounts with open or closing trades are **silently skipped** (not
/// updated) to avoid corrupting their balance. The returned vec only
/// contains accounts that were actually recalculated.
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
mod leverage_validation {
    use super::validate_leverage_for_exchange;
    use auto_trader_core::types::Exchange;
    use rust_decimal_macros::dec;

    #[test]
    fn gmo_fx_accepts_up_to_25x() {
        assert!(validate_leverage_for_exchange(Exchange::GmoFx, dec!(25)).is_ok());
        assert!(validate_leverage_for_exchange(Exchange::GmoFx, dec!(1)).is_ok());
    }

    #[test]
    fn gmo_fx_rejects_above_25x() {
        let err = validate_leverage_for_exchange(Exchange::GmoFx, dec!(26)).unwrap_err();
        assert!(err.contains("25"), "error mentions cap: {err}");
        assert!(err.contains("gmo_fx"), "error mentions exchange: {err}");
    }

    #[test]
    fn bitflyer_cfd_accepts_up_to_2x() {
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(2)).is_ok());
        assert!(validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(1)).is_ok());
    }

    #[test]
    fn bitflyer_cfd_rejects_above_2x() {
        let err = validate_leverage_for_exchange(Exchange::BitflyerCfd, dec!(3)).unwrap_err();
        assert!(err.contains("2"), "error mentions cap: {err}");
    }

    #[test]
    fn oanda_currently_uncapped() {
        assert!(validate_leverage_for_exchange(Exchange::Oanda, dec!(100)).is_ok());
    }
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

    fn paper_req(name: &str, exchange: &str) -> CreateTradingAccount {
        CreateTradingAccount {
            name: name.to_string(),
            exchange: exchange.to_string(),
            initial_balance: dec!(50000),
            leverage: dec!(1),
            strategy: "bb_mean_revert_v1".to_string(),
            account_type: "paper".to_string(),
            currency: "JPY".to_string(),
        }
    }

    fn no_op_update() -> UpdateTradingAccount {
        UpdateTradingAccount {
            name: None,
            leverage: None,
            strategy: None,
            active: None,
        }
    }

    /// Insert a minimal trade row directly (bypassing the executor) with the
    /// given `status` so tests can exercise `list_active_or_with_open_trades`
    /// and the deactivation guard across every unsettled status ('open' and
    /// 'closing') without the full signal -> trade pipeline.
    async fn insert_trade_with_status(pool: &sqlx::PgPool, account_id: Uuid, status: &str) {
        sqlx::query(
            r#"INSERT INTO trades
                   (id, account_id, strategy_name, pair, exchange, direction,
                    entry_price, quantity, leverage, stop_loss, entry_at, status)
               VALUES ($1, $2, 'bb_mean_revert_v1', 'FX_BTC_JPY', 'bitflyer_cfd', 'long',
                       100, 0.01, 2, 90, NOW(), $3)"#,
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(status)
        .execute(pool)
        .await
        .expect("insert trade with status");
    }

    async fn insert_open_trade(pool: &sqlx::PgPool, account_id: Uuid) {
        insert_trade_with_status(pool, account_id, "open").await;
    }

    /// `list_all` returns every account regardless of `active` — it is the
    /// unfiltered view for display/aggregation, not a trading-decision list.
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_all_includes_inactive_accounts(pool: sqlx::PgPool) {
        let retired_id = list_all(&pool)
            .await
            .expect("list accounts")
            .first()
            .expect("migrations seed at least one account")
            .id;

        sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
            .bind(retired_id)
            .execute(&pool)
            .await
            .expect("deactivate account");

        let listed = list_all(&pool).await.expect("list accounts after retire");
        assert!(
            listed.iter().any(|a| a.id == retired_id),
            "list_all must still include the now-inactive account {retired_id}"
        );
    }

    /// `list_active` must hide retired accounts while `get` still returns them.
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_active_excludes_inactive_accounts(pool: sqlx::PgPool) {
        let retired_id = list_active(&pool)
            .await
            .expect("list accounts")
            .first()
            .expect("migrations seed at least one active account")
            .id;

        sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
            .bind(retired_id)
            .execute(&pool)
            .await
            .expect("deactivate account");

        let listed = list_active(&pool)
            .await
            .expect("list accounts after retire");
        assert!(
            listed.iter().all(|a| a.id != retired_id),
            "inactive account {retired_id} must not appear in list_active"
        );
        let fetched = get(&pool, retired_id)
            .await
            .expect("get account")
            .expect("get returns inactive accounts");
        assert!(!fetched.active, "fetched account must be inactive");
    }

    /// `list_active_or_with_open_trades` must include an active account, must
    /// include a retired account that still has an open trade, and must
    /// exclude a retired account with no open trades.
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_active_or_with_open_trades_covers_active_and_retired_with_open_trades(
        pool: sqlx::PgPool,
    ) {
        let active_id = create_account(&pool, &paper_req("active_acct", "gmo_fx"))
            .await
            .expect("create active account")
            .id;
        let retired_no_trades_id = create_account(&pool, &paper_req("retired_no_trades", "gmo_fx"))
            .await
            .expect("create retired-no-trades account")
            .id;
        let retired_with_trade_id =
            create_account(&pool, &paper_req("retired_with_trade", "gmo_fx"))
                .await
                .expect("create retired-with-trade account")
                .id;

        for id in [retired_no_trades_id, retired_with_trade_id] {
            sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await
                .expect("deactivate account");
        }
        insert_open_trade(&pool, retired_with_trade_id).await;

        let active_only = list_active(&pool).await.expect("list_active");
        assert!(
            active_only.iter().any(|a| a.id == active_id),
            "list_active must include the active account"
        );
        assert!(
            active_only
                .iter()
                .all(|a| a.id != retired_no_trades_id && a.id != retired_with_trade_id),
            "list_active must exclude every retired account regardless of open trades"
        );

        let active_or_open = list_active_or_with_open_trades(&pool)
            .await
            .expect("list_active_or_with_open_trades");
        assert!(
            active_or_open.iter().any(|a| a.id == active_id),
            "must include the active account"
        );
        assert!(
            active_or_open.iter().any(|a| a.id == retired_with_trade_id),
            "must include the retired account that still has an open trade"
        );
        assert!(
            active_or_open.iter().all(|a| a.id != retired_no_trades_id),
            "must exclude the retired account with no open trades"
        );

        let all = list_all(&pool).await.expect("list_all");
        assert!(
            all.iter().any(|a| a.id == retired_no_trades_id),
            "list_all must include every account regardless of trades"
        );
    }

    /// A retired account with a `closing` (not yet confirmed filled) trade
    /// must still be included — the position still needs fees applied and
    /// margin monitored while the exit order is in flight.
    #[sqlx::test(migrations = "../../migrations")]
    async fn list_active_or_with_open_trades_includes_retired_account_with_closing_trade(
        pool: sqlx::PgPool,
    ) {
        let retired_closing_id = create_account(&pool, &paper_req("retired_closing", "gmo_fx"))
            .await
            .expect("create retired-closing account")
            .id;
        sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
            .bind(retired_closing_id)
            .execute(&pool)
            .await
            .expect("deactivate account");
        insert_trade_with_status(&pool, retired_closing_id, "closing").await;

        let listed = list_active_or_with_open_trades(&pool)
            .await
            .expect("list_active_or_with_open_trades");
        assert!(
            listed.iter().any(|a| a.id == retired_closing_id),
            "must include the retired account that still has a closing trade"
        );
    }

    /// Retiring a live account frees its exchange slot: a new live account
    /// for the same exchange is then allowed to be created.
    #[sqlx::test(migrations = "../../migrations")]
    async fn live_insert_same_exchange_succeeds_after_existing_retired(pool: sqlx::PgPool) {
        let first = create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("first live insert ok");
        sqlx::query("UPDATE trading_accounts SET active = FALSE WHERE id = $1")
            .bind(first.id)
            .execute(&pool)
            .await
            .expect("retire first live account");

        create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("second live insert should succeed once the first is retired");
    }

    /// Deactivating an account with an open trade is rejected; the error
    /// reports the open/closing trade count so the operator knows what to
    /// close first.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_rejects_deactivation_with_open_trades(pool: sqlx::PgPool) {
        let account = create_account(&pool, &paper_req("has_open_trade", "gmo_fx"))
            .await
            .expect("create account");
        insert_open_trade(&pool, account.id).await;

        let err = update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect_err("deactivation with an open trade must be rejected");
        assert!(
            err.to_string().contains("1 open/closing trade"),
            "error must report the open/closing trade count: {err}"
        );
    }

    /// Deactivating an account whose only unsettled trade is `closing` (exit
    /// order placed, not yet confirmed filled) is rejected the same way an
    /// `open` trade would be — the account must stay active until the
    /// position is fully closed.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_rejects_deactivation_with_closing_trades(pool: sqlx::PgPool) {
        let account = create_account(&pool, &paper_req("has_closing_trade", "gmo_fx"))
            .await
            .expect("create account");
        insert_trade_with_status(&pool, account.id, "closing").await;

        let err = update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect_err("deactivation with a closing trade must be rejected");
        assert!(
            err.to_string().contains("1 open/closing trade"),
            "error must report the closing trade: {err}"
        );
    }

    /// When both an open and a closing trade exist on the same account, the
    /// error reports their combined count, not just one status.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_rejects_deactivation_counts_open_and_closing_together(
        pool: sqlx::PgPool,
    ) {
        let account = create_account(&pool, &paper_req("has_both_statuses", "gmo_fx"))
            .await
            .expect("create account");
        insert_trade_with_status(&pool, account.id, "open").await;
        insert_trade_with_status(&pool, account.id, "closing").await;

        let err = update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect_err("deactivation with open + closing trades must be rejected");
        assert!(
            err.to_string().contains("2 open/closing trade"),
            "error must report the combined count of 2: {err}"
        );
    }

    /// Deactivating a nonexistent account must return `Ok(None)` (not-found),
    /// not the open/closing-trade error — the 0-row UPDATE result must be
    /// disambiguated correctly even when `req.active == Some(false)`.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_returns_none_for_nonexistent_account_when_deactivating(
        pool: sqlx::PgPool,
    ) {
        let result = update_account(
            &pool,
            Uuid::new_v4(),
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect("a nonexistent account must not error");
        assert!(
            result.is_none(),
            "nonexistent account must report Ok(None), not an error"
        );
    }

    /// Deactivating an account with no open trades succeeds.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_allows_deactivation_without_open_trades(pool: sqlx::PgPool) {
        let account = create_account(&pool, &paper_req("no_open_trade", "gmo_fx"))
            .await
            .expect("create account");

        let updated = update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect("deactivation should succeed")
        .expect("account exists");
        assert!(!updated.active);
    }

    /// Reactivating a live account is rejected while another active live
    /// account already exists for the same exchange.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_rejects_reactivating_live_when_another_active_live_exists(
        pool: sqlx::PgPool,
    ) {
        let retired = create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("create first live account");
        update_account(
            &pool,
            retired.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect("retire first live account");
        create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("create second live account while first is retired");

        let err = update_account(
            &pool,
            retired.id,
            &UpdateTradingAccount {
                active: Some(true),
                ..no_op_update()
            },
        )
        .await
        .expect_err("reactivating must be rejected while another live account is active");
        assert!(
            err.to_string().contains("already active"),
            "unexpected error: {err}"
        );
    }

    /// Reactivating a live account succeeds when no other active live
    /// account exists for the same exchange.
    #[sqlx::test(migrations = "../../migrations")]
    async fn update_account_allows_reactivating_live_when_no_conflict(pool: sqlx::PgPool) {
        let account = create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("create live account");
        update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(false),
                ..no_op_update()
            },
        )
        .await
        .expect("retire account");

        let reactivated = update_account(
            &pool,
            account.id,
            &UpdateTradingAccount {
                active: Some(true),
                ..no_op_update()
            },
        )
        .await
        .expect("reactivation should succeed")
        .expect("account exists");
        assert!(reactivated.active);
    }

    /// The FX-new migration retires the legacy FX accounts and seeds `FX新`.
    /// Legacy ids (031/032) are not seeded by every migration history, so they
    /// are only asserted when present.
    #[sqlx::test(migrations = "../../migrations")]
    async fn migration_retires_legacy_fx_accounts_and_seeds_fx_new(pool: sqlx::PgPool) {
        let listed = list_active(&pool).await.expect("list accounts");
        let fx_new_id = Uuid::parse_str("a0000000-0000-0000-0000-000000000040").unwrap();

        let fx_new = listed
            .iter()
            .find(|a| a.id == fx_new_id)
            .expect("FX新 account is seeded and active");
        assert_eq!(fx_new.name, "FX新");
        assert_eq!(fx_new.strategy, "fx_new_v1");
        assert_eq!(fx_new.exchange, "gmo_fx");
        assert_eq!(fx_new.account_type, "paper");
        assert_eq!(fx_new.leverage, dec!(10));
        assert!(fx_new.active);

        for legacy in [
            "a0000000-0000-0000-0000-000000000031",
            "a0000000-0000-0000-0000-000000000032",
        ] {
            let id = Uuid::parse_str(legacy).unwrap();
            assert!(
                listed.iter().all(|a| a.id != id),
                "legacy account {id} must not be listed"
            );
            if let Some(account) = get(&pool, id).await.expect("get legacy account") {
                assert!(!account.active, "legacy account {id} must be inactive");
            }
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

    /// `update_account_rejects_reactivating_live_when_another_active_live_exists`
    /// (above) covers the sequential case: the app-level pre-check inside
    /// `update_account` sees the conflicting active row and rejects with a
    /// friendly "already active" message *before* ever reaching the UPDATE.
    /// That guard can only be bypassed by genuine concurrency (a second
    /// transaction committing a conflicting row between the guard's SELECT
    /// and the UPDATE) — not reproducible deterministically without an
    /// async-runtime dev-dependency this crate does not have. This test
    /// instead verifies the DB-layer fallback itself: `translate_unique_violation`
    /// (the function `update_account`'s `.map_err` now shares with
    /// `create_account`) against a *real* `sqlx::Error::Database` obtained by
    /// provoking the same unique index with a raw INSERT, bypassing
    /// `create_account`'s app-level pre-check the way a non-guarded write
    /// path would.
    #[sqlx::test(migrations = "../../migrations")]
    async fn translate_unique_violation_reports_exchange_and_already_exists(pool: sqlx::PgPool) {
        let first = create_account(&pool, &live_req("bitflyer_cfd"))
            .await
            .expect("create first live account");

        let db_err = sqlx::query(
            "INSERT INTO trading_accounts
                 (id, name, account_type, exchange, strategy,
                  initial_balance, current_balance, leverage, currency, active)
             VALUES ($1, 'second-live', 'live', 'bitflyer_cfd', 'bb_mean_revert_v1',
                     50000, 50000, 1, 'JPY', TRUE)",
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .expect_err("a second active live row for the same exchange must violate the unique index");
        assert!(
            matches!(db_err, sqlx::Error::Database(_)),
            "expected a database error from the unique index, got: {db_err:?}"
        );

        let translated = translate_unique_violation(db_err, &first.exchange);
        assert!(
            translated.to_string().contains(&first.exchange)
                && translated.to_string().contains("already exists"),
            "message must name the exchange and say it already exists: {translated}"
        );
        // The original sqlx::Error must still be reachable via the chain so
        // `ApiError::from(anyhow::Error)` can match on the Postgres
        // constraint name.
        assert!(
            translated.chain().any(|c| matches!(
                c.downcast_ref::<sqlx::Error>(),
                Some(sqlx::Error::Database(e))
                    if e.constraint() == Some("trading_accounts_one_live_per_exchange")
            )),
            "the original sqlx::Error::Database must remain in the anyhow chain"
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

    /// Kill Switch halt state round-trips: None initially, then the stored
    /// (until, reason) after `set_halt`.
    #[sqlx::test(migrations = "../../migrations")]
    async fn set_and_get_halt_roundtrip(pool: sqlx::PgPool) {
        use chrono::TimeZone;
        let account = list_all(&pool)
            .await
            .expect("list accounts")
            .into_iter()
            .next()
            .expect("migrations seed at least one account");

        assert!(
            get_halt(&pool, account.id).await.unwrap().is_none(),
            "no halt set initially"
        );

        let until = chrono::Utc.with_ymd_and_hms(2026, 7, 8, 0, 0, 0).unwrap();
        set_halt(&pool, account.id, until, "daily loss limit")
            .await
            .expect("set_halt");

        let (got_until, got_reason) = get_halt(&pool, account.id)
            .await
            .expect("get_halt")
            .expect("halt should be set");
        assert_eq!(got_until, until);
        assert_eq!(got_reason.as_deref(), Some("daily loss limit"));
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
