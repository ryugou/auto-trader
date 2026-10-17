mod close;
mod latest_price;
mod notification;
mod open;
mod order_executor;
mod order_side;

use std::sync::Arc;
use std::time::Duration;

use auto_trader_core::types::Exchange;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::position_sizer::PositionSizer;

const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// Trader that places and closes orders for one account on one exchange.
pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    account_name: String,
    api: Arc<dyn ExchangeApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    position_sizer: Arc<PositionSizer>,
    liquidation_margin_level: Decimal,
    dry_run: bool,
    poll_timeout: Duration,
}

impl Trader {
    /// Creates a trader for the account, with the default poll timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        exchange: Exchange,
        account_id: Uuid,
        account_name: String,
        api: Arc<dyn ExchangeApi>,
        price_store: Arc<PriceStore>,
        notifier: Arc<Notifier>,
        position_sizer: Arc<PositionSizer>,
        liquidation_margin_level: Decimal,
        dry_run: bool,
    ) -> Self {
        Self {
            pool,
            exchange,
            account_id,
            account_name,
            api,
            price_store,
            notifier,
            position_sizer,
            liquidation_margin_level,
            dry_run,
            poll_timeout: DEFAULT_POLL_TIMEOUT,
        }
    }

    /// Sets how long to wait for an order's executions to be reported.
    pub fn with_poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout;
        self
    }
}
