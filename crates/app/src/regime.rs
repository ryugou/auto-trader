use std::collections::HashMap;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRegime {
    Trend,
    Range,
    HighVol,
    EventWindow,
}

impl MarketRegime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trend => "trend",
            Self::Range => "range",
            Self::HighVol => "high_vol",
            Self::EventWindow => "event_window",
        }
    }
}

const HIGH_VOL_ATR_PERCENTILE_THRESHOLD: Decimal = dec!(80);
const TREND_ADX_THRESHOLD: Decimal = dec!(25);

pub fn classify(indicators: &HashMap<String, Decimal>) -> MarketRegime {
    let adx = indicators.get("adx_14").copied();
    let atr_pct = indicators.get("atr_percentile").copied();

    if let Some(pct) = atr_pct
        && pct > HIGH_VOL_ATR_PERCENTILE_THRESHOLD
    {
        return MarketRegime::HighVol;
    }
    if let Some(adx_val) = adx
        && adx_val > TREND_ADX_THRESHOLD
    {
        return MarketRegime::Trend;
    }
    MarketRegime::Range
}

#[cfg(test)]
mod tests;
