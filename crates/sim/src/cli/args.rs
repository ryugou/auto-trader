use std::path::PathBuf;

use chrono::NaiveDate;

const MAX_RUNS_LIMIT: i64 = 1_000_000;

/// Command line of `auto-trader-sim`.
#[derive(Debug, clap::Parser)]
#[command(name = "auto-trader-sim")]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommand of `auto-trader-sim`.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Applies the database migrations, which create the simulation tables.
    Migrate,
    /// Downloads the M5 bars of the period from GMO and stores them.
    Backfill {
        /// First day of the period.
        #[arg(long)]
        from: NaiveDate,
        /// Last day of the period.
        #[arg(long)]
        to: NaiveDate,
        /// Prints the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Computes the benchmark metrics of the period.
    Benchmark {
        /// First day of the period, defaulting to the earliest day with enough warmup bars.
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Day after the last day of the period, defaulting to the day after the last stored bar.
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Prints the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Runs a script once over the period.
    Run {
        /// Path of the script file.
        #[arg(long)]
        script: PathBuf,
        /// Parameter values as a JSON object, with the script defaults for the parameters left out.
        #[arg(long)]
        params: Option<String>,
        /// First day of the period, defaulting to the earliest day with enough warmup bars.
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Day after the last day of the period, defaulting to the day after the last stored bar.
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Prints the result as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Runs a script over a grid of parameter combinations.
    Sweep {
        /// Path of the script file.
        #[arg(long)]
        script: PathBuf,
        /// First day of the period, defaulting to the earliest day with enough warmup bars.
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Day after the last day of the period, defaulting to the day after the last stored bar.
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Maximum number of runs, from 1 to 1000000.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_RUNS_LIMIT))]
        max_runs: u32,
        /// Seed of the random sample of combinations.
        #[arg(long, default_value_t = 42)]
        seed: u64,
        /// Number of worker threads, at least 1, defaulting to the configured value.
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        jobs: Option<u64>,
        /// Prints the result as JSON.
        #[arg(long)]
        json: bool,
    },
}
