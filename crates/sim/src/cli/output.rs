use uuid::Uuid;

use crate::benchmark::Benchmark;
use crate::engine::RunStatus;
use crate::error::SimError;
use crate::fetch;
use crate::store;
use crate::sweep::RunRecord;
use crate::types::milli_to_pips;

pub(super) fn output_failed(e: std::io::Error) -> i32 {
    tracing::error!(
        error = %e,
        "failed to write result to output stream: \
         any database writes made before this point are already committed"
    );
    1
}

fn flush_because_drop_does_not_report_write_errors(
    out: &mut dyn std::io::Write,
) -> std::io::Result<()> {
    out.flush()
}

fn format_utc_rfc3339(epoch_secs: i64) -> String {
    crate::data::format_epoch_secs_rfc3339(epoch_secs).unwrap_or_else(|| {
            tracing::warn!(
                epoch_secs,
                "failed to convert epoch seconds to rfc3339: falling back to the raw epoch-seconds representation"
            );
            epoch_secs.to_string()
        })
}

fn backfill_json(report: &fetch::BackfillReport) -> serde_json::Value {
    serde_json::json!({
        "days": report.days,
        "saved": report.saved,
        "one_sided": report.one_sided,
        "invalid": report.invalid,
        "failed": report.failed,
    })
}

pub(super) fn print_backfill(
    out: &mut dyn std::io::Write,
    json: bool,
    report: &fetch::BackfillReport,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", backfill_json(report))?;
    } else {
        print_kv_table(
            out,
            &[
                ("days".to_string(), report.days.to_string()),
                ("saved".to_string(), report.saved.to_string()),
                ("one_sided".to_string(), report.one_sided.to_string()),
                ("invalid".to_string(), report.invalid.to_string()),
                ("failed".to_string(), report.failed.join(", ")),
            ],
        )?;
    }
    flush_because_drop_does_not_report_write_errors(out)
}

fn benchmark_json(benchmarks: &[Benchmark]) -> serde_json::Value {
    let by_theta: std::collections::BTreeMap<String, serde_json::Value> = benchmarks
        .iter()
        .map(|b| {
            (
                b.theta_pips.to_string(),
                serde_json::json!({
                    "leg_count": b.legs.len(),
                    "ideal_pips": milli_to_pips(b.ideal_milli),
                    "realizable_pips": milli_to_pips(b.realizable_milli),
                }),
            )
        })
        .collect();
    serde_json::json!({ "by_theta": by_theta })
}

pub(super) fn print_benchmark(
    out: &mut dyn std::io::Write,
    benchmarks: &[Benchmark],
    json: bool,
) -> std::io::Result<()> {
    if json {
        writeln!(out, "{}", benchmark_json(benchmarks))?;
    } else {
        let rows: Vec<Vec<String>> = benchmarks
            .iter()
            .map(|b| {
                vec![
                    b.theta_pips.to_string(),
                    b.legs.len().to_string(),
                    format!("{:.2}", milli_to_pips(b.ideal_milli)),
                    format!("{:.2}", milli_to_pips(b.realizable_milli)),
                ]
            })
            .collect();
        print_columns(
            out,
            &["theta_pips", "leg_count", "ideal_pips", "realizable_pips"],
            &rows,
        )?;
    }
    flush_because_drop_does_not_report_write_errors(out)
}

fn run_json(
    record: &RunRecord,
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
) -> Result<serde_json::Value, SimError> {
    let (status, error) = run_status_and_error_message(record);

    let mut obj = serde_json::json!({
        "status": status,
        "elapsed_secs": elapsed,
        "script_id": script_id,
        "batch_id": batch_id,
    });
    if let Some(err) = &error {
        obj["error"] = serde_json::Value::String(err.clone());
    }
    if let Some(metrics) = &record.metrics {
        obj["metrics"] = serde_json::to_value(metrics).map_err(|e| {
            SimError::Other(anyhow::anyhow!("failed to serialize run metrics: {e}"))
        })?;
    }
    Ok(obj)
}

fn run_status_and_error_message(record: &RunRecord) -> (&'static str, Option<String>) {
    match &record.status {
        RunStatus::Ok => ("ok", None),
        RunStatus::ScriptError { open_time, message } => (
            "script_error",
            Some(format!("{} {message}", format_utc_rfc3339(*open_time))),
        ),
    }
}

