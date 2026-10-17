use auto_trader_core::config::GmoFxSwapConfig;
use auto_trader_core::swap::is_swap_rates_stale;
use chrono::NaiveDate;

/// Returns the alert text if `cfg.rates` is empty and `has_gmo_paper_accounts` is true, or if `cfg.rates` is not empty and `updated_on` is more than `cfg.max_age_days` days before `today`; returns `None` otherwise, including when `cfg.rates` is not empty and `updated_on` is unset or is not a valid date.
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
mod tests;
