use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Hard minimum initial balance for new accounts (JPY-denominated).
///
/// Derivation: bitFlyer Crypto CFD enforces a 0.001 BTC minimum order size
/// (https://bitflyer.com/ja-jp/faq/4-27). At a representative BTC price of
/// ~11M JPY with 2× leverage, the required margin for one minimum-size order
/// is roughly 5,500 JPY. Combined with a 10% loss budget per trade and a
/// safety buffer for adverse price moves, 10,000 JPY is the smallest balance
/// that can practically execute and survive a couple of trades. Anything
/// below this would be rejected by the exchange or wiped out instantly.
pub const MIN_INITIAL_BALANCE_JPY: Decimal = dec!(10000);

/// ASCII whitespace characters trimmed by `normalize_currency`. Mirrored on
/// the DB side by `BTRIM(currency, E' \t\n\r')` in
/// `migrations/20260407000001_min_balance_constraint.sql` so the application
/// and the CHECK constraint agree on what counts as "leading/trailing
/// whitespace". Note: PostgreSQL's `BTRIM(text)` with no second argument
/// strips spaces only, NOT tab/newline/CR — hence the explicit set.
const CURRENCY_TRIM_CHARS: &[char] = &[' ', '\t', '\n', '\r'];

/// Normalize a currency code to canonical form: surrounding ASCII whitespace
/// trimmed and ASCII uppercased (e.g. `" jpy "` → `"JPY"`,
/// `"\tjpy\n"` → `"JPY"`). Matches the
/// `UPPER(BTRIM(currency, E' \t\n\r'))` form used by the DB CHECK constraint.
pub fn normalize_currency(currency: &str) -> String {
    currency
        .trim_matches(CURRENCY_TRIM_CHARS)
        .to_ascii_uppercase()
}