pub(super) fn print_run(
    out: &mut dyn std::io::Write,
    record: &RunRecord,
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    json: bool,
) -> std::io::Result<()> {
    if json {
        let value =
            run_json(record, elapsed, script_id, batch_id).map_err(std::io::Error::other)?;
        writeln!(out, "{value}")?;
        return flush_because_drop_does_not_report_write_errors(out);
    }
    let (status, error) = run_status_and_error_message(record);

    let mut rows = vec![
        ("status".to_string(), status.to_string()),
        ("elapsed_secs".to_string(), format!("{elapsed:.2}")),
        ("script_id".to_string(), script_id.to_string()),
        ("batch_id".to_string(), batch_id.to_string()),
    ];
    if let Some(err) = &error {
        rows.push(("error".to_string(), err.clone()));
    }
    if let Some(metrics) = &record.metrics {
        rows.push((
            "total_pips".to_string(),
            format!("{:.2}", metrics.total_pips),
        ));
        rows.push(("trade_count".to_string(), metrics.trade_count.to_string()));
        rows.push(("win_rate".to_string(), format!("{:.4}", metrics.win_rate)));
        rows.push((
            "max_drawdown_pips".to_string(),
            format!("{:.2}", metrics.max_drawdown_pips),
        ));
        rows.push((
            "time_in_market".to_string(),
            format!("{:.4}", metrics.time_in_market),
        ));
        rows.push((
            "protective_stop_count".to_string(),
            metrics.protective_stop_count.to_string(),
        ));
    }
    print_kv_table(out, &rows)?;
    flush_because_drop_does_not_report_write_errors(out)
}

fn sweep_json(
    top: &[store::RunSummary],
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    total_runs: usize,
) -> serde_json::Value {
    let top_json: Vec<serde_json::Value> = top
        .iter()
        .map(|r| {
            serde_json::json!({
                "params": r.params,
                "total_pips": r.total_pips,
                "trade_count": r.trade_count,
                "metrics": r.metrics,
            })
        })
        .collect();
    serde_json::json!({
        "elapsed_secs": elapsed,
        "script_id": script_id,
        "batch_id": batch_id,
        "total_runs": total_runs,
        "top_runs": top_json,
    })
}

pub(super) fn print_sweep(
    out: &mut dyn std::io::Write,
    top: &[store::RunSummary],
    elapsed: f64,
    script_id: Uuid,
    batch_id: Uuid,
    total_runs: usize,
    json: bool,
) -> std::io::Result<()> {
    if json {
        writeln!(
            out,
            "{}",
            sweep_json(top, elapsed, script_id, batch_id, total_runs)
        )?;
        return flush_because_drop_does_not_report_write_errors(out);
    }
    print_kv_table(
        out,
        &[
            ("elapsed_secs".to_string(), format!("{elapsed:.2}")),
            ("script_id".to_string(), script_id.to_string()),
            ("batch_id".to_string(), batch_id.to_string()),
            ("total_runs".to_string(), total_runs.to_string()),
        ],
    )?;
    let rows: Vec<Vec<String>> = top
        .iter()
        .enumerate()
        .map(|(i, r)| {
            vec![
                (i + 1).to_string(),
                format!("{:.2}", r.total_pips),
                r.trade_count.to_string(),
                r.params.to_string(),
            ]
        })
        .collect();
    print_columns(out, &["rank", "total_pips", "trade_count", "params"], &rows)?;
    flush_because_drop_does_not_report_write_errors(out)
}

fn print_kv_table(out: &mut dyn std::io::Write, rows: &[(String, String)]) -> std::io::Result<()> {
    let width = rows
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0);
    for (k, v) in rows {
        writeln!(out, "{k:<width$}  {v}")?;
    }
    Ok(())
}

fn print_columns(
    out: &mut dyn std::io::Write,
    headers: &[&str],
    rows: &[Vec<String>],
) -> std::io::Result<()> {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: &[String]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
    };
    let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    writeln!(out, "{}", line(&header_cells))?;
    for row in rows {
        writeln!(out, "{}", line(row))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
