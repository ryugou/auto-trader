/// In-process API server for tests.
pub mod app;
/// Seeding and dumping of database rows for tests.
pub mod db;
/// Capture of application logs and the report of a failed test.
pub mod failure_output;
/// Loading of candles from CSV fixtures into the database.
pub mod fixture_loader;
/// Harness that wires an account, a price store and a trader for full-pipeline tests.
pub mod pipeline;
/// Seeding of trades, notifications and daily summaries.
pub mod seed;
/// Expected values of the position sizing, which tests compare with the results of the production code.
pub mod sizing_invariants;
/// Loading of price events from CSV fixtures.
pub mod trade_flow;
