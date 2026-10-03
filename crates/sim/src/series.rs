//! 足の配列、上位足への集約、完成判定（spec 6.1〜6.2 章）。
//!
//! 最重要の性質は「未来非依存」である: 足 `t` より後の足を書き換えても、足 `t` の時点で
//! 見える上位足・指標・`completed` の値は変わらない。この性質は実装のアルゴリズムではなく
//! データ構造の作り方から来る: 上位足のバケットは `open_time` だけから決まり、各バケットの
//! 値はそのバケットに属する M5 だけから計算する。あるバケットが「完成している」と判定される
//! のは、そのバケットに属する最後の M5 がすでに現れた時点ちょうどであり、それより後に現れる
//! バケットの値は、以前に完成したバケットの値に影響しない。

use crate::error::SimError;
use crate::indicators::{self, IndicatorCache, IndicatorKey, IndicatorSeries};
use crate::types::{Bar, M5_SECS, mid2_to_yen};
use std::sync::Arc;

/// 足の時間足（spec 6.1〜6.2 章）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tf {
    M5,
    M15,
    H1,
    H4,
}

/// `Dataset` 内部の配列の添字として使う。`Tf` のバリアント数と一致させる
/// (`ALL_TFS` の順序とも一致させる)。
const TF_COUNT: usize = 4;
const ALL_TFS: [Tf; TF_COUNT] = [Tf::M5, Tf::M15, Tf::H1, Tf::H4];

impl Tf {
    /// 1 本の長さ（秒）。
    pub fn secs(self) -> i64 {
        match self {
            Tf::M5 => M5_SECS,
            Tf::M15 => 900,
            Tf::H1 => 3600,
            Tf::H4 => 14400,
        }
    }

