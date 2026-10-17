#![doc = "Database access of the trading crates: the connection pool, the migrations and the queries on trades, accounts, candles, notifications and summaries."]

/// Candles of the price feeds.
pub mod candles;
/// Statistics and balance histories for the dashboard.
pub mod dashboard;
mod jst_day_start;
/// Recording of macro events.
pub mod macro_events;
/// Notifications about opened and closed trades.
pub mod notifications;
/// Creation of the database connection pool.
pub mod pool;
/// Registered trading strategies.
pub mod strategies;
/// Daily summary of closed trades.
pub mod summary;
/// Reading, writing and closing of trades, and the account entries they cause.
pub mod trades;
/// Creation, lookup, update and balance recalculation of trading accounts.
pub mod trading_accounts;
