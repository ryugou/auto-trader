use auto_trader_core::event::{PriceEvent, TradeEvent, TradeAction};
use auto_trader_core::executor::OrderExecutor;
use auto_trader_core::types::{Direction, ExitReason};
use std::sync::Arc;
use tokio::sync::mpsc;

pub async fn run_position_monitor<E: OrderExecutor>(
    executor: Arc<E>,
    mut price_rx: mpsc::Receiver<PriceEvent>,
    trade_tx: mpsc::Sender<TradeEvent>,
) {
    while let Some(event) = price_rx.recv().await {
        let current_price = event.candle.close;
        let positions = match executor.open_positions().await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("position monitor: failed to get positions: {e}");
                continue;
            }
        };

        for pos in positions {
            let trade = &pos.trade;
            if trade.pair != event.pair {
                continue;
            }

            let exit_reason = match trade.direction {
                Direction::Long => {
                    if current_price <= trade.stop_loss {
                        Some(ExitReason::SlHit)
                    } else if current_price >= trade.take_profit {
                        Some(ExitReason::TpHit)
                    } else {
                        None
                    }
                }
                Direction::Short => {
                    if current_price >= trade.stop_loss {
                        Some(ExitReason::SlHit)
                    } else if current_price <= trade.take_profit {
                        Some(ExitReason::TpHit)
                    } else {
                        None
                    }
                }
            };

            if let Some(reason) = exit_reason {
                let exit_price = match reason {
                    ExitReason::SlHit => trade.stop_loss,
                    ExitReason::TpHit => trade.take_profit,
                    _ => current_price,
                };

                match executor.close_position(&trade.id.to_string(), reason, exit_price).await {
                    Ok(closed_trade) => {
                        tracing::info!(
                            "position closed: {} {} {:?} at {} ({:?})",
                            closed_trade.strategy_name, closed_trade.pair,
                            closed_trade.direction, exit_price, reason
                        );
                        let _ = trade_tx.send(TradeEvent {
                            trade: closed_trade,
                            action: TradeAction::Closed { exit_price, exit_reason: reason },
                        }).await;
                    }
                    Err(e) => tracing::error!("failed to close position: {e}"),
                }
            }
        }
    }
    tracing::info!("position monitor: price channel closed, stopping");
}