    /// `"M5"` 等の文字列から `Tf` を得る。不明な文字列は `None`
    /// (spec 8.2 章: `tf` が範囲外なら実行時エラーとするのは呼び出し元 `script.rs` の責務)。
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

/// 1 つの時間足の系列。値は中値（円）。`end_time` はバケットの終了時刻
/// (UTC エポック秒。`completed` の判定に使う)。
#[derive(Debug, Clone, PartialEq)]
pub struct TfSeries {
    pub open: Vec<f64>,
    pub high: Vec<f64>,
    pub low: Vec<f64>,
    pub close: Vec<f64>,
    pub end_time: Vec<i64>,
}

/// `bars`（`open_time` 昇順）を `tf` のバケット長で集約する（spec 6.2 章）。
/// バケットは UTC エポック秒をバケット長で割った商で決める。バケット内に存在する M5 から
/// 最初の open・最大の high・最小の low・最後の close を取る。M5 が 1 本もないバケットは作らない。
///
/// 価格の集約は `mid2`（`i64`）で行い、バケットが確定した時点で 1 度だけ円（`f64`）に変換する
/// (spec 3 章: 価格の比較・加減算は整数で行う)。
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

/// `bars[t]` の時点で完成している `end_time` 内のバケット数（添字 `t` ごと）。
/// バケット `B` は `end_time[B] <= bars[t].open_time + M5_SECS` のとき完成している
/// (spec 6.2 章)。`end_time` は昇順なので、2 ポインタで O(本数) に求める。
fn completed_counts(bars: &[Bar], end_time: &[i64]) -> Vec<usize> {
    let mut result = Vec::with_capacity(bars.len());
    let mut ptr = 0usize;
    for bar in bars {
        let threshold = bar.open_time + M5_SECS;
        while ptr < end_time.len() && end_time[ptr] <= threshold {
            ptr += 1;
        }
        result.push(ptr);
    }
    result
}

/// 1 日の秒数（UTC。`from` は UTC 0:00 なので日付境界の計算に使う）。
const SECS_PER_DAY: i64 = 86_400;

/// `all_bars`（`open_time` 昇順）のもとで指定可能な最も早い `from`（UTC 0:00 のエポック秒）。
///
/// 「`open_time < D 0:00` の足が `warmup_bars` 本以上になる最初の UTC 日付 D」の 0:00 を返す
/// （spec 9.1 章: `from` は UTC 0:00。13 章: `warmup_bars` を満たす最初の UTC の日付）。
/// これは `all_bars[warmup_bars - 1]` が属する日の翌日 0:00 である。単に `all_bars[warmup_bars]`
/// の日付 0:00 とすると、その足が 0:00 ちょうどでない限り、それより前の足が `warmup_bars`
/// 本に満たなくなるため使えない。
///
/// `warmup_bars == 0` なら先頭の足の日付の 0:00。足が `warmup_bars` 本未満（0 本を含む）なら
/// `None`。評価期間に足があるか（`to` の側）はここでは判定しない（`Dataset::new` の責務）。
///
/// 後続の Task 9（CLI）の `--from` 省略時の既定値（spec 13 章）にもこの関数を使う想定。
pub fn earliest_from(all_bars: &[Bar], warmup_bars: usize) -> Option<i64> {
    if warmup_bars == 0 {
        let first = all_bars.first()?;
        return Some(first.open_time.div_euclid(SECS_PER_DAY) * SECS_PER_DAY);
    }
    let last_warmup = all_bars.get(warmup_bars - 1)?.open_time;
    Some((last_warmup.div_euclid(SECS_PER_DAY) + 1) * SECS_PER_DAY)
}

/// 評価期間 `[from, to)` と、その前の `warmup_bars` 本を保持する足の集合。
/// 上位足の系列・完成本数・指標キャッシュは、この読み込み範囲（warmup を含む）に対して
/// 1 度だけ計算する。
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
    /// `all_bars` は `open_time` 昇順。評価期間 `[from, to)` とその前の `warmup_bars` 本だけを
    /// 保持する。
    ///
    /// - `from` より前の足が `warmup_bars` 本に満たない場合: `SimError::Args`。メッセージには、
    ///   `all_bars`（読み込んだ全履歴）のもとで指定可能な最も早い `from` の日付を含める
    ///   （`earliest_from` の UTC 日付）。
    /// - 評価期間内の足が 0 本の場合: `SimError::Args`。
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
                "no bars in evaluation period [{from}, {to}) (UTC epoch seconds); nothing to simulate"
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
                "need {warmup_bars} warmup bars before `from`, only {eval_start_in_all} available; {detail}"
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

    /// warmup を含む読み込み範囲。
    pub fn bars(&self) -> &[Bar] {
        &self.bars
    }

    /// 評価期間の先頭の添字（`bars()` に対する添字。常に `warmup_bars` と等しい）。
    pub fn eval_start(&self) -> usize {
        self.eval_start
    }

    /// 評価期間だけの足。
    pub fn eval_bars(&self) -> &[Bar] {
        &self.bars[self.eval_start..]
    }

    /// `tf` の系列（`bars()` と同じ読み込み範囲に対して計算済み）。
    pub fn series(&self, tf: Tf) -> &TfSeries {
        &self.series[tf.idx()]
    }

    /// 足 `t`（`bars()` に対する添字）の時点で完成している `tf` の本数。
    pub fn completed(&self, tf: Tf, t: usize) -> usize {
        self.completed[tf.idx()][t]
    }

    /// `key` の指標系列。キャッシュ済みなら共有し、未済なら計算してキャッシュする。
    pub fn indicator(&self, key: IndicatorKey) -> Arc<IndicatorSeries> {
        let series = &self.series[key.tf.idx()];
        self.cache
            .get_or_compute(key, || indicators::compute(series, key))
    }

    /// 評価期間内で足が 1 本もない平日（UTC の月曜〜金曜）。
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
        // `to` は排他的境界なので、最後に含める日は `to - 1` 秒が属する日。
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
mod tests {
    use super::*;
    use crate::indicators::{IndicatorKey, IndicatorKind};
    use chrono::NaiveDate;

