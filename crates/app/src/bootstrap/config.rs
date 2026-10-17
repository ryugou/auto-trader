use std::path::PathBuf;

use auto_trader_core::config::AppConfig;

pub(crate) fn load() -> anyhow::Result<AppConfig> {
    let config_path = resolve_config_path(std::env::var("CONFIG_PATH").ok());
    let mut config = AppConfig::load(&PathBuf::from(&config_path))?;
    tracing::info!(config_path = %config_path, "config loaded");
    override_bitflyer_credentials(
        &mut config,
        non_empty(std::env::var("BITFLYER_API_KEY").ok()),
        non_empty(std::env::var("BITFLYER_API_SECRET").ok()),
    );
    Ok(config)
}

fn resolve_config_path(env_value: Option<String>) -> String {
    env_value.unwrap_or_else(|| "config/default.toml".to_string())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

fn override_bitflyer_credentials(
    config: &mut AppConfig,
    api_key: Option<String>,
    api_secret: Option<String>,
) {
    if let Some(bf) = config.bitflyer.as_mut() {
        bf.api_key = api_key;
        bf.api_secret = api_secret;
    }
}

#[cfg(test)]
mod tests;
