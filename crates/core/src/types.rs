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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// 注文種別。戦略が Signal 生成時に選択する。
///
/// - `Market`: 成行注文。取引所がその瞬間の気配値で約定させる。
///   スリッページが発生しうるが、約定確実性が高い。
/// - `Limit { price }`: 指値注文。指定価格以下 (Long) / 以上 (Short)
///   でのみ約定する。未約定リスクあり。
///
/// JSON 形式は internally-tagged (`{"type": "market"}` /
/// `{"type": "limit", "price": "100.5"}`) — これは strategy ログや
/// /api/signals への出力でも人間可読性を保つため。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrderType {
    #[default]
    Market,
    Limit {
        price: Decimal,
    },
}

impl OrderType {
    /// 指値注文を構築する。price が 0 以下の場合は Err を返し、
    /// 戦略側の計算バグ / 取引所側の異常レスポンスを型境界で弾く。
    /// `unreachable!()` / `todo!()` で済ませない理由は PR 1 Batch A
    /// レビューの FOLLOWUP 参照。
    pub fn limit(price: Decimal) -> Result<Self, InvalidOrderTypeError> {
        if price <= Decimal::ZERO {
            return Err(InvalidOrderTypeError::NonPositiveLimitPrice(price));
        }
        Ok(OrderType::Limit { price })
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InvalidOrderTypeError {
    #[error("limit order price must be > 0, got {0}")]
    NonPositiveLimitPrice(Decimal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeMode {
    Live,
    Paper,
    Backtest,
}

impl TradeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            TradeMode::Live => "live",
            TradeMode::Paper => "paper",
            TradeMode::Backtest => "backtest",
        }
    }
}

impl std::fmt::Display for TradeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeStatus {
    Open,
    Closed,
    /// 注文を取引所に送信済みで約定確認待ち (live のみ)。
    Pending,
    /// DB と取引所で状態が食い違い、手動対処が必要。
    Inconsistent,
}

impl TradeStatus {
    /// Paper / backtest は Open / Closed のみ許容。Live 専用の Pending /
    /// Inconsistent を paper/backtest で誤って書き込んだら debug_assert で
    /// 即死させる。
    ///
    /// 本番 (`--release`) では debug_assert は no-op になるため、PR 2 以降で
    /// 状態遷移関数 (`fn transition(from, to) -> Result<TradeStatus>`) を
    /// 導入して遷移不可能状態を締める予定。このガードはそれまでの暫定措置。
    pub fn assert_valid_for_mode(self, mode: TradeMode) {
        if matches!(mode, TradeMode::Paper | TradeMode::Backtest) {
            debug_assert!(
                matches!(self, TradeStatus::Open | TradeStatus::Closed),
                "paper/backtest trade must not have status {self:?}"
            );
        }
    }

