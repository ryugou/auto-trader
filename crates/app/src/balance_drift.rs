//! live 口座の残高ドリフト検知。bot は current_balance を DB 台帳で管理
//! するが、live では swap/SFD/手数料を取引所が直接徴収するため、実残高
//! とは徐々に乖離する。乖離を検知して運用者に知らせる (自動補正はしない
//! — 台帳の不変条件 current_balance = initial + Σpnl − Σfees を壊さない)。
//!
//! 起動時 + 毎時、live 口座について次を比較する:
//! - exchange_equity = get_collateral().collateral + open_position_pnl
//! - bot_equity      = current_balance + Σrequired_margin + Σunrealized_pnl
//!   (`compute_maintenance_ratio` の純資産 numerator と同じ式)
//!
//! LIVE_DRY_RUN 強制時は取引所残高が動かないためドリフト判定は無意味 →
//! `live_forces_dry_run == true` なら何もしない。

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use auto_trader_core::types::Exchange;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::price_store::PriceStore;
use auto_trader_notify::SystemAlertEvent;
use rust_decimal::Decimal;
use sqlx::PgPool;

use crate::positions::build_close_side_positions;

/// ドリフト判定。閾値 = max(取引所 equity の 1%, 500 円)。
pub fn is_drift(exchange_equity: Decimal, bot_equity: Decimal) -> bool {
    let threshold = (exchange_equity * Decimal::new(1, 2)).max(Decimal::from(500));
    (exchange_equity - bot_equity).abs() > threshold
}

/// 残高ドリフト検知が参照する読み取り専用の環境。
/// 起動時 one-shot / 毎時ジョブの両方から借用で渡す。
pub struct BalanceDriftContext {
    pub pool: PgPool,
    pub price_store: Arc<PriceStore>,
    pub apis: Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>,
    /// `LIVE_DRY_RUN=1` 起動時は取引所残高が動かないため判定を skip する。
    pub live_forces_dry_run: bool,
}

/// live 口座の bot equity と取引所 equity を照合し、ドリフトした口座の
/// `SystemAlertEvent` を返す。送信は呼び出し側 (main.rs) が行う。
///
/// 仕様:
/// - `account_type == "live"` のみ対象。`live_forces_dry_run` 時は空を返す。
/// - exchange API が無い / `get_collateral` が失敗した口座は warn して skip。
/// - open trade のうち PriceStore に bid/ask が無いものが 1 つでもあれば、
///   その口座は bot_equity を確定できないため skip (warn)。false-positive を避ける。
/// - 自動補正はしない (台帳不変条件を守る)。
pub async fn check_live_accounts(ctx: &BalanceDriftContext) -> Vec<SystemAlertEvent> {
    if ctx.live_forces_dry_run {
        return vec![];
    }

    let accounts = match auto_trader_db::trading_accounts::list_all(&ctx.pool).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("balance drift: failed to list accounts: {e}");
            return vec![];
        }
    };

    let mut alerts = Vec::new();

    for account in accounts
        .into_iter()
        .filter(|a| !crate::startup::effective_dry_run(&a.account_type, ctx.live_forces_dry_run))
    {
        let exchange = match Exchange::from_str(&account.exchange) {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(
                    "balance drift: account {} has unknown exchange '{}': {e}; skipping",
                    account.name,
                    account.exchange
                );
                continue;
            }
        };

        let api = match ctx.apis.get(&exchange) {
            Some(a) => a,
            None => {
                tracing::warn!(
                    "balance drift: no ExchangeApi for {:?} (account {}); skipping",
                    exchange,
                    account.name
                );
                continue;
            }
        };

        // 取引所 equity = collateral + 含み損益。
        let collateral = match api.get_collateral().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    "balance drift: get_collateral failed for {} ({:?}): {e}; skipping",
                    account.name,
                    exchange
                );
                continue;
            }
        };
        let exchange_equity = collateral.collateral + collateral.open_position_pnl;

        // bot equity = current_balance + Σrequired_margin + Σunrealized_pnl。
        // open trade を PriceStore の close-side bid/ask で OpenPosition 化する
        // (liquidation.rs と同じ方法)。price 不在があれば口座 skip。
        let open_trades =
            match auto_trader_db::trades::get_open_trades_by_account(&ctx.pool, account.id).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "balance drift: failed to read open trades for {}: {e}; skipping",
                        account.name
                    );
                    continue;
                }
            };

        let ctx_label = format!("balance drift: account {}", account.name);
        // one-shot 呼び出しなので cache は使い回さない。liquidation.rs /
        // margin_alert.rs と同じく名前付きローカルで持つ。
        let mut price_cache = HashMap::new();
        let positions = match build_close_side_positions(
            open_trades.iter(),
            &ctx.price_store,
            &mut price_cache,
            &ctx_label,
        )
        .await
        {
            Some(p) => p,
            None => continue,
        };

        let required: Decimal = positions.iter().map(|p| p.required_margin()).sum();
        let unrealized: Decimal = positions.iter().map(|p| p.unrealized_pnl()).sum();
        let bot_equity = account.current_balance + required + unrealized;

        if is_drift(exchange_equity, bot_equity) {
            let diff = exchange_equity - bot_equity;
            tracing::warn!(
                "balance drift DETECTED for {} ({:?}): exchange={exchange_equity} bot={bot_equity} diff={diff}",
                account.name,
                exchange
            );
            alerts.push(SystemAlertEvent {
                title: "balance drift".to_string(),
                account_name: account.name.clone(),
                exchange,
                body: format!(
                    "exchange equity={exchange_equity} bot equity={bot_equity} diff={diff} \
                     (alert only — no auto-correction; operator must reconcile deposits/swaps)"
                ),
            });
        }
    }

    alerts
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn drift_threshold_is_1pct_or_500yen_whichever_larger() {
        // exchange equity 30,000 → threshold = max(300, 500) = 500
        assert!(!is_drift(dec!(30000), dec!(30400)));
        assert!(is_drift(dec!(30000), dec!(30501)));
        // exchange equity 1,000,000 → threshold = max(10000, 500) = 10000
        assert!(!is_drift(dec!(1000000), dec!(1009999)));
        assert!(is_drift(dec!(1000000), dec!(1010001)));
    }
}
