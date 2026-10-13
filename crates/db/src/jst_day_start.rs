use chrono::{DateTime, Days, FixedOffset, NaiveDate, NaiveTime, Utc};

const JST_OFFSET: FixedOffset = match FixedOffset::east_opt(9 * 3600) {
    Some(offset) => offset,
    None => panic!("jst offset of 9 hours must be valid"),
};

pub(crate) fn jst_day_start_utc(date: NaiveDate) -> anyhow::Result<DateTime<Utc>> {
    let Some(start) = date
        .and_time(NaiveTime::MIN)
        .and_local_timezone(JST_OFFSET)
        .single()
    else {
        anyhow::bail!("invalid date: out of range");
    };
    Ok(start.with_timezone(&Utc))
}

pub(crate) fn jst_next_day_start_utc(date: NaiveDate) -> anyhow::Result<DateTime<Utc>> {
    let Some(next) = date.checked_add_days(Days::new(1)) else {
        anyhow::bail!("invalid date: out of range");
    };
    jst_day_start_utc(next)
}

#[cfg(test)]
mod tests;
