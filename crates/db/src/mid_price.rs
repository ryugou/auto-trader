//! Trait for mid-price lookup, used by `trades::sum_unrealized_pnl_for_account`.
//!
//! Defined here (in `auto-trader-db`) so the DB crate can use it without
//! depending on `auto-trader-market`, which would create a circular dependency
//! (`auto-trader-market` already depends on `auto-trader-db`).

use async_trait::async_trait;
use auto_trader_core::types::Pair;
use rust_decimal::Decimal;

/// Provides mid-price and tick-age for a given trading pair.
///
/// Implementors: `Arc<auto_trader_market::price_store::PriceStore>`.
#[async_trait]
pub trait MidPriceSource: Send + Sync {
    async fn mid(&self, pair: &Pair) -> Option<Decimal>;

    /// Return the age (in seconds) of the most-recent tick for this pair
    /// across all exchanges. Returns `None` when no tick has been observed.
    async fn last_tick_age(&self, pair: &Pair) -> Option<u64>;
}
