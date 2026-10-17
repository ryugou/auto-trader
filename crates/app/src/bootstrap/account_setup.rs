use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::config::{AppConfig, PairConfig};
use auto_trader_core::types::Exchange;
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_executor::position_sizer::PositionSizer;
use auto_trader_notify::Notifier;
use rust_decimal::Decimal;

use super::live_flags::{self, LiveFlags};
use super::{accounts, fx_new_assumptions, position_sizer};
use crate::runtime::pipeline::RiskLimits;

pub(crate) struct AccountSetup {
    pub(crate) db_accounts: Vec<TradingAccount>,
    pub(crate) live: LiveFlags,
    pub(crate) liquidation_levels: Arc<HashMap<Exchange, Decimal>>,
    pub(crate) position_sizer: Arc<PositionSizer>,
    pub(crate) limits: RiskLimits,
}

pub(crate) async fn resolve(
    config: &AppConfig,
    pool: &sqlx::PgPool,
    registered_strategies: &[String],
    notifier: &Arc<Notifier>,
) -> anyhow::Result<AccountSetup> {
    let db_accounts =
        accounts::load_and_log(pool, registered_strategies, &config.gmo_fx.swap).await?;

    let live = live_flags::resolve(config);
    live_flags::log_ignored_position_sizing(config);

    let pair_configs: Arc<HashMap<String, PairConfig>> = Arc::new(config.pair_config.clone());

    let liquidation_levels: Arc<HashMap<Exchange, Decimal>> = Arc::new(
        auto_trader::startup::resolve_exchange_liquidation_levels(&db_accounts, config)?,
    );

    fx_new_assumptions::alert_mismatches(&db_accounts, notifier);

    let risk = config.risk.clone().unwrap_or_default();
    let sizing_margin_buffer: Decimal = risk.sizing_margin_buffer;
    let position_sizer = position_sizer::build(&pair_configs, sizing_margin_buffer);

    Ok(AccountSetup {
        db_accounts,
        live,
        liquidation_levels,
        position_sizer,
        limits: RiskLimits {
            price_freshness_secs: risk.price_freshness_secs,
            daily_loss_limit_pct: risk.daily_loss_limit_pct,
            halt_hours: risk.halt_hours,
        },
    })
}
