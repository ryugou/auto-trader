//! position monitor から呼ばれる「Trader 構築 → close_position → TradeEvent 送信」の共通経路。
//!
//! SL/TP/TimeLimit と Liquidation で同じ手順 (api 解決 → liquidation_level
//! 解決 → UnifiedTrader::new → close_position → trade_tx.send) を踏むため、
//! ここに集約する。close 失敗時の log level も一箇所に揃え、Liquidation
//! 経路で `TradeEvent` 送信を忘れて daily summary に乗らない、といった
//! divergence を防ぐ。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::{TradeAction, TradeEvent};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Exchange, ExitReason, Trade};
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_executor::trader::Trader as UnifiedTrader;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;
use sqlx::PgPool;
use tokio::sync::mpsc;

use crate::startup;

/// position monitor が close 経路を呼び出すときに使う環境。
///
/// price tick ループの先頭で 1 度組み立てて借用で渡す想定。`Arc` で
/// 共有された state を持つので clone は安価。
pub struct CloseContext {
    pub pool: PgPool,
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    pub price_store: Arc<PriceStore>,
    pub notifier: Arc<Notifier>,
    pub position_sizer: Arc<PositionSizer>,
    pub liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub trade_tx: mpsc::Sender<TradeEvent>,
}

/// `trade` を `exit_reason` で close し、成功時に `TradeEvent::Closed` を
/// 流すまでをワンショットで行う。
///
/// - api 不在で live の場合は warn + 早期 return (DB の close は走らない)
/// - paper / `live_forces_dry_run` の場合は `NullExchangeApi` で fallback
/// - liquidation_level が設定漏れなら早期 return (`liquidation_level_or_log`
///   が warn を出す)
/// - close 失敗は debug log (acquire_close_lock の concurrent loser を含む)
pub async fn close_trade(
    ctx: &CloseContext,
    trade: &Trade,
    account_name: String,
    account_type: String,
    dry_run: bool,
    exit_reason: ExitReason,
    current_price: Decimal,
) {
    let api: Arc<dyn ExchangeApi> = match ctx.apis.get(&trade.exchange) {
        Some(a) => a.clone(),
        None => {
            if !dry_run {
                tracing::warn!(
                    "no ExchangeApi registered for exchange {:?}; skipping close for live trade {}",
                    trade.exchange,
                    trade.id
                );
                return;
            }
            Arc::new(auto_trader_market::null_exchange_api::NullExchangeApi)
        }
    };

    let liquidation_margin_level =
        match startup::liquidation_level_or_log(&ctx.liquidation_levels, trade.exchange, || {
            format!("close trade {}", trade.id)
        }) {
            Some(y) => y,
            None => return,
        };

    let trader = UnifiedTrader::new(
        ctx.pool.clone(),
        trade.exchange,
        trade.account_id,
        account_name,
        api,
        ctx.price_store.clone(),
        ctx.notifier.clone(),
        ctx.position_sizer.clone(),
        liquidation_margin_level,
        dry_run,
    );

    match trader
        .close_position(&trade.id.to_string(), exit_reason)
        .await
    {
        Ok(closed_trade) => {
            let exit_price = closed_trade.exit_price.unwrap_or(current_price);
            tracing::info!(
                "position closed: {} {} {:?} at {} ({:?})",
                closed_trade.strategy_name,
                closed_trade.pair,
                closed_trade.direction,
                exit_price,
                exit_reason
            );
            if let Err(e) = ctx
                .trade_tx
                .send(TradeEvent {
                    trade: closed_trade,
                    action: TradeAction::Closed {
                        exit_price,
                        exit_reason,
                    },
                    account_type: Some(account_type),
                })
                .await
            {
                tracing::error!("trade channel send failed for position close: {e}");
            }
        }
        Err(e) => {
            // SL/TP/TimeLimit close failures are mostly concurrent-close losers
            // (`acquire_close_lock` lost the race) — expected, log at debug.
            // Liquidation close failures (price unavailable / DB error etc.)
            // leave the account exposed and must reach operators — log at warn.
            if matches!(exit_reason, ExitReason::Liquidation) {
                tracing::warn!(
                    "liquidation close FAILED for trade {} (account exposed): {e}",
                    trade.id
                );
            } else {
                tracing::debug!("close_position skipped/failed for trade {}: {e}", trade.id);
            }
        }
    }
}
