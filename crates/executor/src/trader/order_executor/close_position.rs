use auto_trader_core::types::{ExitReason, Trade};
use auto_trader_notify::{NotifyEvent, PositionClosedEvent};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::super::Trader;
use super::super::notification::NotificationFailureLevel;

impl Trader {
    pub(super) async fn load_trade_for_close(
        &self,
        id: &str,
    ) -> anyhow::Result<(Uuid, Trade, bool)> {
        let uuid = Uuid::parse_str(id)?;
        let lock = auto_trader_db::trades::acquire_close_lock(&self.pool, uuid, self.account_id)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid trade \"{id}\": not in 'open' state (already closed/closing or belongs to another account)"
                )
            })?;
        Ok((uuid, lock.trade, lock.was_stale_recovery))
    }

    pub(super) async fn close_and_release_lock_on_error(
        &self,
        uuid: Uuid,
        trade: &Trade,
        was_stale_recovery: bool,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        let exit_result = if !self.dry_run && was_stale_recovery {
            tracing::warn!(
                trade_id = %trade.id,
                "stale-lock recovery; verifying exchange state before Phase 2"
            );
            self.fill_close_with_stale_recovery(trade).await
        } else {
            self.fill_close(trade)
                .await
                .map(|(price, commission)| (price, commission, false))
        };
        if let Err(e) = &exit_result
            && let Err(release_err) =
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
        exit_result
    }

    pub(super) async fn persist_closed_trade(
        &self,
        closed_trade: &Trade,
        margin: Decimal,
        pnl_amount: Decimal,
        exit_price: Decimal,
        exit_at: DateTime<Utc>,
        exit_reason: ExitReason,
    ) -> anyhow::Result<()> {
        let phase3_result = async {
            let mut tx = self.pool.begin().await?;
            auto_trader_db::trades::update_trade_closed(
                &mut *tx,
                closed_trade.id,
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
                closed_trade.id,
                margin,
                pnl_amount,
            )
            .await?;

            auto_trader_db::notifications::insert_trade_closed(&mut *tx, closed_trade).await?;
            tx.commit().await?;
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = phase3_result {
            self.handle_close_persist_failure(closed_trade, exit_price, &e);
            return Err(e);
        }
        Ok(())
    }

    fn handle_close_persist_failure(
        &self,
        closed_trade: &Trade,
        exit_price: Decimal,
        e: &anyhow::Error,
    ) {
        if self.dry_run {
            tracing::warn!(
                trade_id = %closed_trade.id,
                account_id = %self.account_id,
                pair = %closed_trade.pair,
                exit_price = %exit_price,
                error = %e,
                "failed to write simulated trade close to DB in dry_run; \
                 trade remains in 'closing', no exchange impact, stale-lock \
                 self-healing will eventually free it"
            );
        } else {
            tracing::error!(
                trade_id = %closed_trade.id,
                account_id = %self.account_id,
                pair = %closed_trade.pair,
                exit_price = %exit_price,
                error = %e,
                "failed to write trade close to DB after exchange fill; inconsistent state, \
                 trade remains in 'closing' status, stale-lock self-healing or PR-2 \
                 reconciler will pick this up"
            );
            let ev = self.order_failed_event(
                closed_trade.strategy_name.clone(),
                closed_trade.pair.clone(),
                format!(
                    "failed to commit trade close for \"{}\": {e}; \
                     trade stuck in 'closing' status",
                    closed_trade.id
                ),
            );
            self.spawn_notification(
                ev,
                NotificationFailureLevel::Warn,
                "failed to send critical notification",
            );
        }
    }

    pub(super) fn notify_close_result(
        &self,
        closed_trade: &Trade,
        pnl_amount: Decimal,
        exit_reason: ExitReason,
        stale_approximate: bool,
    ) {
        let trade_id = closed_trade.id;
        let ev = NotifyEvent::PositionClosed(PositionClosedEvent {
            account_name: self.account_name.clone(),
            exchange: self.exchange,
            trade_id,
            pnl_amount,
            reason: exit_reason.as_str().to_owned(),
        });
        self.spawn_notification(
            ev,
            NotificationFailureLevel::Warn,
            "failed to send slack notification",
        );

        if stale_approximate {
            let ev = self.order_failed_event(
                closed_trade.strategy_name.clone(),
                closed_trade.pair.clone(),
                format!(
                    "invalid close for trade \"{trade_id}\": stale-recovery reconciliation used an \
                     approximation; exit price and/or pnl attribution may be inaccurate, manual audit \
                     against exchange records recommended"
                ),
            );
            self.spawn_notification(
                ev,
                NotificationFailureLevel::Warn,
                "failed to send stale-recovery alert",
            );
        }
    }
}