    /// テスト用の `Bar` を作るヘルパー。スプレッドは 0（bid == ask）、`open_time` は分単位で
    /// 指定する（`minute_offset * 60` 秒）。`open_time_secs` を直接指定する版は
    /// `bar_at_secs` を使う。
    fn bar_at_secs(open_time: i64, mid_close_yen: f64) -> Bar {
        // 円 → ミリ円 → mid2(bid+ask, スプレッド0なので mid2 = 2 * milli)。
        let milli = (mid_close_yen * 1000.0).round() as i64;
        Bar {
            open_time,
            bid_open: milli,
            bid_high: milli,
            bid_low: milli,
            bid_close: milli,
            ask_open: milli,
            ask_high: milli,
            ask_low: milli,
            ask_close: milli,
        }
    }

    /// open/high/low/close を個別に指定できる版（バケット集約のテストで open≠close を作る）。
    fn bar_ohlc(open_time: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
        let to_milli = |v: f64| (v * 1000.0).round() as i64;
        Bar {
            open_time,
            bid_open: to_milli(open),
            bid_high: to_milli(high),
            bid_low: to_milli(low),
            bid_close: to_milli(close),
            ask_open: to_milli(open),
            ask_high: to_milli(high),
            ask_low: to_milli(low),
            ask_close: to_milli(close),
        }
    }

    const MIN: i64 = 60;

    // ---- Step 1: 集約 -----------------------------------------------------

    #[test]
    fn m15_bucket_takes_first_open_max_high_min_low_last_close() {
        // 00:00, 00:05, 00:10 の 3 本の M5 が同じ M15 バケットに入る。
        let bars = vec![
            bar_ohlc(0, 150.00, 150.05, 149.95, 150.02),
            bar_ohlc(5 * MIN, 150.02, 150.10, 150.00, 150.08),
            bar_ohlc(10 * MIN, 150.08, 150.09, 149.90, 150.03),
        ];
        let series = aggregate(&bars, Tf::M15);
        assert_eq!(series.open.len(), 1);
        assert_eq!(series.open[0], 150.00);
        assert_eq!(series.high[0], 150.10);
        assert_eq!(series.low[0], 149.90);
        assert_eq!(series.close[0], 150.03);
        assert_eq!(series.end_time[0], 15 * MIN);
    }

    #[test]
    fn leading_partial_bucket_uses_only_the_m5_bars_present() {
        // 読み込み範囲の先頭が M15 バケットの途中（分 05）から始まる場合、先頭の M15 は
        // 存在する 2 本（05, 10）だけで作られる。
        let bars = vec![
            bar_ohlc(5 * MIN, 150.02, 150.10, 150.00, 150.08),
            bar_ohlc(10 * MIN, 150.08, 150.09, 149.90, 150.03),
        ];
        let series = aggregate(&bars, Tf::M15);
        assert_eq!(series.open.len(), 1);
        assert_eq!(series.open[0], 150.02); // 先頭の M5 の open（05 分の open）
        assert_eq!(series.high[0], 150.10);
        assert_eq!(series.low[0], 149.90);
        assert_eq!(series.close[0], 150.03);
        assert_eq!(series.end_time[0], 15 * MIN); // バケットの終了時刻は変わらない
    }

    #[test]
    fn bucket_with_no_m5_bars_is_not_created() {
        // 金曜 23:55 の次が月曜 00:00（週末を挟む）。間の M15/H1/H4 バケットは作られない。
        let friday_close = 23 * 3600 + 55 * MIN;
        let monday_open = friday_close + 300 + 2 * 24 * 3600; // 土日を挟んで月曜
        let bars = vec![
            bar_ohlc(friday_close, 150.00, 150.01, 149.99, 150.00),
            bar_ohlc(monday_open, 150.50, 150.51, 150.49, 150.50),
        ];
        for tf in [Tf::M15, Tf::H1, Tf::H4] {
            let series = aggregate(&bars, tf);
            // 2 本の M5 が同じバケットに入らない限り、ちょうど 2 要素になる
            // (週末を挟むので別バケット)。
            assert_eq!(
                series.open.len(),
                2,
                "{tf:?}: expected no bucket for the empty weekend gap"
            );
        }
    }

