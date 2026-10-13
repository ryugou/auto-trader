use auto_trader_core::types::Exchange;
use rust_decimal::{Decimal, RoundingStrategy};
use rust_decimal_macros::dec;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::model::{ACCOUNT_COLUMNS, AccountRow, TradingAccount};

pub const MIN_INITIAL_BALANCE_JPY: Decimal = dec!(10000);

pub fn normalize_currency(currency: &str) -> String {
    currency
        .trim_matches([' ', '\t', '\n', '\r'])
        .to_ascii_uppercase()
}

pub fn validate_initial_balance(currency: &str, initial_balance: Decimal) -> Result<(), String> {
    if normalize_currency(currency) == "JPY" && initial_balance < MIN_INITIAL_BALANCE_JPY {
        return Err(format!(
            "invalid initial_balance: must be at least {MIN_INITIAL_BALANCE_JPY} JPY"
        ));
    }
    Ok(())
}

pub fn validate_leverage_for_exchange(exchange: Exchange, leverage: Decimal) -> Result<(), String> {
    let cap = match exchange {
        Exchange::BitflyerCfd => dec!(2),
        Exchange::GmoFx => dec!(25),
        Exchange::Oanda => return Ok(()),
    };
    if leverage > cap {
        Err(format!("invalid leverage: exceeds regulatory cap {cap}"))
    } else {
        Ok(())
    }
}

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

pub const CREATE_ACCOUNT_OPERATION: &str = "create account";
pub const REACTIVATE_ACCOUNT_OPERATION: &str = "reactivate account";

pub fn build_one_live_per_exchange_conflict_message(operation: &str, exchange: &str) -> String {
    format!(
        "failed to {operation}: live account already exists for exchange \"{exchange}\"; \
         only 1 live account per exchange is supported"
    )
}

pub(super) fn translate_unique_violation(
    e: sqlx::Error,
    operation: &str,
    exchange: &str,
) -> anyhow::Error {
    let constraint = match &e {
        sqlx::Error::Database(db_err) => db_err.constraint().map(str::to_string),
        _ => None,
    };
    let message = match constraint.as_deref() {
        Some("trading_accounts_one_live_per_exchange") => Some(
            build_one_live_per_exchange_conflict_message(operation, exchange),
        ),
        Some("trading_accounts_exchange_normalized") => {
            Some("invalid exchange: must match ^[a-z0-9_]+$".to_string())
        }
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
    if req.account_type != "paper" && req.account_type != "live" {
        anyhow::bail!("invalid account_type: must be 'paper' or 'live'");
    }
    let exchange = req.exchange.trim().to_ascii_lowercase();
    if exchange.is_empty()
        || !exchange
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        anyhow::bail!("invalid exchange: must be non-empty and contain only [a-z0-9_]");
    }
    let exchange_enum: Exchange = exchange.parse()?;
    if req.account_type == "live" {
        reject_if_live_account_exists_for_exchange(pool, &exchange).await?;
    }
    let currency = normalize_currency(&req.currency);
    if let Err(msg) = validate_initial_balance(&currency, req.initial_balance) {
        anyhow::bail!(msg);
    }
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
        .map_err(|e| translate_unique_violation(e, CREATE_ACCOUNT_OPERATION, &exchange))?;
    Ok(TradingAccount::from(row))
}

async fn reject_if_live_account_exists_for_exchange(
    pool: &PgPool,
    exchange: &str,
) -> anyhow::Result<()> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM trading_accounts
         WHERE exchange = $1 AND account_type = 'live' AND active
         LIMIT 1",
    )
    .bind(exchange)
    .fetch_optional(pool)
    .await?;
    if existing.is_some() {
        anyhow::bail!(build_one_live_per_exchange_conflict_message(
            CREATE_ACCOUNT_OPERATION,
            exchange
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests;