    /// DB bind 時 / SQL 比較で使う静的文字列。`serde_json::to_string` の
    /// round-trip (`"open"` → `open`) を踏まず 1 アロケーションで済む。
    pub fn as_str(&self) -> &'static str {
        match self {
            TradeStatus::Open => "open",
            TradeStatus::Closed => "closed",
            TradeStatus::Pending => "pending",
            TradeStatus::Inconsistent => "inconsistent",
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub strategy_name: String,
    pub pair: Pair,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
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
    /// 注文種別 (Market / Limit)。Signal を出した戦略が選ぶ。
    /// 既存の JSON を読み込むと Market に default される (後方互換)。
    #[serde(default)]
    pub order_type: OrderType,
}

fn default_allocation_pct() -> Decimal {
    // Conservative default: half the account's capacity. Strategies
    // SHOULD set their own value rather than rely on this.
    rust_decimal::Decimal::new(5, 1) // 0.5
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: Pair,
    pub exchange: Exchange,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub paper_account_id: Option<Uuid>,
    pub entry_at: DateTime<Utc>,
    pub exit_at: Option<DateTime<Utc>>,
    pub pnl_pips: Option<Decimal>,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<ExitReason>,
    pub mode: TradeMode,
    pub status: TradeStatus,
    /// Optional time-based fail-safe — see `Signal::max_hold_until`.
    #[serde(default)]
    pub max_hold_until: Option<DateTime<Utc>>,
    /// bitFlyer 注文受付 ID (sendchildorder のレスポンス)。
    /// Paper トレードでは None。
    #[serde(default)]
    pub child_order_acceptance_id: Option<String>,
    /// bitFlyer 注文 ID (約定確定後に getchildorders から取得)。
    /// Paper トレードでは None。pending 中も None。
    #[serde(default)]
    pub child_order_id: Option<String>,
}

/// テスト専用の Default 実装。
/// 本番コードからは呼ばれないよう `#[cfg(test)]` でガード済み。
/// (PR 2 以降で Trade にフィールドを足しても、戦略 / backtest /
/// paper のテスト固有 Trade リテラルを全書き換えしないで済むよう、
/// ベースライン Trade を用意する。)
///
/// `feature = "testing"` は廃止。production build に `Trade::default()`
/// (entry_price=0 など無効値) が露出するのを防ぐため `#[cfg(test)]` のみ。
#[cfg(test)]
impl Default for Trade {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            strategy_name: String::from("test_strategy"),
            pair: Pair::new("FX_BTC_JPY"),
            exchange: Exchange::BitflyerCfd,
            direction: Direction::Long,
            entry_price: rust_decimal::Decimal::ZERO,
            exit_price: None,
            stop_loss: rust_decimal::Decimal::ZERO,
            take_profit: rust_decimal::Decimal::ZERO,
            quantity: None,
            leverage: rust_decimal::Decimal::ONE,
            fees: rust_decimal::Decimal::ZERO,
            paper_account_id: None,
            entry_at: chrono::DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
            max_hold_until: None,
            child_order_acceptance_id: None,
            child_order_id: None,
        }
    }
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
            entry_price: dec!(150.00),
            stop_loss: dec!(149.50),
            take_profit: dec!(151.00),
            confidence: 0.8,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.5),
            max_hold_until: None,
            order_type: OrderType::Market,
        };
        let json = serde_json::to_string(&signal).unwrap();
        let back: Signal = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pair, signal.pair);
        assert_eq!(back.direction, Direction::Long);
    }

    #[test]
    fn order_type_serializes_market() {
        let json = serde_json::to_string(&OrderType::Market).unwrap();
        assert_eq!(json, r#"{"type":"market"}"#);
    }

    #[test]
    fn order_type_serializes_limit_with_price() {
        let ot = OrderType::Limit { price: dec!(100.5) };
        let json = serde_json::to_string(&ot).unwrap();
        assert_eq!(json, r#"{"type":"limit","price":"100.5"}"#);
    }

    #[test]
    fn order_type_roundtrip_market() {
        let ot = OrderType::Market;
        let json = serde_json::to_string(&ot).unwrap();
        let back: OrderType = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, OrderType::Market));
    }

    #[test]
    fn order_type_roundtrip_limit() {
        let ot = OrderType::Limit {
            price: dec!(150.25),
        };
        let json = serde_json::to_string(&ot).unwrap();
        let back: OrderType = serde_json::from_str(&json).unwrap();
        match back {
            OrderType::Limit { price } => assert_eq!(price, dec!(150.25)),
            _ => panic!("expected Limit"),
        }
    }

    #[test]
    fn signal_defaults_order_type_to_market_when_absent() {
        // 既存コードが生成した Signal JSON (order_type フィールドなし)
        // は OrderType::Market に既定化されることを検証する。
        let legacy_json = r#"{
            "strategy_name": "legacy",
            "pair": "USD_JPY",
            "direction": "long",
            "entry_price": "150.00",
            "stop_loss": "149.50",
            "take_profit": "151.00",
            "confidence": 0.8,
            "timestamp": "2024-01-01T00:00:00Z",
            "allocation_pct": "0.5"
        }"#;
        let signal: Signal = serde_json::from_str(legacy_json).unwrap();
        assert!(matches!(signal.order_type, OrderType::Market));
    }

    #[test]
    fn signal_serializes_with_explicit_order_type() {
        let signal = Signal {
            strategy_name: "s".to_string(),
            pair: Pair::new("USD_JPY"),
            direction: Direction::Long,
            entry_price: dec!(150.0),
            stop_loss: dec!(149.0),
            take_profit: dec!(151.0),
            confidence: 0.8,
            timestamp: Utc::now(),
            allocation_pct: dec!(0.5),
            max_hold_until: None,
            order_type: OrderType::Limit { price: dec!(150.5) },
        };
        let json = serde_json::to_string(&signal).unwrap();
        assert!(json.contains(r#""order_type":{"type":"limit","price":"150.5"}"#));
    }

    #[test]
    fn trade_status_serializes_pending() {
        let json = serde_json::to_string(&TradeStatus::Pending).unwrap();
        assert_eq!(json, r#""pending""#);
    }

    #[test]
    fn trade_status_serializes_inconsistent() {
        let json = serde_json::to_string(&TradeStatus::Inconsistent).unwrap();
        assert_eq!(json, r#""inconsistent""#);
    }

    #[test]
    fn trade_status_roundtrip_all_variants() {
        for status in [
            TradeStatus::Open,
            TradeStatus::Closed,
            TradeStatus::Pending,
            TradeStatus::Inconsistent,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let back: TradeStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
    }

    #[test]
    fn assert_valid_for_mode_accepts_paper_open() {
        TradeStatus::Open.assert_valid_for_mode(TradeMode::Paper);
        TradeStatus::Closed.assert_valid_for_mode(TradeMode::Paper);
    }

    #[test]
    fn assert_valid_for_mode_accepts_backtest_open() {
        TradeStatus::Open.assert_valid_for_mode(TradeMode::Backtest);
        TradeStatus::Closed.assert_valid_for_mode(TradeMode::Backtest);
    }

    #[test]
    fn assert_valid_for_mode_accepts_live_all_statuses() {
        // Live は 4 バリアント全て許容
        TradeStatus::Pending.assert_valid_for_mode(TradeMode::Live);
        TradeStatus::Open.assert_valid_for_mode(TradeMode::Live);
        TradeStatus::Closed.assert_valid_for_mode(TradeMode::Live);
        TradeStatus::Inconsistent.assert_valid_for_mode(TradeMode::Live);
    }

    #[test]
    #[should_panic(expected = "paper/backtest trade must not have status")]
    fn assert_valid_for_mode_panics_on_paper_pending() {
        TradeStatus::Pending.assert_valid_for_mode(TradeMode::Paper);
    }

    #[test]
    #[should_panic(expected = "paper/backtest trade must not have status")]
    fn assert_valid_for_mode_panics_on_paper_inconsistent() {
        TradeStatus::Inconsistent.assert_valid_for_mode(TradeMode::Paper);
    }

    #[test]
    #[should_panic(expected = "paper/backtest trade must not have status")]
    fn assert_valid_for_mode_panics_on_backtest_pending() {
        TradeStatus::Pending.assert_valid_for_mode(TradeMode::Backtest);
    }

    #[test]
    fn trade_default_produces_paper_open_with_none_order_ids() {
        let t = Trade::default();
        assert_eq!(t.mode, TradeMode::Paper);
        assert_eq!(t.status, TradeStatus::Open);
        assert!(t.child_order_acceptance_id.is_none());
        assert!(t.child_order_id.is_none());
        assert_eq!(t.direction, Direction::Long);
        assert_eq!(t.exchange, Exchange::BitflyerCfd);
    }

    #[test]
    fn order_type_limit_new_accepts_positive_price() {
        let ot = OrderType::limit(dec!(100.5)).unwrap();
        assert!(matches!(ot, OrderType::Limit { price } if price == dec!(100.5)));
    }

    #[test]
    fn order_type_limit_new_rejects_zero() {
        assert!(OrderType::limit(Decimal::ZERO).is_err());
    }

    #[test]
    fn order_type_limit_new_rejects_negative() {
        assert!(OrderType::limit(dec!(-1)).is_err());
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
            "entry_price": "150.00",
            "stop_loss": "149.50",
            "take_profit": "151.00",
            "confidence": 0.8,
            "timestamp": "2024-01-01T00:00:00Z"
        }"#;
        let signal: Signal =
            serde_json::from_str(legacy_json).expect("legacy Signal must deserialize");
        assert_eq!(signal.allocation_pct, dec!(0.5));
        assert!(signal.max_hold_until.is_none());
    }
}
