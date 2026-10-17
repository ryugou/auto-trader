use auto_trader_core::config::AppConfig;

#[derive(Clone, Copy)]
pub(crate) struct LiveFlags {
    pub(crate) forces_dry_run: bool,
    pub(crate) enabled: bool,
}

pub(crate) fn resolve(config: &AppConfig) -> LiveFlags {
    let live_cfg_dry_run = config.live.as_ref().is_some_and(|l| l.dry_run);
    let forces_dry_run = parse_live_dry_run_override(
        std::env::var("LIVE_DRY_RUN").ok().as_deref(),
        live_cfg_dry_run,
    );
    let enabled = config.live.as_ref().is_some_and(|l| l.enabled);
    LiveFlags {
        forces_dry_run,
        enabled,
    }
}

pub(crate) fn log_ignored_position_sizing(config: &AppConfig) {
    if let Some(ps) = config.position_sizing.as_ref() {
        tracing::info!(
            method = %ps.method,
            risk_rate = %ps.risk_rate,
            "position_sizing config is ignored, sizing is now per-strategy via signal allocation_pct"
        );
    }
}

fn parse_live_dry_run_override(raw: Option<&str>, config_dry_run: bool) -> bool {
    match raw {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                tracing::warn!(
                    value = ?raw,
                    fallback = config_dry_run,
                    "invalid env var \"LIVE_DRY_RUN\": unrecognized value, accepted values are \
                     1/true/yes/on/0/false/no/off, falling back to configured value"
                );
                config_dry_run
            }
        },
        None => config_dry_run,
    }
}

#[cfg(test)]
mod tests;
