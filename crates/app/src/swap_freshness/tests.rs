use std::collections::HashMap;

use auto_trader_core::config::GmoFxSwapConfig;
use chrono::NaiveDate;
use rust_decimal_macros::dec;

use super::*;

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
    let updated_on = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
    let cfg = cfg_with_rate(Some("2026-07-07"), 35);
    let today = updated_on + chrono::Duration::days(36);
    let body = swap_freshness_alert(&cfg, today, true);
    assert!(body.is_some());
    assert!(body.unwrap().contains("2026-07-07"));
}

#[test]
fn fresh_rates_produce_no_alert() {
    let cfg = cfg_with_rate(Some("2026-07-07"), 35);
    let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
    assert!(swap_freshness_alert(&cfg, today, true).is_none());
}

#[test]
fn empty_rates_with_gmo_paper_accounts_produce_alert() {
    let cfg = empty_cfg();
    let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
    let body = swap_freshness_alert(&cfg, today, true);
    assert!(body.is_some());
    assert!(body.unwrap().contains("EMPTY"));
}

#[test]
fn empty_rates_without_gmo_paper_accounts_produce_no_alert() {
    let cfg = empty_cfg();
    let today = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
    assert!(swap_freshness_alert(&cfg, today, false).is_none());
}
