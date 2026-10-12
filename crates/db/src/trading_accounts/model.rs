use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

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
