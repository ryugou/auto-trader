use std::sync::Arc;
use std::time::{Duration, Instant};

use auto_trader_core::commission;
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::sfd;
use auto_trader_core::types::*;
use auto_trader_market::bitflyer_private::{
    ChildOrderState, ChildOrderType, Execution, SendChildOrderRequest, Side,
};
use auto_trader_market::exchange_api::{ExchangeApi, StopOrderStatus, weighted_avg_fills};
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::{
    Notifier, NotifyEvent, OrderFailedEvent, OrderFilledEvent, PositionClosedEvent,
};
use chrono::Utc;
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;
use uuid::Uuid;

use crate::position_sizer::PositionSizer;

const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_EXECUTIONS_INITIAL_DELAY: Duration = Duration::from_millis(100);
const POLL_EXECUTIONS_MAX_DELAY: Duration = Duration::from_millis(1600);
const EXECUTIONS_RECONCILE_DELAY: Duration = Duration::from_millis(500);
const POSITION_CUTOFF_CLOCK_SKEW_MARGIN_SECS: i64 = 1;

fn truncate_yen(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

fn aggregate_executions(execs: &[Execution]) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
    weighted_avg_fills(execs.iter().map(|e| (e.price, e.size, e.commission)))
        .map_err(|e| anyhow::anyhow!("failed to aggregate {} executions: {e}", execs.len()))
}

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
    async fn cleanup_unfilled_order(
        &self,
        pair: &str,
        order_id: &str,
        context: &str,
    ) -> anyhow::Error {
        let state = match self.api.get_child_orders(pair, order_id).await {
            Ok(orders) => orders.first().map(|o| o.child_order_state),
            Err(e) => {
                return anyhow::anyhow!(
                    "{context}: failed to get child orders for \"{order_id}\": {e}; may be orphan"
                );
            }
        };

        match state {
            Some(ChildOrderState::Completed) => {
                return anyhow::anyhow!(
                    "{context}: invalid order \"{order_id}\": state=Completed but fill details \
                     could not be aggregated; manual reconciliation required"
                );
            }
            Some(ChildOrderState::Canceled)
            | Some(ChildOrderState::Expired)
            | Some(ChildOrderState::Rejected) => {
                return anyhow::anyhow!(
                    "{context}: order \"{order_id}\" not filled: state={state:?}, no cleanup needed"
                );
            }
            _ => {}
        }

        match self.api.cancel_child_order(pair, order_id).await {
            Ok(_) => {
                anyhow::anyhow!("{context}: order \"{order_id}\" state={state:?}, cancel requested")
            }
            Err(cancel_err) => anyhow::anyhow!(
                "{context}: failed to cancel order \"{order_id}\" (state={state:?}): {cancel_err}; \
                 manual intervention required"
            ),
        }
    }

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

    pub fn with_poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout;
        self
    }

    async fn fill_open(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        if self.dry_run {
            let feed_key = FeedKey::new(self.exchange, signal.pair.clone());
            let (bid, ask) = self
                .price_store
                .latest_bid_ask(&feed_key)
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to get bid/ask for pair \"{}\": no price available",
                        signal.pair
                    )
                })?;
            let price = match signal.direction {
                Direction::Long => ask,
                Direction::Short => bid,
            };
            let commission =
                commission::estimate(commission::Action::Open, self.exchange, price, quantity);
            Ok((price, quantity, commission))
        } else {
            let req = self.signal_to_send_child_order(signal, quantity);
            let resp = self.api.send_child_order(req).await?;
            let order_id = resp.child_order_acceptance_id.clone();
            match self
                .poll_executions(&order_id, &signal.pair.0, self.poll_timeout)
                .await
            {
                Ok((price, qty, commission)) => Ok((price, qty, commission)),
                Err(poll_err) => {
                    tracing::warn!(
                        order_id = %order_id,
                        error = %poll_err,
                        "failed to poll executions; reconciling via exchange"
                    );
                    tokio::time::sleep(EXECUTIONS_RECONCILE_DELAY).await;
                    match self.api.get_executions(&signal.pair.0, &order_id).await {
                        Ok(execs) if !execs.is_empty() => match aggregate_executions(&execs) {
                            Ok((avg_price, total_size, commission)) => {
                                tracing::info!(
                                    order_id = %order_id,
                                    quantity = %total_size,
                                    fill_price = %avg_price,
                                    "fill reconciled after timeout"
                                );
                                Ok((avg_price, total_size, commission))
                            }
                            Err(agg_err) => {
                                tracing::error!(
                                    order_id = %order_id,
                                    error = %agg_err,
                                    "failed to aggregate executions; treating as not filled and attempting cancel"
                                );
                                let cleanup_err = self
                                        .cleanup_unfilled_order(
                                            &signal.pair.0,
                                            &order_id,
                                            &format!(
                                                "failed to aggregate executions for order \"{order_id}\": {agg_err}"
                                            ),
                                        )
                                        .await;
                                Err(anyhow::anyhow!(
                                    "failed to open order \"{order_id}\": aggregate error: {agg_err}; cleanup: {cleanup_err}"
                                ))
                            }
                        },
                        Ok(_) => match self.api.get_child_orders(&signal.pair.0, &order_id).await {
                            Ok(orders) if !orders.is_empty() => {
                                let order = &orders[0];
                                match order.child_order_state {
                                    ChildOrderState::Completed => {
                                        tokio::time::sleep(EXECUTIONS_RECONCILE_DELAY).await;
                                        match self
                                            .api
                                            .get_executions(&signal.pair.0, &order_id)
                                            .await
                                        {
                                            Ok(execs) if !execs.is_empty() => {
                                                let (avg_price, total_size, commission) =
                                                    aggregate_executions(&execs)?;
                                                Ok((avg_price, total_size, commission))
                                            }
                                            _ => {
                                                if order.average_price > Decimal::ZERO
                                                    && order.executed_size > Decimal::ZERO
                                                {
                                                    tracing::warn!(
                                                        order_id = %order_id,
                                                        avg_price = %order.average_price,
                                                        executed_size = %order.executed_size,
                                                        "order Completed but get_executions empty; using order-level data with commission=0"
                                                    );
                                                    Ok((
                                                        order.average_price,
                                                        order.executed_size,
                                                        Decimal::ZERO,
                                                    ))
                                                } else {
                                                    Err(anyhow::anyhow!(
                                                        "invalid order \"{order_id}\": state=Completed but no execution data available; \
                                                             orphan — manual reconciliation required"
                                                    ))
                                                }
                                            }
                                        }
                                    }
                                    ChildOrderState::Active => {
                                        let cleanup_err = self
                                            .cleanup_unfilled_order(
                                                &signal.pair.0,
                                                &order_id,
                                                "invalid order: no executions and still active",
                                            )
                                            .await;
                                        Err(anyhow::anyhow!(
                                            "failed to open order \"{order_id}\": {poll_err}; \
                                                 reconciliation found no executions; cleanup: {cleanup_err}"
                                        ))
                                    }
                                    _ => Err(anyhow::anyhow!(
                                        "invalid order \"{order_id}\": not filled (state={:?})",
                                        order.child_order_state
                                    )),
                                }
                            }
                            Ok(_) => {
                                let cleanup_err = self
                                    .cleanup_unfilled_order(
                                        &signal.pair.0,
                                        &order_id,
                                        "invalid order: no executions and no order record found",
                                    )
                                    .await;
                                Err(anyhow::anyhow!(
                                    "failed to open order \"{order_id}\": {poll_err}; \
                                         reconciliation found no executions or order; cleanup: {cleanup_err}"
                                ))
                            }
                            Err(e) => {
                                let cleanup_err = self
                                    .cleanup_unfilled_order(
                                        &signal.pair.0,
                                        &order_id,
                                        &format!(
                                            "failed to get child orders for \"{order_id}\": {e}"
                                        ),
                                    )
                                    .await;
                                Err(anyhow::anyhow!(
                                    "failed to open order \"{order_id}\": {poll_err}; \
                                         failed to get child orders: {e}; cleanup: {cleanup_err}"
                                ))
                            }
                        },
                        Err(e) => {
                            let cleanup_err = self
                                .cleanup_unfilled_order(
                                    &signal.pair.0,
                                    &order_id,
                                    &format!("failed to get executions for \"{order_id}\": {e}"),
                                )
                                .await;
                            Err(anyhow::anyhow!(
                                "failed to open order \"{order_id}\": {poll_err}; \
                                 failed to get executions: {e}; cleanup: {cleanup_err}"
                            ))
                        }
                    }
                }
            }
        }
    }

    async fn stop_guard_before_close(
        &self,
        trade: &Trade,
    ) -> anyhow::Result<Option<(Decimal, Decimal)>> {
        let Some(stop_id) = &trade.stop_order_id else {
            return Ok(None);
        };
        match self.api.stop_order_status(&trade.pair.0, stop_id).await {
            Ok(StopOrderStatus::Executed { price, commission }) => {
                tracing::info!(
                    stop_id = %stop_id,
                    price = %price,
                    "stop order already executed; recording without new order"
                );
                Ok(Some((price, commission)))
            }
            Ok(StopOrderStatus::Active) => {
                if let Err(cancel_err) = self.api.cancel_stop_order(&trade.pair.0, stop_id).await {
                    match self.api.stop_order_status(&trade.pair.0, stop_id).await {
                        Ok(StopOrderStatus::Executed { price, commission }) => {
                            return Ok(Some((price, commission)));
                        }
                        Ok(StopOrderStatus::Gone) => {
                            tracing::warn!(
                                stop_id = %stop_id,
                                error = %cancel_err,
                                "failed to cancel stop order but stop is now gone; proceeding with market close"
                            );
                        }
                        _ => {
                            anyhow::bail!(
                                "failed to cancel stop order \"{stop_id}\": {cancel_err}; \
                                 status still active/unclear; aborting close to avoid double-close"
                            );
                        }
                    }
                }
                Ok(None)
            }
            Ok(StopOrderStatus::Gone) => {
                tracing::warn!(stop_id = %stop_id, "stop order already gone (canceled/expired)");
                Ok(None)
            }
            Err(e) => {
                anyhow::bail!(
                    "failed to get stop order status for \"{stop_id}\": {e}; aborting close"
                );
            }
        }
    }

    async fn fill_close(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            let feed_key = FeedKey::new(self.exchange, trade.pair.clone());
            let (bid, ask) = self
                .price_store
                .latest_bid_ask(&feed_key)
                .await
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to get bid/ask for pair \"{}\": no price available",
                        trade.pair
                    )
                })?;
            let price = match trade.direction {
                Direction::Long => bid,
                Direction::Short => ask,
            };
            let commission = commission::estimate(
                commission::Action::Close,
                self.exchange,
                price,
                trade.quantity,
            );
            Ok((price, commission))
        } else {
            if let Some(fill) = self.stop_guard_before_close(trade).await? {
                return Ok(fill);
            }
            self.ensure_close_position_id_present(trade)?;
            let req = self.opposite_side_market_order(trade);
            let resp = self.api.send_child_order(req).await?;
            let (price, _qty, commission) = self
                .poll_executions(
                    &resp.child_order_acceptance_id,
                    &trade.pair.0,
                    self.poll_timeout,
                )
                .await?;
            Ok((price, commission))
        }
    }

    async fn poll_executions(
        &self,
        acceptance_id: &str,
        pair_str: &str,
        timeout: Duration,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        let start = Instant::now();
        let mut delay = POLL_EXECUTIONS_INITIAL_DELAY;
        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("failed to poll executions: timed out after {timeout:?}");
            }
            let execs = self.api.get_executions(pair_str, acceptance_id).await?;
            if !execs.is_empty()
                && let Ok(outcome) = aggregate_executions(&execs)
            {
                return Ok(outcome);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(POLL_EXECUTIONS_MAX_DELAY);
        }
    }

    fn signal_to_send_child_order(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> SendChildOrderRequest {
        let side = match signal.direction {
            Direction::Long => Side::Buy,
            Direction::Short => Side::Sell,
        };
        SendChildOrderRequest {
            product_code: signal.pair.0.clone(),
            child_order_type: ChildOrderType::Market,
            side,
            size: quantity,
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        }
    }

    async fn resolve_position_id_with_retry(
        &self,
        product_code: &str,
        after: chrono::DateTime<chrono::Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> Option<String> {
        const MAX_ATTEMPTS: usize = 3;
        const BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);
        for attempt in 1..=MAX_ATTEMPTS {
            match self
                .api
                .resolve_position_id(product_code, after, expected_side, expected_size)
                .await
            {
                Ok(Some(pid)) => return Some(pid),
                Ok(None) => {
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(BACKOFF).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        pair = %product_code,
                        attempt,
                        error = %e,
                        "failed to resolve position id; retrying if attempts remain"
                    );
                    if attempt < MAX_ATTEMPTS {
                        tokio::time::sleep(BACKOFF).await;
                    }
                }
            }
        }
        tracing::warn!(
            pair = %product_code,
            attempts = MAX_ATTEMPTS,
            "failed to resolve position id after all retries"
        );
        None
    }

    fn ensure_close_position_id_present(&self, trade: &Trade) -> anyhow::Result<()> {
        if self.api.requires_close_position_id() && trade.exchange_position_id.is_none() {
            anyhow::bail!(
                "invalid trade \"{}\": exchange_position_id is required by {} but missing \
                 (closing without it would open an opposite position)",
                trade.id,
                self.exchange.as_str()
            );
        }
        Ok(())
    }

    fn opposite_side_market_order(&self, trade: &Trade) -> SendChildOrderRequest {
        let side = match trade.direction {
            Direction::Long => Side::Sell,
            Direction::Short => Side::Buy,
        };
        SendChildOrderRequest {
            product_code: trade.pair.0.clone(),
            child_order_type: ChildOrderType::Market,
            side,
            size: trade.quantity,
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: trade.exchange_position_id.clone(),
        }
    }

    async fn fill_close_with_stale_recovery(
        &self,
        trade: &Trade,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        let positions = self.api.get_positions(&trade.pair.0).await?;

        let mut total_exchange_size = Decimal::ZERO;
        let mut has_any_for_product = false;

        for p in positions
            .iter()
            .filter(|p| p.product_code == trade.pair.0 && p.size > Decimal::ZERO)
        {
            has_any_for_product = true;
            match exchange_side_to_direction(&p.side) {
                Some(d) if d == trade.direction => total_exchange_size += p.size,
                Some(d) => {
                    anyhow::bail!(
                        "invalid position for trade \"{}\": opposite side {:?} vs {:?} (size={}); \
                         refusing auto-recover, manual intervention required",
                        trade.id,
                        d,
                        trade.direction,
                        p.size
                    );
                }
                None => {
                    anyhow::bail!(
                        "unsupported side for trade \"{}\" on {}: \"{}\"; manual intervention required",
                        trade.id,
                        p.product_code,
                        p.side
                    );
                }
            }
        }

        if !has_any_for_product {
            tracing::warn!(
                trade_id = %trade.id,
                "exchange shows trade already closed; completing Phase 3 with best-effort exit price"
            );
            let price = self.resolve_stale_exit_price(trade).await?;
            Ok((price, Decimal::ZERO, true))
        } else if total_exchange_size == trade.quantity {
            tracing::info!(
                trade_id = %trade.id,
                exchange_size = %total_exchange_size,
                "exchange has full size; re-running Phase 2"
            );
            let (price, commission) = self.fill_close(trade).await?;
            Ok((price, commission, false))
        } else if total_exchange_size < trade.quantity {
            tracing::warn!(
                trade_id = %trade.id,
                quantity = %trade.quantity,
                exchange_size = %total_exchange_size,
                "partial fill detected; closing only remaining size to avoid over-close"
            );
            let (price, commission) = self.fill_close_size(trade, total_exchange_size).await?;
            Ok((price, commission, true))
        } else {
            anyhow::bail!(
                "invalid position for trade \"{}\": exchange size {} exceeds trade quantity {}; \
                 refusing to close to avoid oscillation, manual intervention required",
                trade.id,
                total_exchange_size,
                trade.quantity,
            )
        }
    }

    async fn fill_close_size(
        &self,
        trade: &Trade,
        size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            return self.fill_close(trade).await;
        }
        if let Some(fill) = self.stop_guard_before_close(trade).await? {
            return Ok(fill);
        }
        self.ensure_close_position_id_present(trade)?;
        let side = match trade.direction {
            Direction::Long => Side::Sell,
            Direction::Short => Side::Buy,
        };
        let req = SendChildOrderRequest {
            product_code: trade.pair.0.clone(),
            child_order_type: ChildOrderType::Market,
            side,
            size,
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: trade.exchange_position_id.clone(),
        };
        let resp = self.api.send_child_order(req).await?;
        let (price, _qty, commission) = self
            .poll_executions(
                &resp.child_order_acceptance_id,
                &trade.pair.0,
                self.poll_timeout,
            )
            .await?;
        Ok((price, commission))
    }

    async fn resolve_stale_exit_price(&self, trade: &Trade) -> anyhow::Result<Decimal> {
        let feed_key = FeedKey::new(self.exchange, trade.pair.clone());
        match self.price_store.latest_bid_ask(&feed_key).await {
            Some((bid, ask)) => {
                let mid = (bid + ask) / Decimal::from(2);
                tracing::warn!(
                    trade_id = %trade.id,
                    exit_price = %mid,
                    "using PriceStore mid as exit price"
                );
                Ok(mid)
            }
            None => {
                tracing::warn!(
                    trade_id = %trade.id,
                    pair = %trade.pair,
                    exit_price = %trade.entry_price,
                    "no PriceStore data; using entry_price as approximate exit price"
                );
                Ok(trade.entry_price)
            }
        }
    }

    async fn snapshot_live_sfd_before_close(&self, trade: &Trade) -> Decimal {
        match self.api.fetch_close_sfd(&trade.pair.0).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    trade_id = %trade.id,
                    pair = %trade.pair,
                    error = %e,
                    "failed to fetch close sfd; treating sfd as 0 to avoid blocking close"
                );
                Decimal::ZERO
            }
        }
    }
}

