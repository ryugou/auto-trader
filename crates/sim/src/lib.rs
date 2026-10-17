#![doc = "Simulator that runs Rhai trading scripts over historical USD/JPY M5 bars, evaluates them against benchmarks and stores the results."]

/// Benchmarks of the profit available from the price swings, which the runs are compared with.
pub mod benchmark;
/// Command line of the simulator.
pub mod cli;
/// Configuration of the simulator.
pub mod config;
/// Storage and loading of historical bars.
pub mod data;
/// Simulation of the trades that a script decides.
pub mod engine;
/// Error type of the simulator.
pub mod error;
/// Metrics of a simulation run.
pub mod eval;
/// Download of historical bars from GMO.
pub mod fetch;
/// Indicators that scripts can request.
pub mod indicators;
/// Compilation and execution of Rhai scripts.
pub mod script;
/// Bars of the simulation period aggregated to several timeframes.
pub mod series;
/// Storage of scripts, batches and runs.
pub mod store;
/// Sweep of a script over a grid of parameter combinations.
pub mod sweep;
/// Bars and unit conversions.
pub mod types;