    #[test]
    fn completed_increases_exactly_when_the_bucket_last_bar_arrives() {
        // 分が 00, 05, 10 の 3 本。M15 バケットは 10 分の足が来た時点で完成する。
        let bars = vec![
            bar_at_secs(0, 150.00),
            bar_at_secs(5 * MIN, 150.01),
            bar_at_secs(10 * MIN, 150.02),
        ];
        let series = aggregate(&bars, Tf::M15);
        let completed = completed_counts(&bars, &series.end_time);
        assert_eq!(completed[0], 0, "at :00 the M15 bucket is not complete yet");
        assert_eq!(completed[1], 0, "at :05 the M15 bucket is not complete yet");
        assert_eq!(completed[2], 1, "at :10 the M15 bucket just completed");
    }

    #[test]
    fn future_bars_do_not_affect_past_completed_or_series_values() {
        // 足 t より後の足の値を書き換えても、足 t の時点の completed と上位足の値は変わらない
        // (すべての上位時間足で確認)。
        // 240 本 = 20 時間。H4 が 4 本完成するまで含める（空のスライス同士の比較で空振りしない）。
        let n = 240usize;
        let base: Vec<Bar> = (0..n)
            .map(|i| {
                let i = i as i64;
                bar_ohlc(
                    i * 5 * MIN,
                    150.0 + i as f64 * 0.01,
                    150.0 + i as f64 * 0.01 + 0.03,
                    150.0 + i as f64 * 0.01 - 0.03,
                    150.0 + i as f64 * 0.01 + 0.01,
                )
            })
            .collect();
        let to = base.last().unwrap().open_time + M5_SECS;
        let dataset_a = Dataset::new(base.clone(), 0, to, 0, 64).unwrap();

        // バケットの途中（2, 100, 200）、ちょうど完成する足（M15: 2、H1: 11、H4: 47）、その直前
        // （3 は M15 の次バケット途中、12 は H1 の次、48 は H4 の次の足）の前後を含める。
        let probe_ts = [2usize, 3, 11, 12, 47, 48, 100, 200];
        let mut nonempty_seen = [false; TF_COUNT];

        for &t in &probe_ts {
            let mut mutated = base.clone();
            for bar in mutated.iter_mut().skip(t + 1) {
                // 後続の足の値を大きく変える（時刻は変えない）。
                *bar = bar_ohlc(bar.open_time, 999.0, 1000.0, 998.0, 999.5);
            }
            let dataset_b = Dataset::new(mutated, 0, to, 0, 64).unwrap();

            for tf in ALL_TFS {
                let visible = dataset_a.completed(tf, t);
                assert_eq!(
                    visible,
                    dataset_b.completed(tf, t),
                    "{tf:?} t={t}: completed(t) must not depend on future bars"
                );
                if visible >= 1 {
                    nonempty_seen[tf.idx()] = true;
                }
                let (a, b) = (dataset_a.series(tf), dataset_b.series(tf));
                assert_eq!(a.open[..visible], b.open[..visible], "{tf:?} t={t}: open");
                assert_eq!(a.high[..visible], b.high[..visible], "{tf:?} t={t}: high");
                assert_eq!(a.low[..visible], b.low[..visible], "{tf:?} t={t}: low");
                assert_eq!(
                    a.close[..visible],
                    b.close[..visible],
                    "{tf:?} t={t}: close"
                );
                assert_eq!(
                    a.end_time[..visible],
                    b.end_time[..visible],
                    "{tf:?} t={t}: end_time"
                );

                let sma_key = IndicatorKey {
                    tf,
                    kind: IndicatorKind::Sma,
                    period: 3,
                    mult_x100: 0,
                };
                let bb_key = IndicatorKey {
                    tf,
                    kind: IndicatorKind::Bb,
                    period: 3,
                    mult_x100: 200,
                };
                let bits = |v: &[f64]| -> Vec<u64> { v.iter().map(|x| x.to_bits()).collect() };
                match (
                    &*dataset_a.indicator(sma_key),
                    &*dataset_b.indicator(sma_key),
                ) {
                    (IndicatorSeries::Single(x), IndicatorSeries::Single(y)) => assert_eq!(
                        bits(&x[..visible]),
                        bits(&y[..visible]),
                        "{tf:?} t={t}: sma must not depend on future bars"
                    ),
                    _ => panic!("sma must be Single"),
                }
                match (&*dataset_a.indicator(bb_key), &*dataset_b.indicator(bb_key)) {
                    (
                        IndicatorSeries::Band {
                            lower: l1,
                            middle: m1,
                            upper: u1,
                        },
                        IndicatorSeries::Band {
                            lower: l2,
                            middle: m2,
                            upper: u2,
                        },
                    ) => {
                        for (name, x, y) in
                            [("lower", l1, l2), ("middle", m1, m2), ("upper", u1, u2)]
                        {
                            assert_eq!(
                                bits(&x[..visible]),
                                bits(&y[..visible]),
                                "{tf:?} t={t}: bb.{name} must not depend on future bars"
                            );
                        }
                    }
                    _ => panic!("bb must be Band"),
                }
            }
        }

        for tf in ALL_TFS {
            assert!(
                nonempty_seen[tf.idx()],
                "{tf:?}: no probe t had a completed bar; the comparison would be vacuous"
            );
        }
    }

