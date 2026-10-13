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
