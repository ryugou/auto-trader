use crate::series::Tf;

/// Kind of indicator that a script can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndicatorKind {
    /// Simple moving average of the closes.
    Sma,
    /// Exponential moving average of the closes.
    Ema,
    /// Relative strength index of the closes.
    Rsi,
    /// Average true range.
    Atr,
    /// Average directional index.
    Adx,
    /// Bollinger bands.
    Bb,
    /// Donchian channel.
    Donchian,
    /// Keltner channel.
    Keltner,
}

/// Identity of an indicator series: the timeframe, the kind and the parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IndicatorKey {
    /// Timeframe of the bars that the indicator is computed from.
    pub tf: Tf,
    /// Kind of the indicator.
    pub kind: IndicatorKind,
    /// Number of bars of the indicator window.
    pub period: u32,
    /// Band multiplier times 100, used by `Bb` and `Keltner`.
    pub mult_x100: u32,
}

/// Values of an indicator for each bar, with `NaN` where the indicator is not yet defined.
#[derive(Debug, Clone)]
pub enum IndicatorSeries {
    /// One value per bar.
    Single(Vec<f64>),
    /// Three lines per bar.
    Band {
        /// Lower line.
        lower: Vec<f64>,
        /// Middle line.
        middle: Vec<f64>,
        /// Upper line.
        upper: Vec<f64>,
    },
    /// Two lines per bar.
    Channel {
        /// Lower line.
        lower: Vec<f64>,
        /// Upper line.
        upper: Vec<f64>,
    },
}
