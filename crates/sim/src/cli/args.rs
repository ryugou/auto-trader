use std::path::PathBuf;

use chrono::NaiveDate;

const MAX_RUNS_LIMIT: i64 = 1_000_000;

#[derive(Debug, clap::Parser)]
#[command(name = "auto-trader-sim")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    Migrate,
    Backfill {
        #[arg(long)]
        from: NaiveDate,
        #[arg(long)]
        to: NaiveDate,
        #[arg(long)]
        json: bool,
    },
    Benchmark {
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        #[arg(long)]
        json: bool,
    },
    Run {
        #[arg(long)]
        script: PathBuf,
        #[arg(long)]
        params: Option<String>,
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        #[arg(long)]
        json: bool,
    },
    Sweep {
        #[arg(long)]
        script: PathBuf,
        #[arg(long)]
        from: Option<NaiveDate>,
        #[arg(long)]
        to: Option<NaiveDate>,
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=MAX_RUNS_LIMIT))]
        max_runs: u32,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        jobs: Option<u64>,
        #[arg(long)]
        json: bool,
    },
}
