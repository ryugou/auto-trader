use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Pair(pub String);

impl Pair {
    pub fn new(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl std::fmt::Display for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    Oanda,
    BitflyerCfd,
}

impl Exchange {
    pub fn as_str(&self) -> &'static str {
        match self {
            Exchange::Oanda => "oanda",
            Exchange::BitflyerCfd => "bitflyer_cfd",
        }
    }
}

impl std::fmt::Display for Exchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Long,
    Short,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Long => "long",
            Direction::Short => "short",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    /// 中間ロック状態: Trader が close を確定する前に取引所 API へ
    /// 反対売買を発注している間、他の close 経路が並行で同じ trade に
    /// 発注しないよう所有権を CAS で取得した状態。
    /// API 失敗時は Open に戻す。成功時は Closed に遷移する。
    Closing,
    Closed,
}

impl TradeStatus {
    /// DB bind 時 / SQL 比較で使う静的文字列。`serde_json::to_string` の
    /// round-trip (`"open"` → `open`) を踏まず 1 アロケーションで済む。
    pub fn as_str(&self) -> &'static str {
        match self {
            TradeStatus::Open => "open",
            TradeStatus::Closing => "closing",
            TradeStatus::Closed => "closed",
        }
    }
}

impl std::fmt::Display for TradeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitReason {
    TpHit,
    SlHit,
    Manual,
    SignalReverse,
    /// Mean-reversion target reached (e.g. price returned to BB middle).
    /// Strategy-driven exit emitted via `Strategy::on_open_positions`.
    StrategyMeanReached,
    /// Trailing channel break (e.g. Donchian opposite-side break).
    StrategyTrailingChannel,
    /// Trailing moving-average break (e.g. close beyond EMA(21)).
    StrategyTrailingMa,
    /// Indicator-based reversal (e.g. RSI crossed back through midline).
    StrategyIndicatorReversal,
    /// Time-based fail-safe — `max_hold_until` deadline reached.
    StrategyTimeLimit,
    /// Closed by the startup reconciler: the exchange position was gone but
    /// Phase 3 (DB update) never completed before a process crash.
    /// Distinguishes reconciled closes from normal SL/TP/manual exits in
    /// audit queries; previously written as a raw &str which caused the
    /// TradeRow mapper to bail on unknown strings.
    Reconciled,
}

