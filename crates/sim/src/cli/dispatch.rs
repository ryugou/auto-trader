use std::path::Path;

use clap::Parser;

use super::args::{Cli, Command};
use super::backfill::run_backfill;
use super::benchmark::run_benchmark;
use super::migrate::run_migrate;
use super::run_cmd::run_run;
use super::sweep_cmd::run_sweep_cmd;
use crate::config;

pub async fn run_args<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(cli) => run(cli).await,
        Err(e) => {
            if let Err(print_err) = e.print() {
                tracing::error!(
                    error = %print_err,
                    "failed to print clap help or usage error output"
                );
            }
            if e.exit_code() == 0 { 0 } else { 1 }
        }
    }
}

pub async fn run(cli: Cli) -> i32 {
    run_with_config_path(cli, &config::config_path()).await
}

pub async fn run_with_config_path(cli: Cli, config_path: &Path) -> i32 {
    run_with_config_path_and_output(cli, config_path, &mut std::io::stdout()).await
}

pub async fn run_with_config_path_and_output(
    cli: Cli,
    config_path: &Path,
    out: &mut (dyn std::io::Write + Send),
) -> i32 {
    match cli.command {
        Command::Migrate => run_migrate(config_path).await,
        Command::Backfill { from, to, json } => {
            run_backfill(config_path, from, to, json, out).await
        }
        Command::Benchmark { from, to, json } => {
            run_benchmark(config_path, from, to, json, out).await
        }
        Command::Run {
            script,
            params,
            from,
            to,
            json,
        } => run_run(config_path, script, params, from, to, json, out).await,
        Command::Sweep {
            script,
            from,
            to,
            max_runs,
            seed,
            jobs,
            json,
        } => {
            run_sweep_cmd(
                config_path,
                script,
                from,
                to,
                max_runs,
                seed,
                jobs,
                json,
                out,
            )
            .await
        }
    }
}
