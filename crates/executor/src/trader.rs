//! Unified Trader — serves both paper and live accounts.
//!
//! The only difference between paper and live:
//!   dry_run == true  → fill price from local PriceStore (bid/ask)
//!                      no bitFlyer API call
//!   dry_run == false → fill price from bitFlyer get_executions
//!                      actual order placed via send_child_order
//!
//! Everything else — DB writes, balance management, margin lock,
//! pnl computation, notifications — is identical.

use std::sync::Arc;
use std::time::{Duration, Instant};

use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::*;
use auto_trader_market::bitflyer_private::{
    ChildOrderState, ChildOrderType, Execution, SendChildOrderRequest, Side,
};
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::{
    Notifier, NotifyEvent, OrderFailedEvent, OrderFilledEvent, PositionClosedEvent,
};
use chrono::Utc;
use rust_decimal::{Decimal, RoundingStrategy};
use sqlx::PgPool;
use uuid::Uuid;

use crate::position_sizer::PositionSizer;

/// Truncate a yen amount toward zero to whole yen. All yen-denominated
/// figures written to the DB (balance, pnl, fees, margin) go through
/// this helper so the ledger never carries fractional yen.
fn truncate_yen(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(0, RoundingStrategy::ToZero)
}

/// Aggregate a non-empty execution list into
/// (volume-weighted avg price, total size, total commission).
///
/// Used when `poll_executions` timed out but a follow-up `get_executions` call
/// confirmed the order did fill. Returns an error if the executions are empty
/// or total size is zero (caller should have guarded against the empty case).
fn aggregate_executions(execs: &[Execution]) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
    let total_size: Decimal = execs.iter().map(|e| e.size).sum();
    if total_size.is_zero() {
        anyhow::bail!(
            "aggregate_executions: total size is zero across {} execs",
            execs.len()
        );
    }
    let total_notional: Decimal = execs.iter().map(|e| e.price * e.size).sum();
    let total_commission: Decimal = execs.iter().map(|e| e.commission).sum();
    Ok((total_notional / total_size, total_size, total_commission))
}

pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    /// Cached at construction time so every Slack notification shows the
    /// human-readable account name rather than the raw UUID.
    account_name: String,
    api: Arc<dyn ExchangeApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    /// Shared PositionSizer — pre-built at startup, every per-tick task
    /// holds an `Arc::clone` instead of reconstructing the inner
    /// HashMap on every signal/SL/TP check.
    position_sizer: Arc<PositionSizer>,
    /// Broker liquidation threshold (証拠金維持率の下限). Resolved from
    /// `[exchange_margin.<exchange>]` at startup and held per-Trader.
    liquidation_margin_level: Decimal,
    dry_run: bool,
    /// Timeout passed to `poll_executions` for both open and close fills.
    /// Defaults to 5 s in production; can be shortened in tests via
    /// `with_poll_timeout`.
    poll_timeout: Duration,
}

