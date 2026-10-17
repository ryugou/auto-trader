use std::time::Duration;

/// Number of milli units (0.001 yen) in one pip.
pub const PIP_MILLI: i64 = 10;
/// Number of mid2 units (the sum of a bid and an ask in milli units) in one pip.
pub const PIP_MID2: i64 = 20;
/// Number of seconds in an M5 bar.
pub const M5_SECS: i64 = 300;
pub(crate) const FLOAT_GRID_TOLERANCE: f64 = 1e-9;

/// Path of the GMO kline endpoint, relative to the public base URL.
pub const GMO_KLINE_PATH: &str = "v1/klines";
/// Symbol requested from the GMO kline endpoint.
pub const GMO_SYMBOL: &str = "USD_JPY";
/// Interval requested from the GMO kline endpoint.
pub const GMO_INTERVAL: &str = "5min";
/// Timeout of one HTTP request to GMO.
pub const GMO_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Minimum interval between the starts of two requests to GMO.
pub const GMO_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
/// Delays before the successive retries of a failed request to GMO.
pub const GMO_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

/// Exchange value of the stored candles.
pub const SIM_EXCHANGE: &str = "gmo_fx";
/// Pair value of the stored candles.
pub const SIM_PAIR: &str = "USD_JPY";
/// Timeframe value of the stored candles.
pub const SIM_TIMEFRAME: &str = "M5";
/// Maximum number of connections of the database pool.
pub const DB_MAX_CONNECTIONS: u32 = 5;
/// Timeout for acquiring a connection from the database pool.
pub const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Tables that the simulator requires in the database.
pub const REQUIRED_TABLES: [&str; 4] = ["sim_candles", "sim_scripts", "sim_batches", "sim_runs"];

/// M5 bar of bid and ask prices in milli units (0.001 yen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// Open time of the bar in Unix seconds.
    pub open_time: i64,
    /// Bid price at the open.
    pub bid_open: i64,
    /// Highest bid price.
    pub bid_high: i64,
    /// Lowest bid price.
    pub bid_low: i64,
    /// Bid price at the close.
    pub bid_close: i64,
    /// Ask price at the open.
    pub ask_open: i64,
    /// Highest ask price.
    pub ask_high: i64,
    /// Lowest ask price.
    pub ask_low: i64,
    /// Ask price at the close.
    pub ask_close: i64,
}

impl Bar {
    /// Returns the sum of the bid and ask open prices.
    pub fn mid2_open(&self) -> i64 {
        self.bid_open + self.ask_open
    }

    /// Returns the sum of the bid and ask high prices.
    pub fn mid2_high(&self) -> i64 {
        self.bid_high + self.ask_high
    }

    /// Returns the sum of the bid and ask low prices.
    pub fn mid2_low(&self) -> i64 {
        self.bid_low + self.ask_low
    }

    /// Returns the sum of the bid and ask close prices.
    pub fn mid2_close(&self) -> i64 {
        self.bid_close + self.ask_close
    }
}

/// Converts a value in milli units to pips.
pub fn milli_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MILLI as f64
}

/// Converts a value in mid2 units to pips.
pub fn mid2_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MID2 as f64
}

/// Converts a value in mid2 units to yen.
pub fn mid2_to_yen(v: i64) -> f64 {
    v as f64 / 2000.0
}

#[cfg(test)]
mod tests;
