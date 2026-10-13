use std::path::Path;

use chrono::NaiveDate;

use super::output::{output_failed, print_benchmark};
use super::prepare::{build_dataset, compute_benchmarks, prepare};

pub(super) async fn run_benchmark(
    config_path: &Path,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    let prepared = match prepare(config_path).await {
        Ok(p) => p,
        Err(code) => return code,
    };
    let (dataset, _, _) = match build_dataset(&prepared, from, to).await {
        Ok(d) => d,
        Err(code) => return code,
    };
    let benchmarks = compute_benchmarks(&dataset, &prepared.sim);
    match print_benchmark(out, &benchmarks, json) {
        Ok(()) => 0,
        Err(e) => output_failed(e),
    }
}
