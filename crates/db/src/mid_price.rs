//! Trait for mid-price lookup, used by `trades::sum_unrealized_pnl_for_account`.
//!
//! Defined here (in `auto-trader-db`) so the DB crate can use it without
//! depending on `auto-trader-market`, which would create a circular dependency
//! (`auto-trader-market` already depends on `auto-trader-db`).

use auto_trader_core::types::Pair;
use async_trait::async_trait;
use rust_decimal::Decimal;

/// Provides mid-price for a given trading pair.
///
/// Implementors: `Arc<auto_trader_market::price_store::PriceStore>`.
#[async_trait]
pub trait MidPriceSource: Send + Sync {
    async fn mid(&self, pair: &Pair) -> Option<Decimal>;
}
