#![doc = "Price feeds, candle building, indicators and exchange API clients of the exchanges."]

/// Feed of the bitFlyer WebSocket ticks.
pub mod bitflyer;
/// Client of the bitFlyer private API.
pub mod bitflyer_private;
/// Candle building from ticks.
pub mod candle_builder;
/// Trait of the private API of an exchange.
pub mod exchange_api;
/// Feed of the GMO FX ticker.
pub mod gmo_fx;
/// Client of the GMO FX private API.
pub mod gmo_fx_private;
/// Technical indicators of price series.
pub mod indicators;
/// Trait of price feeds.
pub mod market_feed;
/// Feed that polls OANDA candles.
pub mod monitor;
/// Exchange API that supports no operation.
pub mod null_exchange_api;
/// Client of the OANDA candle and price API.
pub mod oanda;
/// Client of the OANDA private API.
pub mod oanda_private;
/// Store of the latest tick of each feed.
pub mod price_store;
/// Trait of market data providers.
pub mod provider;