impl ExitReason {
    /// DB bind / display string — avoids `serde_json::to_string` + `trim_matches('"')` round-trip.
    pub fn as_str(&self) -> &'static str {
        match self {
            ExitReason::TpHit => "tp_hit",
            ExitReason::SlHit => "sl_hit",
            ExitReason::Manual => "manual",
            ExitReason::SignalReverse => "signal_reverse",
            ExitReason::StrategyMeanReached => "strategy_mean_reached",
            ExitReason::StrategyTrailingChannel => "strategy_trailing_channel",
            ExitReason::StrategyTrailingMa => "strategy_trailing_ma",
            ExitReason::StrategyIndicatorReversal => "strategy_indicator_reversal",
            ExitReason::StrategyTimeLimit => "strategy_time_limit",
            ExitReason::Reconciled => "reconciled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    /// Stop-loss distance as a fraction of fill price (required).
    /// e.g. 0.005 means SL at fill_price × (1 ∓ 0.005).
    pub stop_loss_pct: Decimal,
    /// Take-profit distance as a fraction of fill price.
    /// `None` for strategies using dynamic exit logic.
    #[serde(default)]
    pub take_profit_pct: Option<Decimal>,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
    /// Fraction of leveraged account capacity the strategy wants to
    /// commit to this trade. Must be in (0, 1].
    ///
    /// The sizer turns this into a quantity via
    /// `floor((balance × leverage × allocation_pct / price) / min_lot)`.
    /// `allocation_pct` is the **only** sizing knob the strategy gets;
    /// chart-derived values (SL distance, ATR, …) intentionally do not
    /// influence quantity, matching the layering "signal = chart,
    /// execution = balance".
    #[serde(default = "default_allocation_pct")]
    pub allocation_pct: Decimal,
    /// Optional time-based fail-safe: position monitor will force-close
    /// the trade at this UTC time even if neither SL nor TP nor any
    /// strategy-driven exit has fired. Strategies use this to bound
    /// "stale" trades (e.g. mean-reversion 24h, vol-breakout 48h).
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}

fn default_allocation_pct() -> Decimal {
    // Conservative default: half the account's capacity. Strategies
    // SHOULD set their own value rather than rely on this.
    rust_decimal::Decimal::new(5, 1) // 0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    /// Links to trading_accounts; account_type on that row carries Live/Paper/Backtest.
    pub account_id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    /// Actual fill price at entry.
    pub entry_price: Decimal,
    /// Actual fill price at exit.
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    /// None for strategies using dynamic exit logic.
    pub take_profit: Option<Decimal>,
    pub quantity: Decimal,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    /// Trade lifecycle status: Open → Closing (live close in flight) → Closed.
    /// Closing is a short-lived intermediate held while the exchange API
    /// fill is pending; the 5-min stale-lock recovery in
    /// `db::trades::acquire_close_lock` adopts orphans automatically.
    pub status: TradeStatus,
    /// Optional time-based fail-safe — see `Signal::max_hold_until`.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub trade: Trade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub pair: Pair,
    pub exchange: Exchange,
    pub timeframe: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Option<u64>,
    /// Best bid price at candle close. `None` for data sources that do not
    /// provide bid/ask (e.g. OANDA mid-price candles).
    #[serde(default)]
    pub best_bid: Option<Decimal>,
    /// Best ask price at candle close. `None` for data sources that do not
    /// provide bid/ask (e.g. OANDA mid-price candles).
    #[serde(default)]
    pub best_ask: Option<Decimal>,
    pub timestamp: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn pair_display_format() {
        let pair = Pair::new("USD_JPY");
        assert_eq!(pair.to_string(), "USD_JPY");
    }

    #[test]
    fn direction_serializes_snake_case() {
        let json = serde_json::to_string(&Direction::Long).unwrap();
        assert_eq!(json, r#""long""#);
    }

    #[test]
    fn exchange_serializes_snake_case() {
        let json = serde_json::to_string(&Exchange::BitflyerCfd).unwrap();
        assert_eq!(json, r#""bitflyer_cfd""#);
        let json = serde_json::to_string(&Exchange::Oanda).unwrap();
        assert_eq!(json, r#""oanda""#);
    }

    #[test]
    fn exchange_display() {
        assert_eq!(Exchange::Oanda.as_str(), "oanda");
        assert_eq!(Exchange::BitflyerCfd.as_str(), "bitflyer_cfd");
    }

    #[test]
    fn signal_roundtrip() {
        let signal = Signal {
            strategy_name: "test".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            stop_loss_pct: dec!(0.005),
            take_profit_pct: Some(dec!(0.01)),
            confidence: 0.8,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.5),
            max_hold_until: None,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pair, signal.pair);
        assert_eq!(back.direction, Direction::Long);
    }

    #[test]
    fn trade_status_roundtrip() {
        for status in [TradeStatus::Open, TradeStatus::Closing, TradeStatus::Closed] {
            let json = serde_json::to_string(&status).unwrap();
            let back: TradeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn signal_deserialize_without_allocation_pct_falls_back_to_default() {
        // Backwards compatibility: a serialized Signal from before the
        // allocation_pct field was added must still deserialize, with
        // the field defaulting to 0.5 (the conservative half-allocation
        // fallback).
        let legacy_json = r#"{
            "strategy_name": "legacy",
            "pair": "USD_JPY",
            "direction": "long",
            "stop_loss_pct": "0.005",
            "confidence": 0.8,
            "timestamp": "2024-01-01T00:00:00Z"
        }"#;
        let signal: Signal = serde_json::from_str(legacy_json).expect("Signal must deserialize");
        assert_eq!(signal.allocation_pct, dec!(0.5));
        assert!(signal.max_hold_until.is_none());
        assert!(signal.take_profit_pct.is_none());
    }

    #[test]
    fn signal_with_take_profit_pct_roundtrip() {
        let signal = Signal {
            strategy_name: "s".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Short,
            stop_loss_pct: dec!(0.005),
            take_profit_pct: Some(dec!(0.015)),
            confidence: 0.75,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.3),
            max_hold_until: None,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.take_profit_pct, Some(dec!(0.015)));
        assert_eq!(back.stop_loss_pct, dec!(0.005));
    }

    #[test]
    fn trade_status_as_str() {
        assert_eq!(TradeStatus::Open.as_str(), "open");
        assert_eq!(TradeStatus::Closing.as_str(), "closing");
        assert_eq!(TradeStatus::Closed.as_str(), "closed");
    }
}
