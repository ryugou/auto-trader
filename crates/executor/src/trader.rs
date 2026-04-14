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
    BitflyerPrivateApi, ChildOrderType, SendChildOrderRequest, Side,
};
use auto_trader_market::price_store::{FeedKey, PriceStore};
use auto_trader_notify::{Notifier, NotifyEvent, OrderFilledEvent, PositionClosedEvent};
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

pub struct Trader {
    pool: PgPool,
    exchange: Exchange,
    account_id: Uuid,
    /// Cached at construction time so every Slack notification shows the
    /// human-readable account name rather than the raw UUID.
    account_name: String,
    api: Arc<BitflyerPrivateApi>,
    price_store: Arc<PriceStore>,
    notifier: Arc<Notifier>,
    /// Cached at construction time; one PositionSizer per Trader lifetime.
    position_sizer: PositionSizer,
    dry_run: bool,
}

impl Trader {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        exchange: Exchange,
        account_id: Uuid,
        account_name: String,
        api: Arc<BitflyerPrivateApi>,
        price_store: Arc<PriceStore>,
        notifier: Arc<Notifier>,
        position_sizer: PositionSizer,
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
            dry_run,
        }
    }

    /// fill_open: signal → 約定価格 + 実数量
    ///
    /// - dry_run=true: PriceStore から Long=ask / Short=bid
    /// - dry_run=false: send_child_order → poll_executions
    async fn fill_open(
        &self,
        signal: &Signal,
        quantity: Decimal,
    ) -> anyhow::Result<(Decimal, Decimal)> {
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
            Ok((price, quantity))
        } else {
            let req = self.signal_to_send_child_order(signal, quantity);
            let resp = self.api.send_child_order(req).await?;
            self.poll_executions(
                &resp.child_order_acceptance_id,
                &signal.pair.0,
                Duration::from_secs(5),
            )
            .await
        }
    }

    /// fill_close: trade → 決済価格
    ///
    /// - dry_run=true: PriceStore から Long 決済=bid / Short 決済=ask
    /// - dry_run=false: 反対売買 send_child_order → poll_executions
    async fn fill_close(&self, trade: &Trade) -> anyhow::Result<Decimal> {
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
            Ok(price)
        } else {
            let req = self.opposite_side_market_order(trade);
            let resp = self.api.send_child_order(req).await?;
            let (price, _qty) = self
                .poll_executions(
                    &resp.child_order_acceptance_id,
                    &trade.pair.0,
                    Duration::from_secs(5),
                )
                .await?;
            Ok(price)
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
    ) -> anyhow::Result<(Decimal, Decimal)> {
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
                    let avg = total_notional / total_size;
                    return Ok((avg, total_size));
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
        }
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
        }
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
            )
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "account balance too small to open minimum order for {}",
                    signal.pair
                )
            })?;

        // 3. fill_open() で fill 価格 + 実数量取得
        let (fill_price, actual_qty) = self.fill_open(signal, quantity).await?;

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
            fees: Decimal::ZERO,
            entry_at,
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
        };

        // 6. DB 操作 (1 トランザクション)
        let mut tx = self.pool.begin().await?;
        auto_trader_db::trades::insert_trade(&mut *tx, &trade).await?;
        auto_trader_db::trades::lock_margin(&mut tx, self.account_id, trade.id, margin).await?;
        auto_trader_db::notifications::insert_trade_opened(&mut *tx, &trade).await?;
        tx.commit().await?;

        // 7. Slack 通知 (fire-and-forget)
        let notifier = self.notifier.clone();
        let account_name = self.account_name.clone();
        let ev = NotifyEvent::OrderFilled(OrderFilledEvent {
            account_name,
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
        let trade = auto_trader_db::trades::acquire_close_lock(
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

        // Phase 2: Execute live fill outside the DB transaction. This is
        // safe because the trade is now status='closing' — no other path
        // will reach fill_close for this trade until we release the lock.
        let exit_price = match self.fill_close(&trade).await {
            Ok(price) => price,
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
        // update_trade_closed uses WHERE status = 'open' — rows_affected == 0
        // means another concurrent close already won this race; we bail.
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
            fees: trade.fees,
            entry_at: trade.entry_at,
            exit_at: Some(exit_at),
            pnl_amount: Some(pnl_amount),
            exit_reason: Some(exit_reason),
            status: TradeStatus::Closed,
            max_hold_until: trade.max_hold_until,
        };

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

        let margin = truncate_yen(trade.entry_price * trade.quantity / trade.leverage);
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

        // 6. Slack 通知 (fire-and-forget)
        let notifier = self.notifier.clone();
        let trade_id = closed_trade.id;
        let account_name = self.account_name.clone();
        let ev = NotifyEvent::PositionClosed(PositionClosedEvent {
            account_name,
            trade_id,
            pnl_amount,
            reason: exit_reason.as_str().to_owned(),
        });
        tokio::spawn(async move {
            if let Err(e) = notifier.send(ev).await {
                tracing::warn!("slack notification failed: {e}");
            }
        });

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
