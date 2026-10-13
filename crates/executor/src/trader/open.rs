use std::time::{Duration, Instant};

use auto_trader_core::commission;
use auto_trader_core::types::{Direction, Signal};
use auto_trader_market::bitflyer_private::{
    ChildOrder, ChildOrderState, ChildOrderType, Execution, SendChildOrderRequest, Side,
};
use auto_trader_market::exchange_api::weighted_avg_fills;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use super::Trader;
use super::order_side::opening_side;

const POLL_EXECUTIONS_INITIAL_DELAY: Duration = Duration::from_millis(100);
const POLL_EXECUTIONS_MAX_DELAY: Duration = Duration::from_millis(1600);
const EXECUTIONS_RECONCILE_DELAY: Duration = Duration::from_millis(500);

fn aggregate_executions(execs: &[Execution]) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
    weighted_avg_fills(execs.iter().map(|e| (e.price, e.size, e.commission)))
        .map_err(|e| anyhow::anyhow!("failed to aggregate {} executions: {e}", execs.len()))
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

    pub(super) async fn fill_open(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        if self.dry_run {
            let (bid, ask) = self.latest_bid_ask(&signal.pair).await?;
            let price = match signal.direction {
                Direction::Long => ask,
                Direction::Short => bid,
            };
            let commission =
                commission::estimate(commission::Action::Open, self.exchange, price, quantity);
            Ok((price, quantity, commission))
        } else {
            self.fill_open_live(signal, quantity).await
        }
    }

    async fn fill_open_live(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
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
                self.reconcile_unpolled_open(signal, &order_id, poll_err)
                    .await
            }
        }
    }

    async fn reconcile_unpolled_open(
        &self,
        signal: &Signal,
        order_id: &str,
        poll_err: anyhow::Error,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        match self.api.get_executions(&signal.pair.0, order_id).await {
            Ok(execs) if !execs.is_empty() => {
                self.finalize_reconciled_fill(&signal.pair.0, order_id, &execs)
                    .await
            }
            Ok(_) => {
                self.reconcile_via_child_order_lookup(signal, order_id, &poll_err)
                    .await
            }
            Err(e) => {
                let cleanup_err = self
                    .cleanup_unfilled_order(
                        &signal.pair.0,
                        order_id,
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

    async fn finalize_reconciled_fill(
        &self,
        pair: &str,
        order_id: &str,
        execs: &[Execution],
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        match aggregate_executions(execs) {
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
                        pair,
                        order_id,
                        &format!(
                            "failed to aggregate executions for order \"{order_id}\": {agg_err}"
                        ),
                    )
                    .await;
                Err(anyhow::anyhow!(
                    "failed to open order \"{order_id}\": aggregate error: {agg_err}; cleanup: {cleanup_err}"
                ))
            }
        }
    }

    async fn reconcile_via_child_order_lookup(
        &self,
        signal: &Signal,
        order_id: &str,
        poll_err: &anyhow::Error,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        match self.api.get_child_orders(&signal.pair.0, order_id).await {
            Ok(orders) if !orders.is_empty() => {
                self.reconcile_from_child_order(&signal.pair.0, order_id, poll_err, &orders[0])
                    .await
            }
            Ok(_) => {
                let cleanup_err = self
                    .cleanup_unfilled_order(
                        &signal.pair.0,
                        order_id,
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
                        order_id,
                        &format!("failed to get child orders for \"{order_id}\": {e}"),
                    )
                    .await;
                Err(anyhow::anyhow!(
                    "failed to open order \"{order_id}\": {poll_err}; \
                     failed to get child orders: {e}; cleanup: {cleanup_err}"
                ))
            }
        }
    }

    async fn reconcile_from_child_order(
        &self,
        pair: &str,
        order_id: &str,
        poll_err: &anyhow::Error,
        order: &ChildOrder,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        match order.child_order_state {
            ChildOrderState::Completed => {
                tokio::time::sleep(EXECUTIONS_RECONCILE_DELAY).await;
                match self.api.get_executions(pair, order_id).await {
                    Ok(execs) if !execs.is_empty() => {
                        let (avg_price, total_size, commission) = aggregate_executions(&execs)?;
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
                            Ok((order.average_price, order.executed_size, Decimal::ZERO))
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
                        pair,
                        order_id,
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

    pub(super) async fn poll_executions(
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
        SendChildOrderRequest {
            product_code: signal.pair.0.clone(),
            child_order_type: ChildOrderType::Market,
            side: opening_side(signal.direction),
            size: quantity,
            price: None,
            minute_to_expire: None,
            time_in_force: None,
            close_position_id: None,
        }
    }

    pub(super) async fn resolve_position_id_with_retry(
        &self,
        product_code: &str,
        after: DateTime<Utc>,
        expected_side: Side,
        expected_size: Decimal,
    ) -> Option<String> {
        const MAX_ATTEMPTS: usize = 3;
        const BACKOFF: Duration = Duration::from_millis(500);
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
}