impl Trader {
    /// Attempt to cancel an order that did not fill, then return the
    /// diagnostic error. Always returns `Err`.
    ///
    /// Checks the current order state first so we only call cancel on orders
    /// that are still active. Called from both the empty-execs path and the
    /// aggregate-error path in `fill_open`.
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
                    "{context}: get_child_orders failed for order {order_id}: {e}; may be orphan"
                );
            }
        };

        match state {
            Some(ChildOrderState::Completed) => {
                return anyhow::anyhow!(
                    "{context}: order {order_id} state=Completed — order filled but fill details \
                     could not be aggregated; manual reconciliation required"
                );
            }
            Some(ChildOrderState::Canceled)
            | Some(ChildOrderState::Expired)
            | Some(ChildOrderState::Rejected) => {
                return anyhow::anyhow!(
                    "{context}: order {order_id} state={:?} (not filled, no cleanup needed)",
                    state
                );
            }
            _ => {
                // Active/Unknown — attempt cancel
            }
        }

        match self.api.cancel_child_order(pair, order_id).await {
            Ok(_) => anyhow::anyhow!(
                "{context}: order {order_id} state={:?} (cancel requested)",
                state
            ),
            Err(cancel_err) => anyhow::anyhow!(
                "{context}: order {order_id} state={:?} — CANCEL ATTEMPT FAILED: {cancel_err}; MANUAL INTERVENTION MAY BE REQUIRED",
                state
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
            poll_timeout: Duration::from_secs(5),
        }
    }

    /// Override the poll timeout used by `fill_open` and `fill_close`.
    /// Primarily used in integration tests to avoid 5 s waits.
    pub fn with_poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout;
        self
    }

    /// fill_open: signal → 約定価格 + 実数量 + commission
    ///
    /// - dry_run=true: PriceStore から Long=ask / Short=bid、commission は estimate_open
    /// - dry_run=false: send_child_order → poll_executions、commission は約定の合計
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
                .ok_or_else(|| anyhow::anyhow!("no bid/ask available for {}", signal.pair))?;
            let price = match signal.direction {
                Direction::Long => ask,
                Direction::Short => bid,
            };
            let commission =
                auto_trader_core::commission::estimate_open(self.exchange, price, quantity);
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
                    // poll_executions failed — the order may or may not have filled at the exchange.
                    // Consult the exchange before giving up to avoid creating an
                    // orphan position that never gets recorded in the DB.
                    tracing::warn!(
                        "open fill poll_executions failed for order {}: {poll_err}; reconciling via exchange",
                        order_id
                    );
                    // One additional attempt after a short pause.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    match self.api.get_executions(&signal.pair.0, &order_id).await {
                        Ok(execs) if !execs.is_empty() => {
                            // Fill did happen — aggregate to get avg price + total size.
                            match aggregate_executions(&execs) {
                                Ok((avg_price, total_size, commission)) => {
                                    tracing::info!(
                                        "open fill reconciled after timeout: order {} filled {} @ {}",
                                        order_id,
                                        total_size,
                                        avg_price
                                    );
                                    Ok((avg_price, total_size, commission))
                                }
                                Err(agg_err) => {
                                    // aggregate failed (e.g. all size=0) — fall through to
                                    // cancel-cleanup path so the order is not left active.
                                    tracing::error!(
                                        "open fill: aggregate_executions failed for order {}: {agg_err}; \
                                         treating as not-filled and attempting cancel",
                                        order_id
                                    );
                                    let cleanup_err = self
                                        .cleanup_unfilled_order(
                                            &signal.pair.0,
                                            &order_id,
                                            &format!(
                                                "open fill failed (aggregate error: {agg_err})"
                                            ),
                                        )
                                        .await;
                                    Err(anyhow::anyhow!(
                                        "open fill aggregate error for order {order_id}: {agg_err}; cleanup: {cleanup_err}"
                                    ))
                                }
                            }
                        }
                        Ok(_) => {
                            // No executions — check order state before deciding how to handle.
                            match self.api.get_child_orders(&signal.pair.0, &order_id).await {
                                Ok(orders) if !orders.is_empty() => {
                                    let order = &orders[0];
                                    match order.child_order_state {
                                        ChildOrderState::Completed => {
                                            // Order IS filled. get_executions was empty — likely a
                                            // transient API lag. Retry once after a short pause;
                                            // if still empty, fall back to order-level avg_price.
                                            tokio::time::sleep(Duration::from_millis(500)).await;
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
                                                    // Fall back to order-level data.
                                                    // commission は execution レベルでしか取得できないので 0。
                                                    if order.average_price > Decimal::ZERO
                                                        && order.executed_size > Decimal::ZERO
                                                    {
                                                        tracing::warn!(
                                                            "open fill: order {} Completed but get_executions empty; \
                                                             using order-level avg_price={} executed_size={}, commission=0",
                                                            order_id,
                                                            order.average_price,
                                                            order.executed_size
                                                        );
                                                        Ok((
                                                            order.average_price,
                                                            order.executed_size,
                                                            Decimal::ZERO,
                                                        ))
                                                    } else {
                                                        Err(anyhow::anyhow!(
                                                            "open fill: order {} Completed but no execution data available; \
                                                             ORPHAN — manual reconciliation required",
                                                            order_id
                                                        ))
                                                    }
                                                }
                                            }
                                        }
                                        ChildOrderState::Active => {
                                            // Not terminal — attempt cancel.
                                            let cleanup_err = self
                                                .cleanup_unfilled_order(
                                                    &signal.pair.0,
                                                    &order_id,
                                                    "open fill failed (no executions, order active)",
                                                )
                                                .await;
                                            Err(anyhow::anyhow!(
                                                "open fill: poll_executions failed for order {order_id}: {poll_err}; \
                                                 reconciliation found no executions; cleanup: {cleanup_err}"
                                            ))
                                        }
                                        _ => {
                                            // Canceled / Expired / Rejected — terminal, not filled.
                                            Err(anyhow::anyhow!(
                                                "open fill: order {order_id} state={:?} (terminal, not filled)",
                                                order.child_order_state
                                            ))
                                        }
                                    }
                                }
                                Ok(_) => {
                                    // No order record found — unexpected. Attempt cancel anyway.
                                    let cleanup_err = self
                                        .cleanup_unfilled_order(
                                            &signal.pair.0,
                                            &order_id,
                                            "open fill failed (no executions, no order found)",
                                        )
                                        .await;
                                    Err(anyhow::anyhow!(
                                        "open fill: poll_executions failed for order {order_id}: {poll_err}; \
                                         reconciliation found no executions or order; cleanup: {cleanup_err}"
                                    ))
                                }
                                Err(e) => {
                                    let cleanup_err = self
                                        .cleanup_unfilled_order(
                                            &signal.pair.0,
                                            &order_id,
                                            &format!(
                                                "open fill failed (get_child_orders error: {e})"
                                            ),
                                        )
                                        .await;
                                    Err(anyhow::anyhow!(
                                        "open fill: poll_executions failed for order {order_id}: {poll_err}; \
                                         get_child_orders error: {e}; cleanup: {cleanup_err}"
                                    ))
                                }
                            }
                        }
                        Err(e) => {
                            let cleanup_err = self
                                .cleanup_unfilled_order(
                                    &signal.pair.0,
                                    &order_id,
                                    &format!("open fill failed (get_executions error: {e})"),
                                )
                                .await;
                            Err(anyhow::anyhow!(
                                "open fill: poll_executions failed + get_executions failed for order {order_id}: {e}; cleanup: {cleanup_err}"
                            ))
                        }
                    }
                }
            }
        }
    }

    /// fill_close: trade → 決済価格 + commission
    ///
    /// - dry_run=true: PriceStore から Long 決済=bid / Short 決済=ask、commission は estimate_close
    /// - dry_run=false: 反対売買 send_child_order → poll_executions、commission は約定の合計
    async fn fill_close(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            let feed_key = FeedKey::new(self.exchange, trade.pair.clone());
            let (bid, ask) = self
                .price_store
                .latest_bid_ask(&feed_key)
                .await
                .ok_or_else(|| anyhow::anyhow!("no bid/ask available for {}", trade.pair))?;
            // Long position クローズ = 売り (bid で約定)
            // Short position クローズ = 買い (ask で約定)
            let price = match trade.direction {
                Direction::Long => bid,
                Direction::Short => ask,
            };
            let commission =
                auto_trader_core::commission::estimate_close(self.exchange, price, trade.quantity);
            Ok((price, commission))
        } else {
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

    /// poll_executions: live のみ、send_child_order 直後に呼んで実約定価格 + 実数量を取得
    ///
    /// Exponential back-off starting at 100 ms (doubling each attempt, capped at
    /// 1.6 s). bitFlyer market orders typically fill within ~100 ms, so the first
    /// poll almost always succeeds — avoiding the 900 ms wasted by a fixed 1 s
    /// sleep. Timeout is unchanged at 5 s.
    async fn poll_executions(
        &self,
        acceptance_id: &str,
        pair_str: &str,
        timeout: Duration,
    ) -> anyhow::Result<(Decimal, Decimal, Decimal)> {
        let start = Instant::now();
        let mut delay = Duration::from_millis(100);
        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("get_executions timed out after {:?}", timeout);
            }
            let execs = self.api.get_executions(pair_str, acceptance_id).await?;
            if !execs.is_empty() {
                let total_size: Decimal = execs.iter().map(|e| e.size).sum();
                if !total_size.is_zero() {
                    let total_notional: Decimal = execs.iter().map(|e| e.price * e.size).sum();
                    let total_commission: Decimal = execs.iter().map(|e| e.commission).sum();
                    let avg = total_notional / total_size;
                    return Ok((avg, total_size, total_commission));
                }
            }
            tokio::time::sleep(delay).await;
            // Double the delay each iteration, cap at 1.6 s
            delay = (delay * 2).min(Duration::from_millis(1600));
        }
    }

    /// signal → SendChildOrderRequest マッピング (helper)
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

    /// Resolve `exchange_position_id` from the exchange with a short retry
    /// loop. GMO's `/v1/openPositions` can lag the fill by a few hundred ms,
    /// so a single GET right after `fill_open` often returns nothing. Three
    /// attempts at 500ms spacing covers typical settlement latency without
    /// blocking the open path for long.
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
                        "resolve_position_id error (retrying if attempts remain)"
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
            "resolve_position_id returned None after all retries"
        );
        None
    }

    /// Reject a live close on exchanges that require a position id when the
    /// trade has none stored. On GMO FX this prevents a close-without-positionId
    /// from being silently dispatched to `/v1/order`, which would open an
    /// opposite-direction position instead of closing the original.
    /// bitFlyer-style exchanges return `false` from `requires_close_position_id`
    /// and pass this check regardless of `exchange_position_id`.
    fn ensure_close_position_id_present(&self, trade: &Trade) -> anyhow::Result<()> {
        if self.api.requires_close_position_id() && trade.exchange_position_id.is_none() {
            anyhow::bail!(
                "live close refused for trade {} on {}: exchange_position_id is None and this exchange requires it (closing without a position id would open an opposite position)",
                trade.id,
                self.exchange.as_str()
            );
        }
        Ok(())
    }

    /// 反対売買用 (close)
    fn opposite_side_market_order(&self, trade: &Trade) -> SendChildOrderRequest {
        let side = match trade.direction {
            // Long の決済 = Sell
            Direction::Long => Side::Sell,
            // Short の決済 = Buy
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

    /// Phase 2 variant for stale-lock crash recovery (live only).
    ///
    /// Before dispatching a new reverse order, verify whether the exchange
    /// still holds the position. If Phase 2 already succeeded before the
    /// crash, re-running it would open an unintended opposite-direction
    /// position. In that case, skip Phase 2 and return a best-effort exit
    /// price directly.
    ///
    /// Also handles partial fills: if the exchange position size is less than
    /// `trade.quantity`, only the remaining size is closed to prevent over-close
    /// (which would otherwise open an unintended opposite position).
    ///
    /// Returns `(exit_price, close_commission, was_approximate)`.
    /// `was_approximate` is `true` when the exchange position was already gone
    /// and we used a best-effort price (PriceStore mid or entry_price fallback);
    /// in that case close_commission is 0 because no fresh execution data is
    /// available. Callers should emit an operator-visible alert when this flag
    /// is set.
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
                        "stale recovery: opposite-side position for trade {} ({:?} vs {:?}, size={}); \
                         refusing auto-recover — manual intervention required",
                        trade.id,
                        d,
                        trade.direction,
                        p.size
                    );
                }
                None => {
                    anyhow::bail!(
                        "stale recovery: unknown side '{}' for trade {} on {}; manual intervention required",
                        p.side,
                        trade.id,
                        p.product_code
                    );
                }
            }
        }

        if !has_any_for_product {
            // Exchange has no matching position → Phase 2 completed before crash.
            // Skip Phase 2, return a best-effort exit price for Phase 3.
            tracing::warn!(
                "close_position: exchange shows trade {} already closed; \
                 completing Phase 3 with best-effort exit price",
                trade.id
            );
            let price = self.resolve_stale_exit_price(trade).await?;
            // No fresh execution → commission unknown, fall back to 0.
            Ok((price, Decimal::ZERO, true))
        } else if total_exchange_size == trade.quantity {
            // Full quantity still open → Phase 2 really failed, re-run it.
            tracing::info!(
                "stale recovery: exchange has full size {} for trade {}, re-running Phase 2",
                total_exchange_size,
                trade.id
            );
            let (price, commission) = self.fill_close(trade).await?;
            Ok((price, commission, false))
        } else if total_exchange_size < trade.quantity {
            // Partial fill before crash: close only remaining size to avoid
            // over-close which would open an unintended opposite position.
            tracing::warn!(
                "stale recovery: partial fill detected — trade.quantity={} but exchange has {} \
                 remaining; closing only remaining size to avoid over-close",
                trade.quantity,
                total_exchange_size
            );
            let (price, commission) = self.fill_close_size(trade, total_exchange_size).await?;
            // The DB row still records trade.quantity; the already-filled delta was
            // handled in the original Phase 2 before the crash. We approximate here.
            Ok((price, commission, true))
        } else {
            // Exchange size > trade.quantity — invariant violation. A single
            // exchange account per process should never have more than trade.quantity
            // in the same direction for this pair. Bail rather than risk oscillation.
            anyhow::bail!(
                "stale recovery: exchange position size {} exceeds trade.quantity {} for trade \
                 {}; invariant violated — refusing to close to avoid oscillation. \
                 Manual intervention required.",
                total_exchange_size,
                trade.quantity,
                trade.id
            )
        }
    }

    /// Close a specific size (not necessarily `trade.quantity`) on the exchange.
    ///
    /// Used by stale recovery when a partial fill was detected: the remaining
    /// exchange position is smaller than `trade.quantity`, so we close only what
    /// the exchange actually holds to prevent over-close / unintended reversal.
    async fn fill_close_size(
        &self,
        trade: &Trade,
        size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            // Dry-run: return price from PriceStore regardless of size (same as fill_close).
            return self.fill_close(trade).await;
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

    /// Best-effort exit price for a trade that was closed at the exchange but
    /// whose Phase 3 DB update never completed (process crash after Phase 2).
    ///
    /// Returns the current PriceStore mid-price, or falls back to `entry_price`
    /// if no price data is available. Logs a warning in either case so the
    /// operator can cross-check against exchange records.
    async fn resolve_stale_exit_price(&self, trade: &Trade) -> anyhow::Result<Decimal> {
        let feed_key = FeedKey::new(self.exchange, trade.pair.clone());
        match self.price_store.latest_bid_ask(&feed_key).await {
            Some((bid, ask)) => {
                let mid = (bid + ask) / Decimal::from(2);
                tracing::warn!(
                    "resolve_stale_exit_price: trade {} using PriceStore mid {} as exit price",
                    trade.id,
                    mid
                );
                Ok(mid)
            }
            None => {
                tracing::warn!(
                    "resolve_stale_exit_price: trade {} no PriceStore data for {}; \
                     using entry_price {} as approximate exit price",
                    trade.id,
                    trade.pair,
                    trade.entry_price
                );
                Ok(trade.entry_price)
            }
        }
    }
}

