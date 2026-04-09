// Items here are used from enriched_ingest, weekly_batch, and main.rs (Task 7).
#![allow(dead_code)]

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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

/// Classify the current market regime from indicator values.
///
/// Priority order:
/// 1. HighVol — ATR percentile > 80 (dangerous regardless of direction).
/// 2. Trend — ADX > 25.
/// 3. Range — fallback when no strong signal.
///
/// EventWindow requires external data (news calendar) and is not
/// auto-detected here.
pub fn classify(indicators: &HashMap<String, Decimal>) -> MarketRegime {
    let adx = indicators.get("adx_14").copied();
    let atr_pct = indicators.get("atr_percentile").copied();

    // High volatility takes priority (dangerous regardless of trend)
    if let Some(pct) = atr_pct
        && pct > dec!(80)
    {
        return MarketRegime::HighVol;
    }
    // ADX > 25 = trending market
    if let Some(adx_val) = adx
        && adx_val > dec!(25)
    {
        return MarketRegime::Trend;
    }
    // Default: range-bound
    MarketRegime::Range
}

#[cfg(test)]
mod tests {
    use super::*;

    fn indicators(adx: f64, atr_pct: f64) -> HashMap<String, Decimal> {
        let mut m = HashMap::new();
        m.insert(
            "adx_14".to_string(),
            Decimal::try_from(adx).expect("adx is valid"),
        );
        m.insert(
            "atr_percentile".to_string(),
            Decimal::try_from(atr_pct).expect("atr_pct is valid"),
        );
        m
    }

    #[test]
    fn high_vol_takes_priority() {
        // ADX is high (trending) but ATR percentile > 80 → high_vol wins
        assert_eq!(classify(&indicators(30.0, 85.0)), MarketRegime::HighVol);
    }

    #[test]
    fn trend_when_adx_above_25() {
        assert_eq!(classify(&indicators(30.0, 50.0)), MarketRegime::Trend);
    }

    #[test]
    fn range_when_adx_below_25() {
        assert_eq!(classify(&indicators(20.0, 50.0)), MarketRegime::Range);
    }

    #[test]
    fn range_when_no_indicators() {
        assert_eq!(classify(&HashMap::new()), MarketRegime::Range);
    }
}
