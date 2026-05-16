//! paper account の維持率ロスカット判定。
//!
//! `Trader::close_position` を直接呼ばず、close 対象 trade_id の Vec を返すだけ
//! の pure 判定 helper (出力は close 計画)。`main.rs` の crypto monitor ループ
//! から呼び出され、戻り値を `closer::close_trade` で処理する。

use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::{OpenPosition, compute_maintenance_ratio};
use auto_trader_core::types::{Direction, Exchange};
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::{FeedKey, PriceStore};
use rust_decimal::Decimal;
use sqlx::PgPool;
use uuid::Uuid;

/// 維持率判定で参照する読み取り専用の環境。
/// price tick ループの先頭で 1 度組み立てて借用で渡す想定。
pub struct LiquidationContext {
    pub pool: PgPool,
    pub price_store: Arc<PriceStore>,
    pub exchange_liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    /// `LIVE_DRY_RUN=1` で起動した時、live account も paper 扱いで
    /// 維持率ロスカットを行うか。通常運用では false。
    pub live_forces_dry_run: bool,
}

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
    ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
) -> Vec<(Uuid, Vec<Uuid>)> {
    let threshold = match ctx.exchange_liquidation_levels.get(&event.exchange) {
        Some(t) => *t,
        None => return vec![], // 設定無しなら判定しない
    };

    // tick の exchange の open trade を account_id で bucketing (1 pass)。
    let mut buckets: HashMap<Uuid, Vec<&OpenTradeWithAccount>> = HashMap::new();
    for owned in open_trades
        .iter()
        .filter(|t| t.trade.exchange == event.exchange)
    {
        buckets
            .entry(owned.trade.account_id)
            .or_default()
            .push(owned);
    }

    // 同 pair の bid/ask 取得は read lock を取るので、tick 内で 1 回キャッシュ。
    let mut price_cache: HashMap<FeedKey, Option<(Decimal, Decimal)>> = HashMap::new();
    let mut results = Vec::new();

    for (account_id, trades_in_account) in buckets {
        // account_type 判定。paper のみ対象。
        let account_type = trades_in_account
            .first()
            .and_then(|t| t.account_type.as_deref())
            .unwrap_or("paper");
        let dry_run = account_type == "paper" || ctx.live_forces_dry_run;
        if !dry_run {
            continue;
        }

        // account row を read
        let account =
            match auto_trader_db::trading_accounts::get_account(&ctx.pool, account_id).await {
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
            let bid_ask = if let Some(cached) = price_cache.get(&feed_key) {
                *cached
            } else {
                let v = ctx.price_store.latest_bid_ask(&feed_key).await;
                price_cache.insert(feed_key.clone(), v);
                v
            };
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
