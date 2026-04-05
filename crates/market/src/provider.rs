use auto_trader_core::types::{Candle, Pair};
use rust_decimal::Decimal;

#[async_trait::async_trait]
pub trait MarketDataProvider: Send + Sync {
    async fn get_candles(
        &self,
        pair: &Pair,
        timeframe: &str,
        count: u32,
    ) -> anyhow::Result<Vec<Candle>>;
    async fn get_latest_price(&self, pair: &Pair) -> anyhow::Result<Decimal>;
}