fn exchange_side_to_direction(side: &str) -> Option<Direction> {
    match side.trim().to_ascii_uppercase().as_str() {
        "BUY" => Some(Direction::Long),
        "SELL" => Some(Direction::Short),
        _ => None,
    }
}

impl OrderExecutor for Trader {
    async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        let account = auto_trader_db::trading_accounts::get_account(&self.pool, self.account_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "failed to load trading account \"{}\": not found",
                    self.account_id
                )
            })?;

        let balance = account.current_balance;
        let leverage = account.leverage;

        if leverage <= Decimal::ZERO {
            anyhow::bail!(
                "invalid account \"{}\": non-positive leverage {leverage}, refusing to open trade",
                self.account_id
            );
        }

        let sizer = &self.position_sizer;

        let feed_key = FeedKey::new(self.exchange, signal.pair.clone());
        let hint_price = {
            if let Some((bid, ask)) = self.price_store.latest_bid_ask(&feed_key).await {
                match signal.direction {
                    Direction::Long => ask,
                    Direction::Short => bid,
                }
            } else {
                anyhow::bail!(
                    "failed to get bid/ask for pair \"{}\": no price available",
                    signal.pair
                );
            }
        };

        let quantity = sizer
            .calculate_quantity(
                &signal.pair,
                balance,
                hint_price,
                leverage,
                signal.allocation_pct,
                signal.stop_loss_pct,
                self.liquidation_margin_level,
            )
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid quantity for pair \"{}\": balance too small to open minimum order",
                    signal.pair
                )
            })?;

        let position_cutoff =
            Utc::now() - chrono::Duration::seconds(POSITION_CUTOFF_CLOCK_SKEW_MARGIN_SECS);

        let (fill_price, actual_qty, open_commission) = self.fill_open(signal, quantity).await?;

        let stop_loss = match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE - signal.stop_loss_pct),
            Direction::Short => fill_price * (Decimal::ONE + signal.stop_loss_pct),
        };
        let take_profit = signal.take_profit_pct.map(|pct| match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE + pct),
            Direction::Short => fill_price * (Decimal::ONE - pct),
        });

        let entry_at = Utc::now();
        let margin = truncate_yen(fill_price * actual_qty / leverage);

        let order_side = match signal.direction {
            Direction::Long => Side::Buy,
            Direction::Short => Side::Sell,
        };
        let exchange_position_id = if self.dry_run || !self.api.requires_close_position_id() {
            None
        } else {
            self.resolve_position_id_with_retry(
                &signal.pair.0,
                position_cutoff,
                order_side,
                actual_qty,
            )
            .await
        };
        if !self.dry_run && self.api.requires_close_position_id() && exchange_position_id.is_none()
        {
            let notifier = self.notifier.clone();
            let ev = NotifyEvent::OrderFailed(OrderFailedEvent {
                account_name: self.account_name.clone(),
                exchange: self.exchange,
                strategy_name: signal.strategy_name.clone(),
                pair: signal.pair.clone(),
                reason: format!(
                    "failed to resolve exchange_position_id for \"{}\" after retries; \
                     trade will be unclosable until reconciled",
                    signal.pair.0
                ),
            });
            tokio::spawn(async move {
                if let Err(e) = notifier.send(ev).await {
                    tracing::error!(error = %e, "failed to send unresolved-position-id alert");
                }
            });
        }

        let stop_order_id = if self.dry_run {
            None
        } else {
            let close_side = match signal.direction {
                Direction::Long => Side::Sell,
                Direction::Short => Side::Buy,
            };
            let trigger =
                self.position_sizer
                    .round_trigger_price(&signal.pair, stop_loss, signal.direction);
            match self
                .api
                .place_stop_order(
                    &signal.pair.0,
                    close_side,
                    actual_qty,
                    trigger,
                    exchange_position_id.as_deref(),
                )
                .await
            {
                Ok(id) => Some(id),
                Err(e) => {
                    tracing::error!(
                        pair = %signal.pair,
                        error = %e,
                        "failed to place stop order; position is unprotected at the exchange (app-side SL monitoring is the only stop)"
                    );
                    let notifier = self.notifier.clone();
                    let ev = NotifyEvent::OrderFailed(OrderFailedEvent {
                        account_name: self.account_name.clone(),
                        exchange: self.exchange,
                        strategy_name: signal.strategy_name.clone(),
                        pair: signal.pair.clone(),
                        reason: format!(
                            "failed to place exchange-side stop order: {e} (position unprotected if app dies)"
                        ),
                    });
                    tokio::spawn(async move {
                        if let Err(e) = notifier.send(ev).await {
                            tracing::warn!(error = %e, "failed to send stop-order-failure alert");
                        }
                    });
                    None
                }
            }
        };

        let trade = Trade {
            id: Uuid::new_v4(),
            account_id: self.account_id,
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: fill_price,
            exit_price: None,
            stop_loss,
            take_profit,
            quantity: actual_qty,
            leverage,
            fees: open_commission,
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
            exchange_position_id,
            stop_order_id,
        };

        let db_result = async {
            let mut tx = self.pool.begin().await?;
            auto_trader_db::trades::insert_trade(&mut *tx, &trade).await?;
            auto_trader_db::trades::lock_margin(&mut tx, self.account_id, trade.id, margin).await?;
            auto_trader_db::notifications::insert_trade_opened(&mut *tx, &trade).await?;
            tx.commit().await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(ref e) = db_result {
            if self.dry_run {
                tracing::warn!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    fill_price = %fill_price,
                    error = %e,
                    "failed to write simulated trade open to DB in dry_run; no exchange impact"
                );
            } else {
                tracing::error!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    fill_price = %fill_price,
                    error = %e,
                    "failed to write trade open to DB after exchange fill; inconsistent state, \
                     orphan exchange position requires manual reconciliation"
                );
                let notifier = self.notifier.clone();
                let account_name = self.account_name.clone();
                let exchange = self.exchange;
                let pair = trade.pair.clone();
                let strategy_name = trade.strategy_name.clone();
                let reason = format!(
                    "failed to commit trade open: {e}; exchange filled but DB write failed, \
                     orphan position requires manual reconciliation"
                );
                tokio::spawn(async move {
                    let ev = auto_trader_notify::NotifyEvent::OrderFailed(
                        auto_trader_notify::OrderFailedEvent {
                            account_name,
                            exchange,
                            strategy_name,
                            pair,
                            reason,
                        },
                    );
                    if let Err(notify_err) = notifier.send(ev).await {
                        tracing::warn!(error = %notify_err, "failed to send critical notification");
                    }
                });
            }
            return Err(db_result.unwrap_err());
        }

        let notifier = self.notifier.clone();
        let account_name = self.account_name.clone();
        let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
            account_name,
            exchange: self.exchange,
            trade_id: trade.id,
            pair: trade.pair.clone(),
            direction: trade.direction,
            quantity: actual_qty,
            price: fill_price,
            at: entry_at,
        });
        tokio::spawn(async move {
            if let Err(e) = notifier.send(ev).await {
                tracing::warn!(error = %e, "failed to send slack notification");
            }
        });

        tracing::info!(
            strategy_name = %trade.strategy_name,
            pair = %trade.pair,
            direction = ?trade.direction,
            fill_price = %fill_price,
            quantity = %actual_qty,
            margin_locked = %margin,
            dry_run = self.dry_run,
            "opened trade"
        );

        Ok(trade)
    }

    async fn close_position(&self, id: &str, exit_reason: ExitReason) -> anyhow::Result<Trade> {
        let uuid = Uuid::parse_str(id)?;

        let lock = auto_trader_db::trades::acquire_close_lock(
            &self.pool,
            uuid,
            self.account_id,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "invalid trade \"{id}\": not in 'open' state (already closed/closing or belongs to another account)"
            )
        })?;
        let trade = lock.trade;
        let was_stale_recovery = lock.was_stale_recovery;

        let live_sfd_snapshot: Option<Decimal> = if self.dry_run {
            None
        } else {
            Some(self.snapshot_live_sfd_before_close(&trade).await)
        };

        let mut stale_approximate = false;
        let exit_result: anyhow::Result<(Decimal, Decimal)> = if !self.dry_run && was_stale_recovery
        {
            tracing::warn!(
                trade_id = %trade.id,
                "stale-lock recovery; verifying exchange state before Phase 2"
            );
            match self.fill_close_with_stale_recovery(&trade).await {
                Ok((price, commission, approximate)) => {
                    stale_approximate = approximate;
                    Ok((price, commission))
                }
                Err(e) => Err(e),
            }
        } else {
            self.fill_close(&trade).await
        };
        let (exit_price, close_commission) = match exit_result {
            Ok(pair) => pair,
            Err(e) => {
                if let Err(release_err) =
                    auto_trader_db::trades::release_close_lock(&self.pool, uuid).await
                {
                    tracing::error!(
                        trade_id = %uuid,
                        original_error = %e,
                        release_error = %release_err,
                        "failed to release close lock after fill_close failed; \
                         trade is stuck in 'closing' status, manual intervention required"
                    );
                }
                return Err(e);
            }
        };

        let sfd_accrued = live_sfd_snapshot
            .unwrap_or_else(|| sfd::estimate(self.exchange, exit_price, trade.quantity));

        let price_diff = match trade.direction {
            Direction::Long => exit_price - trade.entry_price,
            Direction::Short => trade.entry_price - exit_price,
        };
        let pnl_amount = truncate_yen(price_diff * trade.quantity);
        let exit_at = Utc::now();

        let closed_trade = Trade {
            id: trade.id,
            account_id: trade.account_id,
            strategy_name: trade.strategy_name.clone(),
            pair: trade.pair.clone(),
            exchange: trade.exchange,
            direction: trade.direction,
            entry_price: trade.entry_price,
            exit_price: Some(exit_price),
            stop_loss: trade.stop_loss,
            take_profit: trade.take_profit,
            quantity: trade.quantity,
            leverage: trade.leverage,
            fees: trade.fees + close_commission + sfd_accrued,
            entry_at: trade.entry_at,
            exit_at: Some(exit_at),
            pnl_amount: Some(pnl_amount),
            exit_reason: Some(exit_reason),
            status: TradeStatus::Closed,
            max_hold_until: trade.max_hold_until,
            exchange_position_id: trade.exchange_position_id.clone(),
            stop_order_id: trade.stop_order_id.clone(),
        };

        let margin = truncate_yen(trade.entry_price * trade.quantity / trade.leverage);
        let phase3_result = async {
            let mut tx = self.pool.begin().await?;
            auto_trader_db::trades::update_trade_closed(
                &mut *tx,
                trade.id,
                exit_price,
                exit_at,
                pnl_amount,
                exit_reason,
                closed_trade.fees,
            )
            .await?;

            auto_trader_db::trades::release_margin(
                &mut tx,
                self.account_id,
                trade.id,
                margin,
                pnl_amount,
            )
            .await?;

            auto_trader_db::notifications::insert_trade_closed(&mut *tx, &closed_trade).await?;
            tx.commit().await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(ref e) = phase3_result {
            if self.dry_run {
                tracing::warn!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    exit_price = %exit_price,
                    error = %e,
                    "failed to write simulated trade close to DB in dry_run; \
                     trade remains in 'closing', no exchange impact, stale-lock \
                     self-healing will eventually free it"
                );
            } else {
                tracing::error!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    exit_price = %exit_price,
                    error = %e,
                    "failed to write trade close to DB after exchange fill; inconsistent state, \
                     trade remains in 'closing' status, stale-lock self-healing or PR-2 \
                     reconciler will pick this up"
                );
                let notifier = self.notifier.clone();
                let account_name = self.account_name.clone();
                let exchange = self.exchange;
                let strategy_name = trade.strategy_name.clone();
                let pair = trade.pair.clone();
                let trade_id = trade.id;
                let reason = format!(
                    "failed to commit trade close for \"{trade_id}\": {e}; \
                     trade stuck in 'closing' status"
                );
                tokio::spawn(async move {
                    let ev = auto_trader_notify::NotifyEvent::OrderFailed(
                        auto_trader_notify::OrderFailedEvent {
                            account_name,
                            exchange,
                            strategy_name,
                            pair,
                            reason,
                        },
                    );
                    if let Err(notify_err) = notifier.send(ev).await {
                        tracing::warn!(error = %notify_err, "failed to send critical notification");
                    }
                });
            }
            return Err(phase3_result.unwrap_err());
        }

        let notifier = self.notifier.clone();
        let trade_id = closed_trade.id;
        let account_name = self.account_name.clone();
        let ev = NotifyEvent::PositionClosed(PositionClosedEvent {
            account_name: account_name.clone(),
            exchange: self.exchange,
            trade_id,
            pnl_amount,
            reason: exit_reason.as_str().to_owned(),
        });
        tokio::spawn(async move {
            if let Err(e) = notifier.send(ev).await {
                tracing::warn!(error = %e, "failed to send slack notification");
            }
        });

        if stale_approximate {
            let notifier = self.notifier.clone();
            let exchange = self.exchange;
            let pair = closed_trade.pair.clone();
            let strategy_name = closed_trade.strategy_name.clone();
            let reason = format!(
                "invalid close for trade \"{trade_id}\": stale-recovery reconciliation used an \
                 approximation; exit price and/or pnl attribution may be inaccurate, manual audit \
                 against exchange records recommended"
            );
            tokio::spawn(async move {
                let ev = auto_trader_notify::NotifyEvent::OrderFailed(
                    auto_trader_notify::OrderFailedEvent {
                        account_name,
                        exchange,
                        strategy_name,
                        pair,
                        reason,
                    },
                );
                if let Err(notify_err) = notifier.send(ev).await {
                    tracing::warn!(error = %notify_err, "failed to send stale-recovery alert");
                }
            });
        }

        tracing::info!(
            strategy_name = %closed_trade.strategy_name,
            pair = %closed_trade.pair,
            pnl_amount = %pnl_amount,
            exit_reason = ?exit_reason,
            dry_run = self.dry_run,
            "closed trade"
        );

        Ok(closed_trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let trades =
            auto_trader_db::trades::get_open_trades_by_account(&self.pool, self.account_id).await?;
        Ok(trades.into_iter().map(|trade| Position { trade }).collect())
    }
}
