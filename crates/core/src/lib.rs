#![doc = "Domain types, configuration, events and trait definitions shared by the trading crates."]

/// Estimation of the commission charged on a fill.
pub mod commission;
/// Application configuration loaded from a TOML file, and its validation.
pub mod config;
/// Event types that carry prices, signals and trades.
pub mod event;
/// Trait for components that execute trading signals and manage open positions.
pub mod executor;
/// Trait and data types for recording trades and market events in a knowledge store and searching it.
pub mod knowledge;
/// Unrealized profit and loss, required margin and maintenance ratio of open positions.
pub mod margin;
/// Estimation of the SFD charge and its hourly computation from the divergence between FX and spot prices.
pub mod sfd;
/// Strategy trait and the types that strategies use to receive macro updates and report exits.
pub mod strategy;
/// Daily swap computation and swap rate staleness check.
pub mod swap;
/// Domain types: pairs, exchanges, directions, signals, trades and candles.
pub mod types;
/// Trait for the Vegapunk API and the types of its search results.
pub mod vegapunk_port;
