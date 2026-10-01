//! 足の表現と、価格の単位変換。
//!
//! spec 3 章「数値の表現」の決定を型で固定する: 価格の比較・加減算はミリ円
//! （円 × 1000）または `mid2`（買値+売値、0.0005 円単位）の `i64` で行い、
//! `f64` への変換は出力・保存の時点で 1 回だけ行う。

use std::time::Duration;

/// 1 pip = 0.01 円 = 10 ミリ円。
pub const PIP_MILLI: i64 = 10;
/// 1 pip = `mid2`（bid_x + ask_x）の 20 単位。
pub const PIP_MID2: i64 = 20;
/// M5 足 1 本の長さ（秒）。
pub const M5_SECS: i64 = 300;

/// GMO KLine エンドポイントのパス(spec 5.2 章)。シンボル・足種はこの基盤専用で、
/// `config/default.toml` が変えるのは `gmo_public_base_url` だけ(spec 4 章)。
pub const GMO_KLINE_PATH: &str = "v1/klines";
/// GMO KLine のシンボル(spec 5.1 章: USD/JPY だけを扱う)。
pub const GMO_SYMBOL: &str = "USD_JPY";
/// GMO KLine の足種(spec 5.1 章: M5 だけを扱う)。
pub const GMO_INTERVAL: &str = "5min";
/// GMO 公開 API への HTTP リクエストのタイムアウト(spec 5.2 章)。ネットワーク障害時に
/// 取得が無期限にブロックしないための上限。
pub const GMO_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// リクエスト間隔の下限(spec 5.2 章: 1 秒以上あける)。
pub const GMO_MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(1);
/// 取得失敗時の再試行までの待ち時間(spec 5.2 章: 2 秒・4 秒・8 秒の最大 3 回)。
pub const GMO_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
];

/// `sim_candles.exchange` に保存する値(spec 5.1 章)。
pub const SIM_EXCHANGE: &str = "gmo_fx";
/// `sim_candles.pair` に保存する値(spec 5.1 章)。
pub const SIM_PAIR: &str = "USD_JPY";
/// `sim_candles.timeframe` に保存する値(spec 5.1 章)。
pub const SIM_TIMEFRAME: &str = "M5";
/// 接続プールの上限(計画 Task 2 Interfaces)。
pub const DB_MAX_CONNECTIONS: u32 = 5;
/// コネクション取得の上限(spec 12 章)。到達不能な DB に対して無期限にブロックしない
/// ための運用上の判断で、spec に値の明記はない。
pub const DB_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// 起動時に存在確認する 4 テーブル(spec 12 章)。
pub const REQUIRED_TABLES: [&str; 4] = ["sim_candles", "sim_scripts", "sim_batches", "sim_runs"];

/// GMO FX USD/JPY の M5 足 1 本。価格はすべてミリ円（円 × 1000）の整数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// 足の開始時刻（UTC エポック秒）。
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
    /// 始値の中値（`mid2` 単位 = bid + ask）。
    pub fn mid2_open(&self) -> i64 {
        self.bid_open + self.ask_open
    }

    /// 高値の中値。
    pub fn mid2_high(&self) -> i64 {
        self.bid_high + self.ask_high
    }

    /// 安値の中値。
    pub fn mid2_low(&self) -> i64 {
        self.bid_low + self.ask_low
    }

    /// 終値の中値。
    pub fn mid2_close(&self) -> i64 {
        self.bid_close + self.ask_close
    }
}

/// ミリ円を pips に変換する（出力・保存の直前にだけ呼ぶ）。
pub fn milli_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MILLI as f64
}

/// `mid2` 単位を pips に変換する（出力・保存の直前にだけ呼ぶ）。
pub fn mid2_to_pips(v: i64) -> f64 {
    v as f64 / PIP_MID2 as f64
}

/// `mid2` 単位を円に変換する（スクリプト・指標へ渡す直前にだけ呼ぶ）。
pub fn mid2_to_yen(v: i64) -> f64 {
    v as f64 / 2000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テスト用の `Bar` を作るヘルパー。`mid2_*` と pips 変換のテストで不要な
    /// フィールドには、意味のない値だと分かるよう `0` を入れる。
    fn bar(bid_close: i64, ask_close: i64) -> Bar {
        Bar {
            open_time: 0,
            bid_open: 0,
            bid_high: 0,
            bid_low: 0,
            bid_close,
            ask_open: 0,
            ask_high: 0,
            ask_low: 0,
            ask_close,
        }
    }

    #[test]
    fn mid2_close_sums_bid_and_ask() {
        let b = bar(150_000, 150_004);
        assert_eq!(b.mid2_close(), 300_004);
    }

    #[test]
    fn mid2_to_yen_divides_by_2000() {
        assert_eq!(mid2_to_yen(300_004), 150.002);
    }

    #[test]
    fn milli_to_pips_divides_by_10() {
        assert_eq!(milli_to_pips(25), 2.5);
    }

    #[test]
    fn mid2_to_pips_divides_by_20() {
        assert_eq!(mid2_to_pips(50), 2.5);
    }

    #[test]
    fn pip_constants_match_spec_3() {
        // 1 pip = 0.01 円 = 10 ミリ円 = mid2 の 20 単位 (spec 3 章)。
        assert_eq!(PIP_MILLI, 10);
        assert_eq!(PIP_MID2, 20);
        assert_eq!(M5_SECS, 300);
    }
}
