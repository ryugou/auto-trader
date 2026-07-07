//! GMO swap rate 表の鮮度チェック。日次スワップジョブから 1 日 1 回呼ばれ、
//! 「rates が古い」「rates 未設定なのに GMO paper 口座が存在する」を
//! 運用者向けアラート文言として返す。判定のみ (送信は main.rs)。

use auto_trader_core::config::GmoFxSwapConfig;
use auto_trader_core::swap::is_swap_rates_stale;
use chrono::NaiveDate;

/// アラートが必要なら本文を返す。不要なら None。
///
/// - rates 空 + GMO paper 口座あり → 未設定警告 (paper=live 近似が欠ける)
/// - rates あり + updated_on が max_age_days 超過 → 更新督促
pub fn swap_freshness_alert(
    cfg: &GmoFxSwapConfig,
    today: NaiveDate,
    has_gmo_paper_accounts: bool,
) -> Option<String> {
    if cfg.rates.is_empty() {
        if has_gmo_paper_accounts {
            return Some(
                "[gmo_fx.swap.rates] is EMPTY — GMO paper accounts are running WITHOUT \
                 swap simulation (paper PnL is optimistic). Fill rates + updated_on from \
                 the official swap calendar."
                    .to_string(),
            );
        }
        return None;
    }
    // validate 済みなので parse は成功するはずだが、防御的に None 扱い。
    let updated_on = cfg.parsed_updated_on().ok().flatten()?;
    if is_swap_rates_stale(updated_on, today, cfg.max_age_days) {
        let age = (today - updated_on).num_days();
        return Some(format!(
            "[gmo_fx.swap.rates] last verified {updated_on} ({age} days ago, limit {} days) — \
             re-check the official GMO swap calendar and update rates + updated_on",
            cfg.max_age_days
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use auto_trader_core::config::GmoFxSwapConfig;
    use chrono::NaiveDate;
    use rust_decimal_macros::dec;
    use std::collections::HashMap;

    fn cfg_with_rate(updated_on: Option<&str>, max_age_days: u32) -> GmoFxSwapConfig {
        let mut rates = HashMap::new();
        rates.insert(
            "USD_JPY".to_string(),
            auto_trader_core::config::SwapRateEntry {
                long: dec!(100),
                short: dec!(-120),
            },
        );
        GmoFxSwapConfig {
            rates,
            updated_on: updated_on.map(str::to_string),
            max_age_days,
        }
    }

    fn empty_cfg() -> GmoFxSwapConfig {
        GmoFxSwapConfig {
            rates: HashMap::new(),
            updated_on: None,
            max_age_days: 35,
        }
    }

    #[test]
    fn stale_rates_produce_alert() {
        let cfg = cfg_with_rate(Some("2026-07-07"), 35);
        let today = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(); // +36 days
        let body = super::swap_freshness_alert(&cfg, today, true);
        assert!(body.is_some());
        assert!(body.unwrap().contains("2026-07-07"));
    }

    #[test]
    fn fresh_rates_produce_no_alert() {
        let cfg = cfg_with_rate(Some("2026-07-07"), 35);
        let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
        assert!(super::swap_freshness_alert(&cfg, today, true).is_none());
    }

    #[test]
    fn empty_rates_with_gmo_paper_accounts_produce_alert() {
        let cfg = empty_cfg();
        let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
        let body = super::swap_freshness_alert(&cfg, today, true);
        assert!(body.is_some());
        assert!(body.unwrap().contains("EMPTY"));
    }

    #[test]
    fn empty_rates_without_gmo_paper_accounts_produce_no_alert() {
        let cfg = empty_cfg();
        let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
        assert!(super::swap_freshness_alert(&cfg, today, false).is_none());
    }
}
