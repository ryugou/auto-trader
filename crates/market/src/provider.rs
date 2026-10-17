use auto_trader_core::types::{Candle, Pair};
use rust_decimal::Decimal;

/// Source of candles and latest prices.
#[async_trait::async_trait]
pub trait MarketDataProvider: Send + Sync {
    /// Returns the candles of `pair` for `timeframe` among the latest `count`.
    async fn get_candles(
        &self,
        pair: &Pair,
        timeframe: &str,
        count: u32,
    ) -> anyhow::Result<Vec<Candle>>;
    /// Returns the latest price of `pair`.
    async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal>;
}
