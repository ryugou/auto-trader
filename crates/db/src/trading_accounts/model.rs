use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

/// Row of the `trading_accounts` table: a trading account and its balance.
#[derive(Debug, Clone, Serialize)]
pub struct TradingAccount {
    /// Identifier of the account.
    pub id: Uuid,
    /// Name of the account.
    pub name: String,
    /// Kind of the account, either `paper` or `live`.
    pub account_type: String,
    /// Exchange the account trades on.
    pub exchange: String,
    /// Strategy the account runs.
    pub strategy: String,
    /// Balance the account started with.
    pub initial_balance: Decimal,
    /// Balance of the account now.
    pub current_balance: Decimal,
    /// Leverage of the account.
    pub leverage: Decimal,
    /// Currency of the balances.
    pub currency: String,
    /// Time the account was created.
    pub created_at: DateTime<Utc>,
    /// Whether the account is active.
    pub active: bool,
}

#[derive(sqlx::FromRow)]
pub(super) struct AccountRow {
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

pub(super) const ACCOUNT_COLUMNS: &str = "id, name, account_type, exchange, strategy, \
                                initial_balance, current_balance, leverage, currency, created_at, active";
