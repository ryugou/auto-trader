use auto_trader_core::config::AppConfig;

pub(crate) mod accounts;
pub(crate) mod config;
pub(crate) mod exchange_apis;
pub(crate) mod feeds;
pub(crate) mod fx_monitor;
pub(crate) mod fx_new_assumptions;
pub(crate) mod knowledge_store;
pub(crate) mod live_accounts;
pub(crate) mod live_flags;
pub(crate) mod logging;
pub(crate) mod macro_analyst;
pub(crate) mod position_sizer;
pub(crate) mod strategies;

pub(crate) fn resolve_oanda_account_id(config: &AppConfig) -> Option<String> {
    env_trimmed("OANDA_ACCOUNT_ID").or_else(|| {
        config
            .oanda
            .as_ref()
            .and_then(|c| trimmed_non_empty(c.account_id.clone()))
    })
}

pub(crate) fn resolve_oanda_api_key() -> Option<String> {
    env_trimmed("OANDA_API_KEY")
}

pub(crate) fn env_trimmed(name: &str) -> Option<String> {
    std::env::var(name).ok().and_then(trimmed_non_empty)
}

fn trimmed_non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests;
