use std::collections::HashMap;

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Regime of the market, as classified from its indicators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketRegime {
    /// The trend strength `adx_14` is above 25 and the volatility is not high.
    Trend,
    /// Neither the volatility is high nor the trend strong; a missing indicator counts as not above its threshold.
    Range,
    /// The volatility percentile `atr_percentile` is above 80.
    HighVol,
    /// A regime that `classify` never returns.
    EventWindow,
}

impl MarketRegime {
    /// Returns the snake case name of the regime: `"trend"`, `"range"`, `"high_vol"` or `"event_window"`.
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

/// Returns `HighVol` if the indicator `atr_percentile` of `indicators` is above 80, otherwise `Trend` if `adx_14` is above 25, and `Range` otherwise; a missing indicator counts as not above its threshold.
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
