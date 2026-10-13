use auto_trader_core::types::Pair;
use auto_trader_market::price_store::FeedKey;
use rust_decimal::Decimal;

use super::Trader;

impl Trader {
    pub(super) async fn latest_bid_ask(&self, pair: &Pair) -> anyhow::Result<(Decimal, Decimal)> {
        self.price_store
            .latest_bid_ask(&FeedKey::new(self.exchange, pair.clone()))
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("failed to get bid/ask for pair \"{pair}\": no price available")
            })
    }
}