    #[test]
    fn leading_partial_bucket_completes_by_end_time_alone() {
        // 先頭が M15 バケットの途中（分 05, 10）から始まる場合、完成判定は終了時刻だけで決まる:
        // 05 分の足では未完成、10 分の足（end_time 15 分 <= 10 分 + 5 分）で完成。
        let bars = vec![bar_at_secs(5 * MIN, 150.00), bar_at_secs(10 * MIN, 150.01)];
        let to = 15 * MIN;
        let dataset = Dataset::new(bars, 0, to, 0, 64).unwrap();
        assert_eq!(dataset.completed(Tf::M15, 0), 0);
        assert_eq!(dataset.completed(Tf::M15, 1), 1);
    }

    // ---- Step 2: Dataset::new ---------------------------------------------

    fn make_bars(n: usize, start_secs: i64) -> Vec<Bar> {
        (0..n)
            .map(|i| bar_at_secs(start_secs + i as i64 * 5 * MIN, 150.0 + i as f64 * 0.001))
            .collect()
    }

    #[test]
    fn new_succeeds_when_warmup_count_matches_exactly() {
        let warmup_bars = 5;
        let eval_count = 3;
        let all_bars = make_bars(warmup_bars + eval_count, 0);
        let from = all_bars[warmup_bars].open_time;
        let to = all_bars[warmup_bars + eval_count - 1].open_time + M5_SECS; // 最後の足を含む

        let dataset = Dataset::new(all_bars, from, to, warmup_bars, 64)
            .expect("exact warmup count must succeed");
        assert_eq!(dataset.eval_start(), warmup_bars);
        assert_eq!(dataset.bars().len(), warmup_bars + eval_count);
    }

    fn utc_secs(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, 0)
            .unwrap()
            .and_utc()
            .timestamp()
    }

