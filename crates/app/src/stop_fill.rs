//! 取引所側 SL ストップ注文の発火検知ジョブ (Phase 4 / Task 4.6)。
//!
//! アプリが tick 監視で SL を検出する前に、取引所側でストップ注文が
//! 発火・約定しているケースを定期的に拾う。`stop_order_id` を持つ live
//! trade について `stop_order_status` を確認し、`Executed` なら
//! `closer::close_trade(..., SlHit, ..)` を呼ぶ。close_trade →
//! close_position → fill_close は Task 4.5 のガードにより「Executed を
//! 検出してその約定価格を返す」ので、**二重発注にはならない**。

use auto_trader_core::types::ExitReason;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::exchange_api::StopOrderStatus;
use auto_trader_market::price_store::FeedKey;

use crate::closer::{CloseContext, close_trade};
use crate::startup::effective_dry_run;

/// `list_open_with_account_name` の結果から「stop 発火検知の対象」だけを絞る。
///
/// 対象条件: live (effective_dry_run == false) かつ `stop_order_id` を持つ。
/// paper trade は取引所側ストップを置かない (アプリ側 SL シミュレーションが正)
/// ので常に除外する。純関数なのでユニットテストで検証する。
pub fn is_stop_detection_target(t: &OpenTradeWithAccount, live_forces_dry_run: bool) -> bool {
    let account_type = t.account_type.as_deref().unwrap_or("paper");
    let dry_run = effective_dry_run(account_type, live_forces_dry_run);
    !dry_run && t.trade.stop_order_id.is_some()
}

/// stop 発火検知を 1 巡実行する。
///
/// 各対象 trade について `stop_order_status` を呼び、`Executed` なら
/// `close_trade(.., SlHit, ..)` で確定させる。`Active` / `Gone` は何もしない
/// (Gone + position 残存は margin alert / 手動対応領域。warn ログのみ)。
pub async fn detect_and_close_stop_fills(
    ctx: &CloseContext,
    open_trades: &[OpenTradeWithAccount],
    live_forces_dry_run: bool,
) {
    for owned in open_trades
        .iter()
        .filter(|t| is_stop_detection_target(t, live_forces_dry_run))
    {
        let trade = &owned.trade;
        let Some(stop_id) = &trade.stop_order_id else {
            continue;
        };
        let api = match ctx.apis.get(&trade.exchange) {
            Some(a) => a.clone(),
            None => {
                tracing::warn!(
                    "stop-fill detect: no ExchangeApi for {:?}; skipping trade {}",
                    trade.exchange,
                    trade.id
                );
                continue;
            }
        };

        match api.stop_order_status(&trade.pair.0, stop_id).await {
            Ok(StopOrderStatus::Executed { price, .. }) => {
                tracing::info!(
                    "stop-fill detect: stop {stop_id} for trade {} executed at {price}; closing as sl_hit",
                    trade.id
                );
                let account_name = owned
                    .account_name
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let account_type = owned
                    .account_type
                    .clone()
                    .unwrap_or_else(|| "live".to_string());
                // current_price は fallback。close_trade → fill_close のガードが
                // Executed を検出して stop 約定価格 (price) を exit_price に使うため、
                // ここでは best-effort な参照値 (無ければ stop_loss) を渡す。
                let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
                let current_price = ctx
                    .price_store
                    .latest_bid_ask(&feed_key)
                    .await
                    .map(|(bid, _ask)| bid)
                    .unwrap_or(trade.stop_loss);
                close_trade(
                    ctx,
                    trade,
                    account_name,
                    account_type,
                    false, // live
                    ExitReason::SlHit,
                    current_price,
                )
                .await;
            }
            Ok(StopOrderStatus::Active) => { /* まだ発火していない: 何もしない */ }
            Ok(StopOrderStatus::Gone) => {
                tracing::warn!(
                    "stop-fill detect: stop {stop_id} for trade {} is gone (canceled/expired) but \
                     the trade is still open — position may be unprotected (margin alert / manual)",
                    trade.id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "stop-fill detect: stop_order_status failed for trade {}: {e}",
                    trade.id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::{Direction, Exchange, Pair, Trade, TradeStatus};
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use uuid::Uuid;

    fn make_owned(account_type: &str, stop_order_id: Option<&str>) -> OpenTradeWithAccount {
        let trade = Trade {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            strategy_name: "s".into(),
            pair: Pair::new("USD_JPY"),
            exchange: Exchange::GmoFx,
            direction: Direction::Long,
            entry_price: dec!(150),
            exit_price: None,
            stop_loss: dec!(147),
            take_profit: None,
            quantity: dec!(1000),
            leverage: dec!(25),
            fees: dec!(0),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_amount: None,
            exit_reason: None,
            status: TradeStatus::Open,
            max_hold_until: None,
            exchange_position_id: None,
            stop_order_id: stop_order_id.map(|s| s.to_string()),
        };
        OpenTradeWithAccount {
            trade,
            account_name: Some("acct".into()),
            account_type: Some(account_type.into()),
        }
    }

    #[test]
    fn live_trade_with_stop_id_is_target() {
        let t = make_owned("live", Some("stop-1"));
        assert!(is_stop_detection_target(&t, false));
    }

    #[test]
    fn paper_trade_is_never_target() {
        let t = make_owned("paper", Some("stop-1"));
        assert!(!is_stop_detection_target(&t, false));
    }

    #[test]
    fn live_without_stop_id_is_not_target() {
        let t = make_owned("live", None);
        assert!(!is_stop_detection_target(&t, false));
    }

    #[test]
    fn live_forces_dry_run_excludes_live_trade() {
        let t = make_owned("live", Some("stop-1"));
        assert!(
            !is_stop_detection_target(&t, true),
            "LIVE_DRY_RUN で live trade も dry_run 扱いになり対象外"
        );
    }
}
