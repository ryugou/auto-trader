use std::sync::Arc;

use crate::error::SimError;
use crate::indicators::{self, IndicatorCache, IndicatorKey, IndicatorSeries};
use crate::types::{Bar, M5_SECS, mid2_to_yen};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tf {
    M5,
    M15,
    H1,
    H4,
}

const TF_COUNT: usize = 4;
const ALL_TFS: [Tf; TF_COUNT] = [Tf::M5, Tf::M15, Tf::H1, Tf::H4];

impl Tf {
    pub fn secs(self) -> i64 {
        match self {
            Tf::M5 => M5_SECS,
            Tf::M15 => 900,
            Tf::H1 => 3600,
            Tf::H4 => 14400,
        }
    }

    pub fn parse(s: &str) -> Option<Tf> {
        match s {
            "M5" => Some(Tf::M5),
            "M15" => Some(Tf::M15),
            "H1" => Some(Tf::H1),
            "H4" => Some(Tf::H4),
            _ => None,
        }
    }

    fn idx(self) -> usize {
        match self {
            Tf::M5 => 0,
            Tf::M15 => 1,
            Tf::H1 => 2,
            Tf::H4 => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TfSeries {
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub end_time: Vec<i64>,
}

fn aggregate(bars: &[Bar], tf: Tf) -> TfSeries {
    let secs = tf.secs();
    let n = bars.len();
    let mut open = Vec::new();
    let mut high = Vec::new();
    let mut low = Vec::new();
    let mut close = Vec::new();
    let mut end_time = Vec::new();

    let mut idx = 0;
    while idx < n {
        let bucket = bars[idx].open_time.div_euclid(secs);
        let open_mid2 = bars[idx].mid2_open();
        let mut high_mid2 = bars[idx].mid2_high();
        let mut low_mid2 = bars[idx].mid2_low();
        let mut close_mid2 = bars[idx].mid2_close();

        let mut j = idx + 1;
        while j < n && bars[j].open_time.div_euclid(secs) == bucket {
            high_mid2 = high_mid2.max(bars[j].mid2_high());
            low_mid2 = low_mid2.min(bars[j].mid2_low());
            close_mid2 = bars[j].mid2_close();
            j += 1;
        }

        open.push(mid2_to_yen(open_mid2));
        high.push(mid2_to_yen(high_mid2));
        low.push(mid2_to_yen(low_mid2));
        close.push(mid2_to_yen(close_mid2));
        end_time.push((bucket + 1) * secs);

        idx = j;
    }

    TfSeries {
        open,
        high,
        low,
        close,
        end_time,
    }
}

fn bucket_end_time_has_arrived_independent_of_any_later_bar(
    end_time: i64,
    bar_open_time: i64,
) -> bool {
    end_time <= bar_open_time + M5_SECS
}

fn completed_counts(bars: &[Bar], end_time: &[i64]) -> Vec<usize> {
    let mut result = Vec::with_capacity(bars.len());
    let mut ptr = 0usize;
    for bar in bars {
        while ptr < end_time.len()
            && bucket_end_time_has_arrived_independent_of_any_later_bar(
                end_time[ptr],
                bar.open_time,
            )
        {
            ptr += 1;
        }
        result.push(ptr);
    }
    result
}

const SECS_PER_DAY: i64 = 86_400;

fn truncate_to_midnight_of_the_same_day(open_time: i64) -> i64 {
    open_time.div_euclid(SECS_PER_DAY) * SECS_PER_DAY
}

fn advance_to_midnight_of_the_next_day_so_bars_through_index_warmup_bars_minus_1_precede_it(
    open_time: i64,
) -> i64 {
    (open_time.div_euclid(SECS_PER_DAY) + 1) * SECS_PER_DAY
}

pub fn earliest_from(all_bars: &[Bar], warmup_bars: usize) -> Option<i64> {
    if warmup_bars == 0 {
        let first = all_bars.first()?;
        return Some(truncate_to_midnight_of_the_same_day(first.open_time));
    }
    let last_warmup = all_bars.get(warmup_bars - 1)?.open_time;
    Some(
        advance_to_midnight_of_the_next_day_so_bars_through_index_warmup_bars_minus_1_precede_it(
            last_warmup,
        ),
    )
}

#[derive(Debug)]
pub struct Dataset {
    bars: Vec<Bar>,
    eval_start: usize,
    from: i64,
    to: i64,
    series: [TfSeries; TF_COUNT],
    completed: [Vec<usize>; TF_COUNT],
    cache: IndicatorCache,
}

impl Dataset {
    pub fn new(
        all_bars: Vec<Bar>,
        from: i64,
        to: i64,
        warmup_bars: usize,
        cache_mb: usize,
    ) -> Result<Dataset, SimError> {
        let eval_start_in_all = all_bars.partition_point(|b| b.open_time < from);
        let eval_end_in_all = all_bars.partition_point(|b| b.open_time < to);

        if eval_end_in_all <= eval_start_in_all {
            return Err(SimError::Args(format!(
                "invalid evaluation period: no bars in [{from}, {to}) (UTC epoch seconds)"
            )));
        }

        if eval_start_in_all < warmup_bars {
            let detail = match earliest_from(&all_bars, warmup_bars) {
                Some(ts) => {
                    let date = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                        .map(|dt| dt.date_naive().to_string())
                        .unwrap_or_else(|| ts.to_string());
                    format!("earliest possible `from` is {date} (UTC)")
                }
                None => format!(
                    "not enough historical bars in total to satisfy warmup_bars={warmup_bars}: only {} bars loaded",
                    all_bars.len()
                ),
            };
            return Err(SimError::Args(format!(
                "invalid from: need {warmup_bars} warmup bars before it, only {eval_start_in_all} available; {detail}"
            )));
        }

        let window_start = eval_start_in_all - warmup_bars;
        let bars = all_bars[window_start..eval_end_in_all].to_vec();
        let eval_start = warmup_bars;

        let series: [TfSeries; TF_COUNT] = std::array::from_fn(|i| aggregate(&bars, ALL_TFS[i]));
        let completed: [Vec<usize>; TF_COUNT] =
            std::array::from_fn(|i| completed_counts(&bars, &series[i].end_time));

        Ok(Dataset {
            bars,
            eval_start,
            from,
            to,
            series,
            completed,
            cache: IndicatorCache::new(cache_mb),
        })
    }

    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    pub fn eval_start(&self) -> usize {
        self.eval_start
    }

    pub fn eval_bars(&self) -> &[Bar] {
        &self.bars[self.eval_start..]
    }

    pub fn series(&self, tf: Tf) -> &TfSeries {
        &self.series[tf.idx()]
    }

    pub fn completed(&self, tf: Tf, t: usize) -> usize {
        self.completed[tf.idx()][t]
    }

    pub fn indicator(&self, key: IndicatorKey) -> Arc<IndicatorSeries> {
        let series = &self.series[key.tf.idx()];
        self.cache
            .get_or_compute(key, || indicators::compute(series, key))
    }

    pub fn missing_weekdays(&self) -> Vec<chrono::NaiveDate> {
        use chrono::{DateTime, Datelike, Utc, Weekday};

        let present: std::collections::BTreeSet<chrono::NaiveDate> = self
            .eval_bars()
            .iter()
            .filter_map(|b| DateTime::<Utc>::from_timestamp(b.open_time, 0))
            .map(|dt| dt.date_naive())
            .collect();

        let start_date = DateTime::<Utc>::from_timestamp(self.from, 0)
            .expect("Dataset::from must be a valid UTC timestamp")
            .date_naive();
        let end_date = DateTime::<Utc>::from_timestamp(self.to - 1, 0)
            .expect("Dataset::to must be a valid UTC timestamp")
            .date_naive();

        let mut missing = Vec::new();
        let mut date = start_date;
        while date <= end_date {
            let is_weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
            if !is_weekend && !present.contains(&date) {
                missing.push(date);
            }
            date = date
                .succ_opt()
                .expect("date arithmetic should not overflow within an evaluation period");
        }
        missing
    }
}

#[cfg(test)]
mod tests;
