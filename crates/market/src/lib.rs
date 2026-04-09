use auto_trader_core::types::Pair;
use rust_decimal::Decimal;

pub mod bitflyer;
pub mod candle_builder;
pub mod indicators;
pub mod monitor;
pub mod oanda;
pub mod provider;

/// One raw tick observed on any exchange. Used by the dashboard
/// feed-health / PriceStore path (distinct from the candle-aggregated
/// `PriceEvent` channel that strategies consume). Defined here so
/// bitflyer and oanda can both reference it without a circular
/// dependency between the two modules.
pub type RawTick = (Pair, Decimal, chrono::DateTime<chrono::Utc>);
