use auto_trader_core::types::{Direction, Signal, Trade};
use auto_trader_notify::{NotifyEvent, OrderFilledEvent};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use super::super::Trader;
use super::super::notification::NotificationFailureLevel;
use super::super::order_side::{closing_side, opening_side};

const POSITION_CUTOFF_CLOCK_SKEW_MARGIN_SECS: i64 = 1;

impl Trader {
    pub(super) async fn prepare_entry(
        &self,
        signal: &Signal,
    ) -> anyhow::Result<(Decimal, Decimal, DateTime<Utc>)> {
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

        let (bid, ask) = self.latest_bid_ask(&signal.pair).await?;
        let hint_price = match signal.direction {
            Direction::Long => ask,
            Direction::Short => bid,
        };

        let quantity = self
            .position_sizer
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

        Ok((leverage, quantity, position_cutoff))
    }

    pub(super) fn compute_stop_levels(
        signal: &Signal,
        fill_price: Decimal,
    ) -> (Decimal, Option<Decimal>) {
        let stop_loss = match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE - signal.stop_loss_pct),
            Direction::Short => fill_price * (Decimal::ONE + signal.stop_loss_pct),
        };
        let take_profit = signal.take_profit_pct.map(|pct| match signal.direction {
            Direction::Long => fill_price * (Decimal::ONE + pct),
            Direction::Short => fill_price * (Decimal::ONE - pct),
        });
        (stop_loss, take_profit)
    }

    pub(super) async fn resolve_exchange_position_id(
        &self,
        signal: &Signal,
        position_cutoff: DateTime<Utc>,
        actual_qty: Decimal,
    ) -> Option<String> {
        let order_side = opening_side(signal.direction);
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
            let ev = self.order_failed_event(
                signal.strategy_name.clone(),
                signal.pair.clone(),
                format!(
                    "failed to resolve exchange_position_id for \"{}\" after retries; \
                     trade will be unclosable until reconciled",
                    signal.pair.0
                ),
            );
            self.spawn_notification(
                ev,
                NotificationFailureLevel::Error,
                "failed to send unresolved-position-id alert",
            );
        }
        exchange_position_id
    }

    pub(super) async fn place_protective_stop_order(
        &self,
        signal: &Signal,
        actual_qty: Decimal,
        stop_loss: Decimal,
        exchange_position_id: Option<&str>,
    ) -> Option<String> {
        if self.dry_run {
            return None;
        }
        let close_side = closing_side(signal.direction);
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
                exchange_position_id,
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
                let ev = self.order_failed_event(
                    signal.strategy_name.clone(),
                    signal.pair.clone(),
                    format!(
                        "failed to place exchange-side stop order: {e} (position unprotected if app dies)"
                    ),
                );
                self.spawn_notification(
                    ev,
                    NotificationFailureLevel::Warn,
                    "failed to send stop-order-failure alert",
                );
                None
            }
        }
    }

    pub(super) async fn persist_open_trade(
        &self,
        trade: &Trade,
        margin: Decimal,
        fill_price: Decimal,
    ) -> anyhow::Result<()> {
        let db_result = async {
            let mut tx = self.pool.begin().await?;
            auto_trader_db::trades::insert_trade(&mut *tx, trade).await?;
            auto_trader_db::trades::lock_margin(&mut tx, self.account_id, trade.id, margin).await?;
            auto_trader_db::notifications::insert_trade_opened(&mut *tx, trade).await?;
            tx.commit().await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = db_result {
            self.handle_open_persist_failure(trade, fill_price, &e);
            return Err(e);
        }
        Ok(())
    }

    fn handle_open_persist_failure(&self, trade: &Trade, fill_price: Decimal, e: &anyhow::Error) {
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
            let ev = self.order_failed_event(
                trade.strategy_name.clone(),
                trade.pair.clone(),
                format!(
                    "failed to commit trade open: {e}; exchange filled but DB write failed, \
                     orphan position requires manual reconciliation"
                ),
            );
            self.spawn_notification(
                ev,
                NotificationFailureLevel::Warn,
                "failed to send critical notification",
            );
        }
    }

    pub(super) fn notify_order_filled(
        &self,
        trade: &Trade,
        actual_qty: Decimal,
        fill_price: Decimal,
        entry_at: DateTime<Utc>,
    ) {
        let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
            account_name: self.account_name.clone(),
            exchange: self.exchange,
            trade_id: trade.id,
            pair: trade.pair.clone(),
            direction: trade.direction,
            quantity: actual_qty,
            price: fill_price,
            at: entry_at,
        });
        self.spawn_notification(
            ev,
            NotificationFailureLevel::Warn,
            "failed to send slack notification",
        );
    }
}