/// Map an exchange-side string to a `Direction`.
///
/// Returns `None` for unrecognised side strings so callers can treat them
/// conservatively instead of silently misclassifying.
fn exchange_side_to_direction(side: &str) -> Option<Direction> {
    match side.trim().to_ascii_uppercase().as_str() {
        "BUY" => Some(Direction::Long),
        "SELL" => Some(Direction::Short),
        _ => None,
    }
}

impl OrderExecutor for Trader {
    async fn execute(&self, signal: &Signal) -> anyhow::Result<Trade> {
        // 1. DB から account 読み (balance, leverage)
        let account = auto_trader_db::trading_accounts::get_account(&self.pool, self.account_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("trading account {} not found", self.account_id))?;

        let balance = account.current_balance;
        let leverage = account.leverage;

        if leverage <= Decimal::ZERO {
            anyhow::bail!(
                "account {} has non-positive leverage {leverage}, refusing to open trade",
                self.account_id
            );
        }

        // 2. Position sizing — reuse the PositionSizer cached in the struct.
        let sizer = &self.position_sizer;

        // fill_open で fill 価格を確定してから正確なサイズを計算するため、
        // まず hint price として最新 bid/ask の ask 側を使ってサイジングする。
        // 実際の fill 価格が少し異なってもサイズは近似値として機能する。
        let feed_key = FeedKey::new(self.exchange, signal.pair.clone());
        let hint_price = {
            if let Some((bid, ask)) = self.price_store.latest_bid_ask(&feed_key).await {
                match signal.direction {
                    Direction::Long => ask,
                    Direction::Short => bid,
                }
            } else {
                anyhow::bail!(
                    "no price available for {} to calculate position size",
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
                    "account balance too small to open minimum order for {}",
                    signal.pair
                )
            })?;

        // Capture the cutoff BEFORE sending the open order. GMO timestamps
        // the position at fill time, which is earlier than the moment we
        // observe completion after `fill_open` returns. Using a post-fill
        // timestamp would filter out the just-opened position.
        // Subtract a small safety margin (1 second) to cover any clock skew
        // between this host and GMO's exchange-side clock.
        let position_cutoff = Utc::now() - chrono::Duration::seconds(1);

        // 3. fill_open() で fill 価格 + 実数量 + commission 取得
        let (fill_price, actual_qty, open_commission) = self.fill_open(signal, quantity).await?;

        // 4. SL/TP を fill_price から逆算
        let stop_loss = match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE - signal.stop_loss_pct),
            Direction::Short => fill_price * (Decimal::ONE + signal.stop_loss_pct),
        };
        let take_profit = signal.take_profit_pct.map(|pct| match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE + pct),
            Direction::Short => fill_price * (Decimal::ONE - pct),
        });

        // 5. Trade 構築
        let entry_at = Utc::now();
        let margin = truncate_yen(fill_price * actual_qty / leverage);

        // Resolve the exchange-side position id for exchanges that model
        // positions individually (GMO FX). Required by /v1/closeOrder later.
        // Paper trades (dry_run) and exchanges that net positions internally
        // (bitFlyer) return None. We retry with backoff because the exchange
        // may not surface the position in /v1/openPositions for ~hundreds of
        // milliseconds after the fill response. If all retries fail on an
        // exchange that requires a position id (GMO), emit a Slack alert —
        // the resulting trade row will be unclosable until an operator runs
        // manual reconciliation (the position_monitor / startup reconciler
        // can also recover the id later).
        let order_side = match signal.direction {
            Direction::Long => Side::Buy,
            Direction::Short => Side::Sell,
        };
        // Only spend retry latency on exchanges that actually use the field.
        // bitFlyer / OANDA / null impls always return Ok(None); calling them
        // (and waiting 1.5s of backoff) just to discard the result adds dead
        // latency to every non-GMO live open.
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
                    "open succeeded but resolve_position_id returned None for {} after retries — \
                     trade will be unclosable until exchange_position_id is reconciled",
                    signal.pair.0
                ),
            });
            tokio::spawn(async move {
                if let Err(e) = notifier.send(ev).await {
                    tracing::error!("failed to send unresolved-position-id alert: {e}");
                }
            });
        }

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
        };

        // 6. DB 操作 (1 トランザクション)
        // In live mode, the exchange fill is already done at this point — if
        // the DB tx fails we have an orphan exchange position requiring manual
        // reconciliation. In dry_run mode there's no exchange order, so the
        // failure is just a wasted simulated fill (no operator action needed).
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
                    "dry_run: simulated fill computed but DB write failed — no exchange impact"
                );
                // Don't notify Slack on dry_run failures — they're test/sim
                // noise that shouldn't trigger operator action.
            } else {
                tracing::error!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    fill_price = %fill_price,
                    error = %e,
                    "inconsistent state: exchange filled but DB write failed — \
                     orphan exchange position requires manual reconciliation"
                );
                let notifier = self.notifier.clone();
                let account_name = self.account_name.clone();
                let exchange = self.exchange;
                let pair = trade.pair.clone();
                let strategy_name = trade.strategy_name.clone();
                let reason = format!("DB tx failed after exchange fill (orphan position): {e}");
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
                        tracing::warn!("critical notify send failed: {notify_err}");
                    }
                });
            }
            return Err(db_result.unwrap_err());
        }

        // 7. Slack 通知 (fire-and-forget)
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
                tracing::warn!("slack notification failed: {e}");
            }
        });

        // 8. ログ
        tracing::info!(
            "OPEN: {} {} {:?} @ {} qty={} margin_locked={} dry_run={}",
            trade.strategy_name,
            trade.pair,
            trade.direction,
            fill_price,
            actual_qty,
            margin,
            self.dry_run,
        );

        Ok(trade)
    }

    async fn close_position(&self, id: &str, exit_reason: ExitReason) -> anyhow::Result<Trade> {
        let uuid = Uuid::parse_str(id)?;

        // Phase 1: CAS lock — atomically transition open → closing.
        // This is the ownership token that prevents concurrent close paths
        // from BOTH dispatching opposite-side orders to the exchange.
        // If we don't get the lock, another close already won this race.
        let lock = auto_trader_db::trades::acquire_close_lock(
            &self.pool,
            uuid,
            self.account_id,
        )
        .await?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "trade {id} not in 'open' state (already closed/closing or belongs to another account)"
            )
        })?;
        let trade = lock.trade;
        let was_stale_recovery = lock.was_stale_recovery;

        // Phase 2: Execute live fill outside the DB transaction. This is
        // safe because the trade is now status='closing' — no other path
        // will reach fill_close for this trade until we release the lock.
        //
        // If we re-acquired a STALE lock (crash recovery), Phase 2 may have
        // already completed at the exchange. We verify the exchange state
        // before dispatching another reverse order to avoid opening an
        // unintended opposite-direction position.
        // `stale_approximate` is set to true when fill_close_with_stale_recovery
        // determines that the exchange position was already gone and falls back
        // to a best-effort price. We fire an operator alert after Phase 3 in
        // that case so the PnL approximation is always visible.
        let mut stale_approximate = false;
        let exit_result: anyhow::Result<(Decimal, Decimal)> = if !self.dry_run && was_stale_recovery
        {
            tracing::warn!(
                "close_position: stale-lock recovery for trade {}; verifying exchange state before Phase 2",
                trade.id
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
                // Roll back the lock so future close attempts can retry.
                if let Err(release_err) =
                    auto_trader_db::trades::release_close_lock(&self.pool, uuid).await
                {
                    tracing::error!(
                        trade_id = %uuid,
                        original_error = %e,
                        release_error = %release_err,
                        "fill_close failed AND release_close_lock failed; \
                         trade is stuck in 'closing' status — manual intervention required"
                    );
                }
                return Err(e);
            }
        };

        // Phase 3: CAS update + ledger in a single transaction.
        // update_trade_closed accepts WHERE status IN ('open', 'closing'),
        // so we (the lock holder, status='closing') will succeed while any
        // other path that lost Phase 1's CAS would already have been rejected.
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
            fees: trade.fees + close_commission,
            entry_at: trade.entry_at,
            exit_at: Some(exit_at),
            pnl_amount: Some(pnl_amount),
            exit_reason: Some(exit_reason),
            status: TradeStatus::Closed,
            max_hold_until: trade.max_hold_until,
            exchange_position_id: trade.exchange_position_id.clone(),
        };

        // CRITICAL: Phase 2 (exchange fill) succeeded. If this Phase 3 DB tx
        // fails, the exchange position is closed but the DB trade remains in
        // status='closing'. The 5-min stale-closing self-healing in
        // acquire_close_lock (or the PR-2 reconciler) will detect and handle
        // this. Emit a critical notification + error log so the operator is
        // aware immediately.
        let margin = truncate_yen(trade.entry_price * trade.quantity / trade.leverage);
        let phase3_result = async {
            let mut tx = self.pool.begin().await?;
            // CAS: bails with "already closed" if rows_affected == 0
            auto_trader_db::trades::update_trade_closed(
                &mut *tx,
                trade.id,
                exit_price,
                exit_at,
                pnl_amount,
                exit_reason,
                trade.fees,
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
                    "dry_run: simulated close fill computed but DB Phase 3 failed — \
                     trade remains in 'closing'; no exchange impact, stale-lock \
                     self-healing will eventually free it"
                );
                // Don't notify Slack on dry_run failures.
            } else {
                tracing::error!(
                    trade_id = %trade.id,
                    account_id = %self.account_id,
                    pair = %trade.pair,
                    exit_price = %exit_price,
                    error = %e,
                    "inconsistent state: exchange close filled but DB Phase 3 write failed — \
                     trade remains in 'closing' status; stale-lock self-healing or PR-2 \
                     reconciler will pick this up"
                );
                let notifier = self.notifier.clone();
                let account_name = self.account_name.clone();
                let exchange = self.exchange;
                let strategy_name = trade.strategy_name.clone();
                let pair = trade.pair.clone();
                let trade_id = trade.id;
                let reason = format!(
                    "close DB tx failed after exchange fill (trade {trade_id} stuck in 'closing'): {e}"
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
                        tracing::warn!("critical notify send failed: {notify_err}");
                    }
                });
            }
            return Err(phase3_result.unwrap_err());
        }

        // 6. Slack 通知 (fire-and-forget)
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
                tracing::warn!("slack notification failed: {e}");
            }
        });

        // W2: stale-recovery approximate price alert — operator must audit PnL.
        if stale_approximate {
            let notifier = self.notifier.clone();
            let exchange = self.exchange;
            let pair = closed_trade.pair.clone();
            let strategy_name = closed_trade.strategy_name.clone();
            let reason = format!(
                "stale-recovery close for trade {trade_id}: reconciliation used an approximation \
                 during close recovery — exit price and/or PnL attribution may be inaccurate; \
                 manual audit against exchange records recommended"
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
                    tracing::warn!("stale-recovery alert send failed: {notify_err}");
                }
            });
        }

        tracing::info!(
            "CLOSE: {} {} pnl={} reason={:?} dry_run={}",
            closed_trade.strategy_name,
            closed_trade.pair,
            pnl_amount,
            exit_reason,
            self.dry_run,
        );

        Ok(closed_trade)
    }

    async fn open_positions(&self) -> anyhow::Result<Vec<Position>> {
        let trades =
            auto_trader_db::trades::get_open_trades_by_account(&self.pool, self.account_id).await?;
        Ok(trades.into_iter().map(|trade| Position { trade }).collect())
    }
}
