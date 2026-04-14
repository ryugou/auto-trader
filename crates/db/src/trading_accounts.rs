//! Trading account DB access.
//!
//! NOTE: This module is a stub. Real implementation is delivered in PR-1 Task 6.
//! Functions here return `unimplemented!()` to allow the executor crate to compile
//! while the full DB schema migration (Task 6) is pending.

use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Unified account row (replaces PaperAccount).
///
/// Mirrors the `trading_accounts` table created in the Task 1 migration.
#[derive(Debug, Clone)]
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
}

/// Fetch a single account by id.
///
/// # Panics (temporary)
///
/// This is a stub that always panics with `unimplemented!`.
/// It will be replaced by a real SQL query in PR-1 Task 6.
pub async fn get_account(
    _pool: &PgPool,
    _id: Uuid,
) -> anyhow::Result<Option<TradingAccount>> {
    unimplemented!("implemented in PR-1 Task 6 — trading_accounts schema not yet available");
}

/// List all accounts ordered by created_at.
///
/// # Panics (temporary)
pub async fn list_all(_pool: &PgPool) -> anyhow::Result<Vec<TradingAccount>> {
    unimplemented!("implemented in PR-1 Task 6 — trading_accounts schema not yet available");
}
