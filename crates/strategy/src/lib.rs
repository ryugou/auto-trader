#![doc = "Trading strategies and the engine that feeds price events to them."]

/// Mean reversion strategy on Bollinger Bands and RSI.
pub mod bb_mean_revert;
pub(crate) mod candle_history;
pub(crate) mod donchian_baseline;
/// Donchian channel breakout trend strategy.
pub mod donchian_trend;
/// Donchian channel breakout trend strategy with parameters supplied at construction.
pub mod donchian_trend_evolve;
/// Engine that dispatches price events to the registered strategies and forwards their signals.
pub mod engine;
/// Breakout strategy on the GMO FX M15 channel filtered by the H1 trend.
pub mod fx_new;
/// Breakout strategy that enters after a Bollinger Bands squeeze inside Keltner Channels.
pub mod squeeze_momentum;
pub(crate) mod stop_loss;
/// Swing strategy that asks an LLM for the trade decision.
pub mod swing_llm;
#[cfg(test)]
pub(crate) mod test_support;
