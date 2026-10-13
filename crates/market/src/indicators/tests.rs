use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;

use super::*;

#[test]
fn sma_basic() {
    let prices = vec![dec!(1), dec!(2), dec!(3), dec!(4), dec!(5)];
    assert_eq!(sma(&prices, 3), Some(dec!(4)));
}

#[test]
fn sma_insufficient_data() {
    let prices = vec![dec!(1), dec!(2)];
    assert_eq!(sma(&prices, 3), None);
}

#[test]
fn rsi_all_gains() {
    let prices: Vec<Decimal> = (0..=14).map(Decimal::from).collect();
    let result = rsi(&prices, 14).unwrap();
    assert_eq!(result, dec!(100));
}

#[test]
fn rsi_mixed() {
    let prices = vec![
        dec!(44),
        dec!(44.34),
        dec!(44.09),
        dec!(43.61),
        dec!(44.33),
        dec!(44.83),
        dec!(45.10),
        dec!(45.42),
        dec!(45.84),
        dec!(46.08),
        dec!(45.89),
        dec!(46.03),
        dec!(45.61),
        dec!(46.28),
        dec!(46.28),
    ];
    let result = rsi(&prices, 14).unwrap();
    let f = result.to_f64().unwrap();
    assert!(f > 60.0 && f < 80.0, "RSI should be ~70, got {f}");
}

#[test]
fn bollinger_bands_centers_on_sma_and_widens_with_volatility() {
    let flat = vec![dec!(100); 20];
    let (lo, mid, up) = bollinger_bands(&flat, 20, dec!(2)).unwrap();
    assert_eq!(mid, dec!(100));
    assert_eq!(lo, dec!(100));
    assert_eq!(up, dec!(100));

    let alt: Vec<Decimal> = (0..20)
        .map(|i| if i % 2 == 0 { dec!(100) } else { dec!(102) })
        .collect();
    let (lo, mid, up) = bollinger_bands(&alt, 20, dec!(2)).unwrap();
    assert_eq!(mid, dec!(101));
    let lo_f = lo.to_f64().unwrap();
    let up_f = up.to_f64().unwrap();
    assert!((lo_f - 99.0).abs() < 0.01);
    assert!((up_f - 103.0).abs() < 0.01);
}

#[test]
fn bollinger_bands_insufficient_data_returns_none() {
    let prices = vec![dec!(1); 5];
    assert!(bollinger_bands(&prices, 20, dec!(2)).is_none());
}

#[test]
fn atr_basic_constant_range() {
    let highs: Vec<Decimal> = (0..20).map(|_| dec!(110)).collect();
    let lows: Vec<Decimal> = (0..20).map(|_| dec!(100)).collect();
    let closes: Vec<Decimal> = (0..20).map(|_| dec!(105)).collect();
    let v = atr(&highs, &lows, &closes, 14).unwrap();
    assert_eq!(v, dec!(10));
}

#[test]
fn atr_includes_gap_in_true_range() {
    let highs = vec![dec!(110), dec!(130)];
    let lows = vec![dec!(100), dec!(120)];
    let closes = vec![dec!(105), dec!(125)];
    let v = atr(&highs, &lows, &closes, 1).unwrap();
    assert_eq!(v, dec!(25));
}

#[test]
fn donchian_channel_excludes_current_by_default() {
    let highs = vec![dec!(10), dec!(20), dec!(15), dec!(50)];
    let lows = vec![dec!(5), dec!(8), dec!(7), dec!(40)];
    let (lo, up) = donchian_channel(&highs, &lows, 3, false).unwrap();
    assert_eq!(lo, dec!(5));
    assert_eq!(up, dec!(20));

    let (lo, up) = donchian_channel(&highs, &lows, 3, true).unwrap();
    assert_eq!(lo, dec!(7));
    assert_eq!(up, dec!(50));
}

#[test]
fn donchian_channel_empty_input_returns_none() {
    let highs: Vec<Decimal> = vec![];
    let lows: Vec<Decimal> = vec![];
    assert_eq!(donchian_channel(&highs, &lows, 3, false), None);
    assert_eq!(donchian_channel(&highs, &lows, 3, true), None);
}

#[test]
fn keltner_channels_widen_with_atr() {
    let closes: Vec<Decimal> = (0..20).map(|i| dec!(100) + Decimal::from(i)).collect();
    let highs: Vec<Decimal> = closes.iter().map(|c| *c + dec!(0.5)).collect();
    let lows: Vec<Decimal> = closes.iter().map(|c| *c - dec!(0.5)).collect();
    let (lo, mid, up) = keltner_channels(&highs, &lows, &closes, 14, dec!(1.5)).unwrap();
    assert!(mid > dec!(100) && mid < dec!(120));
    assert!(lo < mid && mid < up);
    let band_width = up - lo;
    let f = band_width.to_f64().unwrap();
    assert!(f > 4.0 && f < 5.0, "expected band width ~4.5, got {f}");
}

#[test]
fn indicator_map_has_full_key_set_for_sufficient_bars() {
    let closes: Vec<Decimal> = (0..60)
        .map(|i| Decimal::from(100 + (i % 7) * 3 + i / 5))
        .collect();
    let highs: Vec<Decimal> = closes.iter().map(|c| *c + dec!(2)).collect();
    let lows: Vec<Decimal> = closes.iter().map(|c| *c - dec!(2)).collect();

    let map = indicator_map(&closes, &highs, &lows);

    let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "adx_14",
            "atr_14",
            "atr_percentile",
            "bb_width_pct",
            "rsi_14",
            "sma_20",
            "sma_50",
        ]
    );
}
