use auto_trader_core::commission;
use auto_trader_core::types::{Direction, Trade};
use auto_trader_market::bitflyer_private::{ChildOrderType, SendChildOrderRequest};
use auto_trader_market::exchange_api::StopOrderStatus;
use auto_trader_market::price_store::FeedKey;
use rust_decimal::Decimal;

use super::Trader;
use super::order_side::closing_side;

fn exchange_side_to_direction(side: &str) -> Option<Direction> {
    match side.trim().to_ascii_uppercase().as_str() {
        "BUY" => Some(Direction::Long),
        "SELL" => Some(Direction::Short),
        _ => None,
    }
}

impl Trader {
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

    pub(super) async fn fill_close(&self, trade: &Trade) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            let (bid, ask) = self.latest_bid_ask(&trade.pair).await?;
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
            self.fill_close_live(trade, trade.quantity).await
        }
    }

    async fn fill_close_live(
        &self,
        trade: &Trade,
        size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal)> {
        if let Some(fill) = self.stop_guard_before_close(trade).await? {
            return Ok(fill);
        }
        self.ensure_close_position_id_present(trade)?;
        let req = SendChildOrderRequest {
            product_code: trade.pair.0.clone(),
            child_order_type: ChildOrderType::Market,
            side: closing_side(trade.direction),
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

    async fn sum_exchange_position_size(&self, trade: &Trade) -> anyhow::Result<(Decimal, bool)> {
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

        Ok((total_exchange_size, has_any_for_product))
    }

    async fn recover_exit_when_already_closed(
        &self,
        trade: &Trade,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        tracing::warn!(
            trade_id = %trade.id,
            "exchange shows trade already closed; completing Phase 3 with best-effort exit price"
        );
        let price = self.resolve_stale_exit_price(trade).await?;
        Ok((price, Decimal::ZERO, true))
    }

    async fn recover_exit_when_full_size(
        &self,
        trade: &Trade,
        total_exchange_size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        tracing::info!(
            trade_id = %trade.id,
            exchange_size = %total_exchange_size,
            "exchange has full size; re-running Phase 2"
        );
        let (price, commission) = self.fill_close(trade).await?;
        Ok((price, commission, false))
    }

    async fn recover_exit_when_partial_size(
        &self,
        trade: &Trade,
        total_exchange_size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        tracing::warn!(
            trade_id = %trade.id,
            quantity = %trade.quantity,
            exchange_size = %total_exchange_size,
            "partial fill detected; closing only remaining size to avoid over-close"
        );
        let (price, commission) = self.fill_close_size(trade, total_exchange_size).await?;
        Ok((price, commission, true))
    }

    pub(super) async fn fill_close_with_stale_recovery(
        &self,
        trade: &Trade,
    ) -> anyhow::Result<(Decimal, Decimal, bool)> {
        let (total_exchange_size, has_any_for_product) =
            self.sum_exchange_position_size(trade).await?;

        if !has_any_for_product {
            return self.recover_exit_when_already_closed(trade).await;
        }
        if total_exchange_size == trade.quantity {
            return self
                .recover_exit_when_full_size(trade, total_exchange_size)
                .await;
        }
        if total_exchange_size < trade.quantity {
            return self
                .recover_exit_when_partial_size(trade, total_exchange_size)
                .await;
        }
        anyhow::bail!(
            "invalid position for trade \"{}\": exchange size {} exceeds trade quantity {}; \
             refusing to close to avoid oscillation, manual intervention required",
            trade.id,
            total_exchange_size,
            trade.quantity,
        )
    }

    async fn fill_close_size(
        &self,
        trade: &Trade,
        size: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal)> {
        if self.dry_run {
            return self.fill_close(trade).await;
        }
        self.fill_close_live(trade, size).await
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

    pub(super) async fn snapshot_live_sfd_before_close(&self, trade: &Trade) -> Decimal {
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
