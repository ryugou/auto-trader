use crate::series::Tf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorKind {
    Sma,
    Ema,
    Rsi,
    Atr,
    Adx,
    Bb,
    Donchian,
    Keltner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorKey {
    pub tf: Tf,
    pub kind: IndicatorKind,
    pub period: u32,
    pub mult_x100: u32,
}

#[derive(Debug, Clone)]
pub enum IndicatorSeries {
    Single(Vec<f64>),
    Band {
        lower: Vec<f64>,
        middle: Vec<f64>,
        upper: Vec<f64>,
    },
    Channel {
        lower: Vec<f64>,
        upper: Vec<f64>,
    },
}
