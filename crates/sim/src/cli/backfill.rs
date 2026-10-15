use std::path::Path;

use chrono::NaiveDate;

use super::output::{output_failed, print_backfill};
use super::prepare::prepare;
use crate::error::SimError;
use crate::fetch::{self, GmoKlineClient};

fn validate_backfill_range(from: NaiveDate, to: NaiveDate) -> Result<(), i32> {
    if from > to {
        tracing::error!(from = %from, to = %to, "invalid --from: must not be after --to");
        return Err(1);
    }
    let min_date = fetch::gmo_kline_min_date();
    if from < min_date {
        tracing::error!(
            from = %from,
            min_date = %min_date,
            "invalid --from: earlier than the oldest date the gmo kline api accepts; pass --from min_date or later"
        );
        return Err(1);
    }
    Ok(())
}

async fn run_backfill_loop(
    config_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<fetch::BackfillReport, i32> {
    let prepared = prepare(config_path).await?;
    let client = GmoKlineClient::new(&prepared.sim.gmo_public_base_url).map_err(|e| {
        tracing::error!(error = %e, "failed to build gmo kline http client");
        1
    })?;

    let mut total = fetch::BackfillReport {
        days: 0,
        saved: 0,
        one_sided: 0,
        invalid: 0,
        failed: Vec::new(),
    };
    let mut date = from;
    loop {
        match fetch::backfill(&prepared.pool, &client, date, date).await {
            Ok(r) => {
                total.days += r.days;
                total.saved += r.saved;
                total.one_sided += r.one_sided;
                total.invalid += r.invalid;
                total.failed.extend(r.failed);
            }
            Err(SimError::FetchIncomplete(f)) => {
                total.days += 1;
                total.failed.extend(f);
            }
            Err(e) => {
                tracing::error!(
                    date = %date,
                    from = %from,
                    to = %to,
                    days = total.days,
                    saved = total.saved,
                    one_sided = total.one_sided,
                    invalid = total.invalid,
                    failed = ?total.failed,
                    error = %e,
                    "backfill aborted by a non-fetch error; counts above cover only the dates \
                     processed before this one"
                );
                return Err(1);
            }
        }
        if date == to {
            break;
        }
        match date.succ_opt() {
            Some(next) => date = next,
            None => break,
        }
    }
    Ok(total)
}

fn write_backfill_result(
    out: &mut dyn std::io::Write,
    json: bool,
    total: &fetch::BackfillReport,
    from: NaiveDate,
    to: NaiveDate,
) -> i32 {
    if let Err(e) = print_backfill(out, json, total) {
        return output_failed(e);
    }
    if !total.failed.is_empty() {
        tracing::error!(
            from = %from,
            to = %to,
            failed = ?total.failed,
            "failed to fetch some dates after retries: rerun the same range to \
             fill the gaps (already-saved dates are unaffected)"
        );
        return 1;
    }
    0
}

pub(super) async fn run_backfill(
    config_path: &Path,
    from: NaiveDate,
    to: NaiveDate,
    json: bool,
    out: &mut dyn std::io::Write,
) -> i32 {
    if let Err(code) = validate_backfill_range(from, to) {
        return code;
    }
    let total = match run_backfill_loop(config_path, from, to).await {
        Ok(t) => t,
        Err(code) => return code,
    };
    write_backfill_result(out, json, &total, from, to)
}