    #[test]
    fn earliest_from_is_next_midnight_when_both_boundary_bars_are_mid_day() {
        // 2024-01-03 22:00 から 5 分間隔。bars[4] = 22:20、bars[5] = 22:25 (どちらも 01-03 の途中)。
        // 01-03 0:00 より前の足は 0 本なので、5 本そろうのは 01-04 0:00 が最初。
        let bars = make_bars(60, utc_secs(2024, 1, 3, 22, 0));
        assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 4, 0, 0)));
    }

    #[test]
    fn earliest_from_is_that_midnight_when_the_first_eval_bar_is_exactly_midnight() {
        // bars[4] = 01-03 23:55、bars[5] = 01-04 0:00 ちょうど。
        let bars = make_bars(20, utc_secs(2024, 1, 3, 23, 35));
        assert_eq!(bars[5].open_time, utc_secs(2024, 1, 4, 0, 0));
        assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 4, 0, 0)));
    }

    #[test]
    fn earliest_from_skips_to_saturday_midnight_across_a_weekend_gap() {
        // bars[4] = 金曜 21:55、bars[5] = 日曜 22:00。金曜 0:00 より前の足は 5 本に満たないので、
        // 土曜 0:00 が最初。
        let friday = utc_secs(2024, 1, 5, 21, 35); // 2024-01-05 は金曜
        let mut bars = make_bars(5, friday);
        assert_eq!(bars[4].open_time, utc_secs(2024, 1, 5, 21, 55));
        bars.extend(make_bars(10, utc_secs(2024, 1, 7, 22, 0)));
        assert_eq!(earliest_from(&bars, 5), Some(utc_secs(2024, 1, 6, 0, 0)));
    }

    #[test]
    fn earliest_from_edge_cases() {
        let bars = make_bars(5, utc_secs(2024, 1, 3, 22, 0));
        assert_eq!(
            earliest_from(&bars, 0),
            Some(utc_secs(2024, 1, 3, 0, 0)),
            "warmup 0 → date of the first bar"
        );
        assert_eq!(
            earliest_from(&bars, 5),
            Some(utc_secs(2024, 1, 4, 0, 0)),
            "exactly warmup_bars bars → next midnight after the last of them"
        );
        assert_eq!(earliest_from(&bars, 6), None, "fewer than warmup_bars bars");
        assert_eq!(earliest_from(&[], 0), None);
    }

    #[test]
    fn new_accepts_earliest_from_and_rejects_one_day_earlier_naming_that_date() {
        let warmup_bars = 5;
        let all_bars = make_bars(60, utc_secs(2024, 1, 3, 22, 0));
        let earliest = earliest_from(&all_bars, warmup_bars).expect("enough bars");
        let to = all_bars.last().unwrap().open_time + M5_SECS;

        Dataset::new(all_bars.clone(), earliest, to, warmup_bars, 64)
            .expect("earliest_from itself must be accepted");

        let err = Dataset::new(all_bars, earliest - SECS_PER_DAY, to, warmup_bars, 64)
            .expect_err("one day earlier has too few warmup bars");
        assert!(matches!(err, SimError::Args(_)));
        let message = err.to_string();
        assert!(
            message.contains("2024-01-04"),
            "message should name the earliest `from` date (2024-01-04): {message}"
        );
    }

    #[test]
    fn new_rejects_exactly_one_warmup_bar_short_and_accepts_exactly_enough() {
        let warmup_bars = 5;
        // 01-03 の 23:40〜23:55 に 4 本（= warmup_bars - 1）、01-04 の昼に 1 本、
        // 01-05 0:00 以降に 10 本（評価期間の足）。
        let mut all_bars = make_bars(4, utc_secs(2024, 1, 3, 23, 40));
        all_bars.push(bar_at_secs(utc_secs(2024, 1, 4, 12, 0), 150.0));
        all_bars.extend(make_bars(10, utc_secs(2024, 1, 5, 0, 0)));
        let to = all_bars.last().unwrap().open_time + M5_SECS;
        let earliest = earliest_from(&all_bars, warmup_bars).expect("enough bars in total");
        assert_eq!(earliest, utc_secs(2024, 1, 5, 0, 0));

        // from = 01-04 0:00: それより前の足はちょうど warmup_bars - 1 = 4 本。
        let one_short_from = utc_secs(2024, 1, 4, 0, 0);
        let err = Dataset::new(all_bars.clone(), one_short_from, to, warmup_bars, 64)
            .expect_err("4 bars before `from` is one short of warmup_bars = 5");
        assert!(matches!(err, SimError::Args(_)));
        let message = err.to_string();
        assert!(
            message.contains("2024-01-05"),
            "message should name the earliest `from` date (2024-01-05): {message}"
        );

        // from = 01-05 0:00: 同じデータで、それより前の足がちょうど 5 本。
        let dataset = Dataset::new(all_bars, earliest, to, warmup_bars, 64)
            .expect("exactly warmup_bars bars before `from` must succeed");
        assert_eq!(dataset.eval_start(), warmup_bars);
    }

    #[test]
    fn new_fails_when_evaluation_period_has_no_bars() {
        let warmup_bars = 2;
        let all_bars = make_bars(warmup_bars + 3, 0);
        let last_open = all_bars.last().unwrap().open_time;
        // 評価期間を、最後の足より後ろに置く(足が 1 本もない)。
        let from = last_open + 10 * M5_SECS;
        let to = from + M5_SECS;

        let err = Dataset::new(all_bars, from, to, warmup_bars, 64)
            .expect_err("empty evaluation period must be rejected");
        assert!(matches!(err, SimError::Args(_)));
    }

    #[test]
    fn missing_weekdays_reports_only_weekdays_without_bars() {
        // 2024-01-01(月)は足あり、2024-01-02(火)は足なし、2024-01-06/07(土日)は対象外。
        let monday = NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let warmup_bars = 1;
        let all_bars = vec![
            bar_at_secs(monday - 5 * MIN, 149.0), // warmup
            bar_at_secs(monday, 150.0),           // 月曜だけ足がある
        ];
        // 評価期間は月曜0:00から日曜0:00まで(月〜日)。火〜金は足なし、土日は対象外。
        let from = monday;
        let to = monday + 7 * 24 * 3600;

        let dataset = Dataset::new(all_bars, from, to, warmup_bars, 64).unwrap();
        let missing = dataset.missing_weekdays();
        let expected: Vec<NaiveDate> = vec![
            NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(), // 火
            NaiveDate::from_ymd_opt(2024, 1, 3).unwrap(), // 水
            NaiveDate::from_ymd_opt(2024, 1, 4).unwrap(), // 木
            NaiveDate::from_ymd_opt(2024, 1, 5).unwrap(), // 金
        ];
        assert_eq!(missing, expected);
    }

    // ---- Dataset accessors / indicator delegation --------------------------

    #[test]
    fn indicator_delegates_to_the_series_for_the_requested_timeframe() {
        let warmup_bars = 5;
        let all_bars = make_bars(warmup_bars + 10, 0);
        let from = all_bars[warmup_bars].open_time;
        let to = all_bars.last().unwrap().open_time + M5_SECS;
        let dataset = Dataset::new(all_bars, from, to, warmup_bars, 64).unwrap();

        let key = IndicatorKey {
            tf: Tf::M5,
            kind: IndicatorKind::Sma,
            period: 3,
            mult_x100: 0,
        };
        let result = dataset.indicator(key);
        match &*result {
            IndicatorSeries::Single(values) => {
                assert_eq!(values.len(), dataset.series(Tf::M5).close.len());
            }
            _ => panic!("sma must be a Single series"),
        }
    }

    #[test]
    fn tf_parse_roundtrips_known_strings_and_rejects_unknown() {
        assert_eq!(Tf::parse("M5"), Some(Tf::M5));
        assert_eq!(Tf::parse("M15"), Some(Tf::M15));
        assert_eq!(Tf::parse("H1"), Some(Tf::H1));
        assert_eq!(Tf::parse("H4"), Some(Tf::H4));
        assert_eq!(Tf::parse("D1"), None);
        assert_eq!(Tf::parse(""), None);
    }

    #[test]
    fn tf_secs_match_spec() {
        assert_eq!(Tf::M5.secs(), 300);
        assert_eq!(Tf::M15.secs(), 900);
        assert_eq!(Tf::H1.secs(), 3600);
        assert_eq!(Tf::H4.secs(), 14400);
    }
}
