use std::time::Duration;

pub const PIP_MILLI: i64 = 10;
pub const PIP_MID2: i64 = 20;
pub const M5_SECS: i64 = 300;

pub const GMO_KLINE_PATH: &str = "v1/klines";
pub const GMO_SYMBOL: &str = "USD_JPY";
pub const GMO_INTERVAL: &str = "5min";
pub const GMO_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
pub const GMO_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
pub const GMO_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

pub const SIM_EXCHANGE: &str = "gmo_fx";
pub const SIM_PAIR: &str = "USD_JPY";
pub const SIM_TIMEFRAME: &str = "M5";
pub const DB_MAX_CONNECTIONS: u32 = 5;
pub const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
pub const REQUIRED_TABLES: [&str; 4] = ["sim_candles", "sim_scripts", "sim_batches", "sim_runs"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub open_time: i64,
    pub bid_open: i64,
    pub bid_high: i64,
    pub bid_low: i64,
    pub bid_close: i64,
    pub ask_open: i64,
    pub ask_high: i64,
    pub ask_low: i64,
    pub ask_close: i64,
}

impl Bar {
    pub fn mid2_open(&self) -> i64 {
        self.bid_open + self.ask_open
    }

    pub fn mid2_high(&self) -> i64 {
        self.bid_high + self.ask_high
    }

    pub fn mid2_low(&self) -> i64 {
        self.bid_low + self.ask_low
    }

    pub fn mid2_close(&self) -> i64 {
        self.bid_close + self.ask_close
    }
}

pub fn milli_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MILLI as f64
}

pub fn mid2_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MID2 as f64
}

pub fn mid2_to_yen(v: i64) -> f64 {
    v as f64 / 2000.0
}

#[cfg(test)]
mod tests;