/// Validate that a currency / initial_balance combination satisfies the
/// minimum-balance invariant. The currency is normalized internally so
/// callers may pass any casing — keeping the bypass surface area small.
/// Defense-in-depth: the same rule is enforced by a CHECK constraint in
/// the database (see `migrations/20260407000001_min_balance_constraint.sql`).
///
/// The error message is intentionally exchange-agnostic — this rule applies
/// uniformly to every JPY-denominated paper account (FX, crypto, …). The
/// 10,000 JPY floor was originally derived from bitFlyer Crypto CFD's
/// 0.001 BTC minimum order size, but the same number is also a sensible
/// minimum for FX paper accounts where leveraged tick moves can wipe out
/// tiny balances within a single trade.
pub fn validate_initial_balance(currency: &str, initial_balance: Decimal) -> Result<(), String> {
    if normalize_currency(currency) == "JPY" && initial_balance < MIN_INITIAL_BALANCE_JPY {
        return Err(format!(
            "initial_balance must be at least {MIN_INITIAL_BALANCE_JPY} JPY (minimum required for JPY-denominated paper accounts to cover margin, trading losses, and a safety buffer)"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PaperAccount {
    pub id: Uuid,
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub current_balance: Decimal,
    pub currency: String,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePaperAccount {
    pub name: String,
    pub exchange: String,
    pub initial_balance: Decimal,
    pub leverage: Decimal,
    pub strategy: String,
    pub account_type: String,
    #[serde(default = "default_currency")]
    pub currency: String,
}

fn default_currency() -> String {
    "JPY".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePaperAccount {
    pub name: Option<String>,
    pub leverage: Option<Decimal>,
    pub strategy: Option<String>,
}

const ACCOUNT_COLUMNS: &str = "id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, account_type, created_at, updated_at";

pub async fn list_paper_accounts(pool: &PgPool) -> anyhow::Result<Vec<PaperAccount>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM paper_accounts ORDER BY created_at ASC"
    );
    let accounts = sqlx::query_as::<_, PaperAccount>(&sql)
        .fetch_all(pool)
        .await?;
    Ok(accounts)
}

pub async fn get_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<PaperAccount>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM paper_accounts WHERE id = $1"
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(account)
}

pub async fn create_paper_account(
    pool: &PgPool,
    req: &CreatePaperAccount,
) -> anyhow::Result<PaperAccount> {
    // Normalize and validate currency / balance before touching the DB so
    // direct (non-API) callers also get the invariant. The DB layer is the
    // last line of defense; the HTTP handler does the same up-front for
    // friendlier error responses.
    let currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&currency, req.initial_balance) {
        anyhow::bail!(msg);
    }

    let id = Uuid::new_v4();
    let sql = format!(
        r#"INSERT INTO paper_accounts (id, name, exchange, initial_balance, current_balance, currency, leverage, strategy, account_type)
           VALUES ($1, $2, $3, $4, $4, $5, $6, $7, $8)
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(&req.exchange)
        .bind(req.initial_balance)
        .bind(&currency)
        .bind(req.leverage)
        .bind(&req.strategy)
        .bind(&req.account_type)
        .fetch_one(pool)
        .await?;
    Ok(account)
}

pub async fn update_paper_account(
    pool: &PgPool,
    id: Uuid,
    req: &UpdatePaperAccount,
) -> anyhow::Result<Option<PaperAccount>> {
    let sql = format!(
        r#"UPDATE paper_accounts SET
            name = COALESCE($2, name),
            leverage = COALESCE($3, leverage),
            strategy = COALESCE($4, strategy),
            updated_at = NOW()
           WHERE id = $1
           RETURNING {ACCOUNT_COLUMNS}"#
    );
    let account = sqlx::query_as::<_, PaperAccount>(&sql)
        .bind(id)
        .bind(&req.name)
        .bind(req.leverage)
        .bind(&req.strategy)
        .fetch_optional(pool)
        .await?;
    Ok(account)
}

/// Add a P&L delta to current_balance (positive or negative).
/// Returns error if the account does not exist.
pub async fn add_pnl(pool: &PgPool, id: Uuid, pnl_delta: Decimal) -> anyhow::Result<()> {
    let result = sqlx::query("UPDATE paper_accounts SET current_balance = current_balance + $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(pnl_delta)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        anyhow::bail!("paper account {id} not found when updating balance");
    }
    Ok(())
}

pub async fn delete_paper_account(pool: &PgPool, id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query("DELETE FROM paper_accounts WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Look up account_type by id. Returns None when the account does not exist.
pub async fn get_account_type(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<String>> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT account_type FROM paper_accounts WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(v,)| v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_currency_uppercases_and_trims_ascii_whitespace() {
        assert_eq!(normalize_currency("jpy"), "JPY");
        assert_eq!(normalize_currency("Jpy"), "JPY");
        assert_eq!(normalize_currency("  jpy  "), "JPY");
        // Tab / newline / carriage-return are explicitly part of the trim
        // set so they can't sneak past the DB CHECK either (Postgres'
        // default BTRIM only strips spaces).
        assert_eq!(normalize_currency("\tJPY\n"), "JPY");
        assert_eq!(normalize_currency("\r\nJPY\r\n"), "JPY");
        assert_eq!(normalize_currency(" \t jpy \n "), "JPY");
        assert_eq!(normalize_currency("USD"), "USD");
    }

    #[test]
    fn validate_initial_balance_catches_tab_newline_jpy() {
        // Defense against the BTRIM(text) default-trim escape route.
        assert!(validate_initial_balance("\tJPY\n", dec!(1)).is_err());
        assert!(validate_initial_balance("\r\njpy\r\n", dec!(9999)).is_err());
        assert!(validate_initial_balance("\tJPY\n", dec!(10000)).is_ok());
    }

    #[test]
    fn normalize_currency_handles_empty_and_whitespace_only() {
        assert_eq!(normalize_currency(""), "");
        assert_eq!(normalize_currency("   "), "");
    }

    #[test]
    fn normalize_currency_does_not_unicode_uppercase() {
        // to_ascii_uppercase is intentional: full-width or non-ASCII letters
        // are NOT normalized. The DB CHECK uses UPPER(BTRIM(...)) which is
        // also Postgres' default-locale uppercase, so anything we don't catch
        // here would still hit the constraint as a non-canonical token.
        let fullwidth = "ＪＰＹ";
        let normalized = normalize_currency(fullwidth);
        assert_ne!(normalized, "JPY");
        // ...but it's still trimmed of surrounding whitespace.
        assert_eq!(normalize_currency("  ＪＰＹ  "), fullwidth);
    }

    #[test]
    fn validate_initial_balance_rejects_too_small_jpy() {
        assert!(validate_initial_balance("JPY", dec!(9999)).is_err());
        assert!(validate_initial_balance("JPY", dec!(0)).is_err());
    }

    #[test]
    fn validate_initial_balance_accepts_min_and_above_jpy() {
        assert!(validate_initial_balance("JPY", dec!(10000)).is_ok());
        assert!(validate_initial_balance("JPY", dec!(100000)).is_ok());
    }

    #[test]
    fn validate_initial_balance_does_not_check_non_jpy() {
        // Non-JPY currencies are out of scope for the min-balance rule today.
        assert!(validate_initial_balance("USD", dec!(1)).is_ok());
        assert!(validate_initial_balance("EUR", dec!(0)).is_ok());
    }

    #[test]
    fn validate_initial_balance_normalizes_input_currency() {
        // Bypass surface: the validator normalizes internally so any
        // lowercase / padded form still triggers the JPY rule.
        assert!(validate_initial_balance("jpy", dec!(1)).is_err());
        assert!(validate_initial_balance("  Jpy  ", dec!(1)).is_err());
        assert!(validate_initial_balance("jpy", dec!(10000)).is_ok());
    }
}
