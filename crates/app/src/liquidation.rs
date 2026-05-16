//! paper account の維持率ロスカット判定。
//!
//! `Trader::close_position` を直接呼ばず、close 対象 trade_id の Vec を返すだけ
//! の純粋な判定 helper。`main.rs` の crypto monitor ループから呼び出される。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::{OpenPosition, compute_maintenance_ratio};
use auto_trader_core::types::{Direction, Exchange};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// `event` の tick が来た時、同 exchange の paper account を walk して、
/// 維持率 `< threshold` の account の全 trade_id を返す。
///
/// 戻り値 `Vec<(account_id, vec_of_trade_ids)>`。caller が順次 close する。
///
/// 仕様:
/// - paper account のみ対象 (`account_type == "paper"` または
///   `live_forces_dry_run == true`)。live は exchange 側で自動なのでスキップ。
/// - 同 account の trade のうち、PriceStore に最新 bid/ask が無いものが
///   1 つでもあればその account 全体を skip (false-positive Liquidation を避ける)。
/// - 維持率は `< threshold` (厳密下回り) のみ発火。`==` では発火しない。
pub async fn detect_liquidation_targets(
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
    price_store: &Arc<PriceStore>,
    pool: &PgPool,
    exchange_liquidation_levels: &HashMap<Exchange, Decimal>,
    live_forces_dry_run: bool,
) -> Vec<(Uuid, Vec<Uuid>)> {
    let threshold = match exchange_liquidation_levels.get(&event.exchange) {
        Some(t) => *t,
        None => return vec![], // 設定無しなら判定しない
    };

    // tick の exchange に該当する account_id を抽出 (重複排除)
    let tick_accounts: HashSet<Uuid> = open_trades
        .iter()
        .filter(|t| t.trade.exchange == event.exchange)
        .map(|t| t.trade.account_id)
        .collect();

    let mut results = Vec::new();

    for account_id in tick_accounts {
        // 同 account の trades をフィルタ
        let trades_in_account: Vec<&OpenTradeWithAccount> = open_trades
            .iter()
            .filter(|t| t.trade.account_id == account_id)
            .collect();
        if trades_in_account.is_empty() {
            continue;
        }

        // account_type 判定。paper のみ対象。
        let account_type = trades_in_account
            .first()
            .and_then(|t| t.account_type.as_deref())
            .unwrap_or("paper");
        let dry_run = account_type == "paper" || live_forces_dry_run;
        if !dry_run {
            continue;
        }

        // account row を read
        let account = match auto_trader_db::trading_accounts::get_account(pool, account_id).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                tracing::warn!(
                    "liquidation: account {account_id} not found (delete race?), skipping"
                );
                continue;
            }
            Err(e) => {
                tracing::warn!("liquidation: failed to read account {account_id}: {e}");
                continue;
            }
        };

        // OpenPosition vec を組む。price 不在の trade があったら account 判定 skip
        // (false-positive Liquidation を避ける、保守的)。
        let mut positions = Vec::with_capacity(trades_in_account.len());
        let mut skip_account = false;
        for owned in &trades_in_account {
            let trade = &owned.trade;
            let feed_key = FeedKey::new(trade.exchange, trade.pair.clone());
            let bid_ask = price_store.latest_bid_ask(&feed_key).await;
            let current_price = match bid_ask {
                Some((bid, ask)) => match trade.direction {
                    // close-side bid/ask: Long close=bid, Short close=ask
                    Direction::Long => bid,
                    Direction::Short => ask,
                },
                None => {
                    tracing::warn!(
                        "liquidation: no price for {:?} {} — skipping account {account_id}",
                        trade.exchange,
                        trade.pair
                    );
                    skip_account = true;
                    break;
                }
            };
            positions.push(OpenPosition {
                direction: trade.direction,
                entry_price: trade.entry_price,
                current_price,
                quantity: trade.quantity,
                leverage: trade.leverage,
            });
        }
        if skip_account {
            continue;
        }

        // 維持率計算
        let ratio = match compute_maintenance_ratio(account.current_balance, &positions) {
            Some(r) => r,
            None => continue, // required=0、open 無し
        };

        if ratio < threshold {
            tracing::warn!(
                "liquidation: account {account_id} maintenance_ratio={ratio} < threshold={threshold} \
                 — force-closing {} trade(s)",
                trades_in_account.len()
            );
            let trade_ids: Vec<Uuid> = trades_in_account.iter().map(|t| t.trade.id).collect();
            results.push((account_id, trade_ids));
        }
    }

    results
}
