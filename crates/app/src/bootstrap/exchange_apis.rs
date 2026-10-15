use std::collections::HashMap;
use std::sync::Arc;

use auto_trader_core::config::AppConfig;
use auto_trader_core::types::Exchange;
use auto_trader_market::bitflyer_private::BitflyerPrivateApi;
use auto_trader_market::exchange_api::ExchangeApi;
use auto_trader_market::gmo_fx_private::GmoFxPrivateApi;
use auto_trader_market::oanda_private::OandaPrivateApi;

pub(crate) fn build(
    config: &AppConfig,
) -> anyhow::Result<Arc<HashMap<Exchange, Arc<dyn ExchangeApi>>>> {
    let mut exchange_apis: HashMap<Exchange, Arc<dyn ExchangeApi>> = HashMap::new();
    exchange_apis.insert(Exchange::BitflyerCfd, build_bitflyer_api(config)?);
    register_oanda(config, &mut exchange_apis)?;
    register_gmo_fx(&mut exchange_apis)?;
    Ok(Arc::new(exchange_apis))
}

fn build_bitflyer_api(config: &AppConfig) -> anyhow::Result<Arc<BitflyerPrivateApi>> {
    Ok(Arc::new(if let Some(bf) = config.bitflyer.as_ref() {
        let api_key = bf.api_key.clone().unwrap_or_default();
        let api_secret = bf.api_secret.clone().unwrap_or_default();
        BitflyerPrivateApi::new(api_key, api_secret)?
    } else {
        BitflyerPrivateApi::new(String::new(), String::new())?
    }))
}

fn register_oanda(
    config: &AppConfig,
    apis: &mut HashMap<Exchange, Arc<dyn ExchangeApi>>,
) -> anyhow::Result<()> {
    if let (Some(account_id), Some(api_key), Some(oanda_config)) = (
        super::resolve_oanda_account_id(config),
        super::resolve_oanda_api_key(),
        config.oanda.as_ref(),
    ) {
        let oanda_api: Arc<dyn ExchangeApi> = Arc::new(OandaPrivateApi::new(
            oanda_config.api_url.clone(),
            account_id,
            api_key,
        )?);
        apis.insert(Exchange::Oanda, oanda_api);
        tracing::info!(exchange = %Exchange::Oanda, "exchange api registered");
    } else {
        tracing::info!(
            exchange = %Exchange::Oanda,
            "no exchange api registered for exchange, requires OANDA_API_KEY env, an account id \
             (OANDA_ACCOUNT_ID env or [oanda].account_id config), and [oanda].api_url in config"
        );
    }
    Ok(())
}

fn register_gmo_fx(apis: &mut HashMap<Exchange, Arc<dyn ExchangeApi>>) -> anyhow::Result<()> {
    match (
        super::env_trimmed("GMO_API_KEY"),
        super::env_trimmed("GMO_API_SECRET"),
    ) {
        (Some(key), Some(secret)) => {
            let gmo_api: Arc<dyn ExchangeApi> = Arc::new(GmoFxPrivateApi::new(key, secret)?);
            apis.insert(Exchange::GmoFx, gmo_api);
            tracing::info!(exchange = %Exchange::GmoFx, "exchange api registered");
        }
        _ => tracing::info!(
            exchange = %Exchange::GmoFx,
            "no exchange api registered for exchange, requires GMO_API_KEY and GMO_API_SECRET env"
        ),
    }
    Ok(())
}
