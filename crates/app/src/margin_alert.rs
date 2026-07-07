//! live account の維持率アラート。paper のロスカット (liquidation.rs) と
//! 対になる live 側の監視。**close はしない** — live のロスカット執行は
//! 取引所の責務。bot は接近を運用者に知らせるだけ。
//! 注意: ここで使う残高は DB 管理値であり、取引所実残高とはドリフトしうる。

use auto_trader_core::event::PriceEvent;
use auto_trader_core::margin::compute_maintenance_ratio;
use auto_trader_db::trades::OpenTradeWithAccount;
use auto_trader_market::price_store::FeedKey;
use rust_decimal::Decimal;
use std::collections::HashMap;
use uuid::Uuid;

use crate::liquidation::LiquidationContext;
use crate::positions::build_close_side_positions;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertLevel {
    Warn,
    Critical,
}

impl AlertLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Critical => "critical",
        }
    }
}

/// 検出された 1 件の維持率アラート。close はせず、運用者通知の材料にする。
#[derive(Debug, Clone)]
pub struct MarginAlert {
    pub account_id: Uuid,
    pub account_name: String,
    pub ratio: Decimal,
    pub threshold: Decimal,
    pub level: AlertLevel,
}

/// 維持率 ratio をアラートレベルに分類する。pure 関数。
/// `Decimal::new(11, 1)` = 1.1, `Decimal::new(13, 1)` = 1.3。
pub fn classify_alert_level(ratio: Decimal, liquidation_level: Decimal) -> Option<AlertLevel> {
    let critical = liquidation_level * Decimal::new(11, 1); // Y × 1.1
    let warn = liquidation_level * Decimal::new(13, 1); // Y × 1.3
    if ratio < critical {
        Some(AlertLevel::Critical)
    } else if ratio < warn {
        Some(AlertLevel::Warn)
    } else {
        None
    }
}

/// `event` の tick が来た時、同 exchange の **live** account を walk して、
/// 維持率が warn/critical 帯に入った account の [`MarginAlert`] を返す。
///
/// [`detect_liquidation_targets`](crate::liquidation::detect_liquidation_targets)
/// の live 版ミラー。ただし **close はしない**。
///
/// 仕様:
/// - live account のみ対象 (`effective_dry_run == false`)。paper は
///   liquidation.rs が扱う。
/// - 同 account の trade のうち PriceStore に最新 bid/ask が無いものが
///   1 つでもあればその account 全体を skip (false-positive alert を避ける)。
/// - 維持率は `classify_alert_level` の帯 (Y×1.1 / Y×1.3) で分類する。
pub async fn detect_margin_alerts(
    ctx: &LiquidationContext,
    open_trades: &[OpenTradeWithAccount],
    event: &PriceEvent,
) -> Vec<MarginAlert> {
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
        // account_type 判定。live のみ対象 (liquidation.rs と逆)。
        let account_type = trades_in_account
            .first()
            .and_then(|t| t.account_type.as_deref())
            .unwrap_or("paper");
        let dry_run = crate::startup::effective_dry_run(account_type, ctx.live_forces_dry_run);
        if dry_run {
            continue;
        }

        // account row を read
        let account =
            match auto_trader_db::trading_accounts::get_account(&ctx.pool, account_id).await {
                Ok(Some(a)) => a,
                Ok(None) => {
                    tracing::warn!(
                        "margin_alert: account {account_id} not found (delete race?), skipping"
                    );
                    continue;
                }
                Err(e) => {
                    tracing::warn!("margin_alert: failed to read account {account_id}: {e}");
                    continue;
                }
            };

        // OpenPosition vec を組む。price 不在の trade があったら account 判定 skip
        // (false-positive alert を避ける、保守的)。
        let ctx_label = format!("margin_alert: account {account_id}");
        let positions = match build_close_side_positions(
            trades_in_account.iter().map(|owned| &owned.trade),
            &ctx.price_store,
            &mut price_cache,
            &ctx_label,
        )
        .await
        {
            Some(p) => p,
            None => continue,
        };

        // 維持率計算
        let ratio = match compute_maintenance_ratio(account.current_balance, &positions) {
            Some(r) => r,
            None => continue, // required=0、open 無し
        };

        if let Some(level) = classify_alert_level(ratio, threshold) {
            let account_name = trades_in_account
                .first()
                .and_then(|t| t.account_name.clone())
                .unwrap_or_else(|| account_id.to_string());
            tracing::warn!(
                "margin_alert: live account {account_id} maintenance_ratio={ratio} \
                 threshold={threshold} level={} — alerting (no auto-close)",
                level.as_str()
            );
            results.push(MarginAlert {
                account_id,
                account_name,
                ratio,
                threshold,
                level,
            });
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn classify_levels() {
        let y = dec!(0.50); // bitflyer_cfd
        assert_eq!(classify_alert_level(dec!(0.70), y), None); // >= 0.65 (Y×1.3)
        assert_eq!(classify_alert_level(dec!(0.64), y), Some(AlertLevel::Warn)); // < 0.65, >= 0.55
        assert_eq!(
            classify_alert_level(dec!(0.54), y),
            Some(AlertLevel::Critical)
        ); // < 0.55 (Y×1.1)
    }

    #[test]
    fn classify_boundaries_are_exclusive_upper() {
        let y = dec!(0.50);
        // Y×1.3 = 0.65 ちょうどは None (>= warn 閾値)。
        assert_eq!(classify_alert_level(dec!(0.65), y), None);
        // Y×1.1 = 0.55 ちょうどは Warn (>= critical 閾値、< warn 閾値)。
        assert_eq!(classify_alert_level(dec!(0.55), y), Some(AlertLevel::Warn));
    }
}
