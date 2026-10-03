//! 評価指標の計算（spec 10 章）。
//!
//! `evaluate` は `outcome.status` が `RunStatus::Ok` の場合にだけ呼ぶ契約（Task 7 Produces の
//! コメント）。`script_error` の実行は `metrics` 列自体を NULL にする設計（spec 12 章）であり、
//! 指標を計算する意味がないため、ここでは `RunStatus` を一切参照しない。呼び出し元
//! （Task 8 `sweep.rs`/`store.rs` の責務）が `status` で分岐する。
//!
//! 金額（損益 `pnl_milli` と基準値 `ideal_milli`/`realizable_milli`）はミリ円の `i64` のまま
//! 扱い、出力直前に `milli_to_pips`/`mid2_to_pips` で pips（`f64`）へ 1 回だけ変換する
//! （spec 3 章）。

use crate::benchmark::{Benchmark, Leg};
use crate::engine::{ExitReason, SimOutcome, SimTrade};
use crate::types::{Bar, mid2_to_pips, milli_to_pips};
use std::collections::BTreeMap;

/// 捕捉できなかった波（spec 10 章）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MissedLeg {
    pub start_time: String,
    pub end_time: String,
    pub direction: i8,
    pub realizable_pips: f64,
    pub flat_bars: u32,
    pub opposite_bars: u32,
}

/// 1 つの折り返し幅（`theta_pips`）についての指標（spec 10 章）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ThetaMetrics {
    pub leg_count: usize,
    pub ideal_pips: f64,
    pub realizable_pips: f64,
    pub capture_rate: Option<f64>,
    pub correct_side_ratio: Option<f64>,
    pub mean_lag_bars: Option<f64>,
    pub mean_lag_pips: Option<f64>,
    pub missed_legs: Vec<MissedLeg>,
}

/// 1 回のシミュレーションの評価指標一式（spec 10 章）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Metrics {
    pub total_pips: f64,
    pub trade_count: usize,
    pub win_rate: f64,
    pub max_drawdown_pips: f64,
    pub time_in_market: f64,
    pub protective_stop_count: usize,
    pub segments: [f64; 6],
    pub by_theta: BTreeMap<String, ThetaMetrics>,
}

impl Metrics {
    /// `sim_runs.metrics` 列の JSON（spec 10 章）。`segments` と `by_theta` だけを含み、
    /// 全体指標の他のフィールド（`total_pips` 等）は `sim_runs` の専用列に入るため含めない。
    pub fn metrics_json(&self) -> serde_json::Value {
        serde_json::json!({
            "segments": self.segments,
            "by_theta": self.by_theta,
        })
    }
}

/// 評価期間の足と売買から、全体指標と `theta` ごとの指標を計算する（spec 10 章）。
///
/// `outcome.status` が `RunStatus::Ok` の場合にだけ呼ぶこと。`outcome.positions` と
/// `eval_bars`、および各 `benchmarks[i].labels` は同じ長さ（評価期間の足数）であることを
/// 前提とする（`Dataset`/`simulate`/`benchmark::compute` がいずれも評価期間の足数に対して
/// 一貫した配列を返すため、呼び出し元がこれらを揃えて渡す限り常に成り立つ）。
pub fn evaluate(eval_bars: &[Bar], outcome: &SimOutcome, benchmarks: &[Benchmark]) -> Metrics {
    let trades = &outcome.trades;
    let positions = &outcome.positions;

    let total_milli: i64 = trades.iter().map(|t| t.pnl_milli).sum();
    let total_pips = milli_to_pips(total_milli);
    let trade_count = trades.len();
    let win_rate = if trade_count == 0 {
        0.0
    } else {
        trades.iter().filter(|t| t.pnl_milli > 0).count() as f64 / trade_count as f64
    };
    let max_drawdown_pips = max_drawdown(trades);
    let time_in_market =
        positions.iter().filter(|&&p| p != 0).count() as f64 / eval_bars.len() as f64;
    let protective_stop_count = trades
        .iter()
        .filter(|t| t.reason == ExitReason::ProtectiveStop)
        .count();
    let segments = compute_segments(eval_bars.len(), trades);

    let by_theta = benchmarks
        .iter()
        .map(|bm| {
            (
                bm.theta_pips.to_string(),
                theta_metrics(eval_bars, positions, bm, total_milli),
            )
        })
        .collect();

    Metrics {
        total_pips,
        trade_count,
        win_rate,
        max_drawdown_pips,
        time_in_market,
        protective_stop_count,
        segments,
        by_theta,
    }
}

/// 決済ごとの累積損益の、最高値（初期値 0）からの最大下落幅（spec 10 章）。
/// `trades` は `simulate` が返す順序（決済が起きた順）であることを前提にする
/// （`engine::simulate` は足を先頭から処理するため、`trades` は常にこの順で積まれる）。
fn max_drawdown(trades: &[SimTrade]) -> f64 {
    // 累積・最高値・下落幅はすべてミリ円（i64）で持ち、最後に 1 回だけ pips へ変換する（spec 3 章）。
    let mut peak = 0i64;
    let mut cumulative = 0i64;
    let mut max_dd = 0i64;
    for trade in trades {
        cumulative += trade.pnl_milli;
        peak = peak.max(cumulative);
        max_dd = max_dd.max(peak - cumulative);
    }
    milli_to_pips(max_dd)
}

/// 評価期間を足数で 6 等分した各区間の損益合計（spec 10 章: 余りは最後の区間に含める。
/// 売買は決済した足の区間に計上する）。
fn compute_segments(n: usize, trades: &[SimTrade]) -> [f64; 6] {
    // 区間ごとにミリ円（i64）で合計し、最後に 1 回だけ pips へ変換する（spec 3 章）。
    let base = n / 6;
    let remainder = n % 6;
    let mut boundaries = [0usize; 7];
    for i in 0..6 {
        let size = if i < 5 { base } else { base + remainder };
        boundaries[i + 1] = boundaries[i] + size;
    }
    let mut segments_milli = [0i64; 6];
    for trade in trades {
        let idx = segment_index(trade.exit_idx, &boundaries);
        segments_milli[idx] += trade.pnl_milli;
    }
    segments_milli.map(milli_to_pips)
}

/// `boundaries`（7 要素、区間の境界）の中で `exit_idx` が属する区間（0〜5）を返す。
fn segment_index(exit_idx: usize, boundaries: &[usize; 7]) -> usize {
    for (i, window) in boundaries.windows(2).enumerate() {
        if exit_idx < window[1] {
            return i;
        }
    }
    5
}

/// 1 つの波について、方向ラベル区間（`a+1..=b`）内の `positions` との突き合わせ結果。
struct LegEval<'a> {
    leg: &'a Leg,
    matched: usize,
    total: usize,
    flat_bars: u32,
    opposite_bars: u32,
    /// 区間内で最初に `positions[i] == leg.direction` になった添字。
    first_match_idx: Option<usize>,
}

/// `leg` の方向ラベル区間と `positions` を突き合わせる（spec 10 章の `correct_side_ratio`、
/// `missed_legs`、`mean_lag_*` の元データ）。`positions[i]` は `leg.direction`（一致）、`0`
/// （`flat_bars`）、その逆符号（`opposite_bars`）のいずれかであり、この 3 分類で尽くされる
/// （`position` は `{-1, 0, 1}`、`direction` は `{-1, 1}` のため）。
fn evaluate_leg<'a>(leg: &'a Leg, positions: &[i8]) -> LegEval<'a> {
    let start = leg.a + 1;
    let end = leg.b;
    let mut matched = 0usize;
    let mut flat_bars = 0u32;
    let mut opposite_bars = 0u32;
    let mut first_match_idx = None;
    for (i, &p) in positions.iter().enumerate().take(end + 1).skip(start) {
        if p == leg.direction {
            matched += 1;
            if first_match_idx.is_none() {
                first_match_idx = Some(i);
            }
        } else if p == 0 {
            flat_bars += 1;
        } else {
            opposite_bars += 1;
        }
    }
    LegEval {
        leg,
        matched,
        total: end - start + 1,
        flat_bars,
        opposite_bars,
        first_match_idx,
    }
}

/// 1 つの `theta_pips` についての指標一式（spec 10 章）。
fn theta_metrics(
    eval_bars: &[Bar],
    positions: &[i8],
    bm: &Benchmark,
    total_milli: i64,
) -> ThetaMetrics {
    let ideal_pips = milli_to_pips(bm.ideal_milli);
    let realizable_pips = milli_to_pips(bm.realizable_milli);
    let capture_rate = if bm.realizable_milli > 0 {
        Some(total_milli as f64 / bm.realizable_milli as f64)
    } else {
        None
    };

    let labeled_count = bm.labels.iter().filter(|&&l| l != 0).count();
    let correct_side_ratio = if labeled_count == 0 {
        None
    } else {
        let matched = (0..bm.labels.len())
            .filter(|&i| bm.labels[i] != 0 && positions[i] == bm.labels[i])
            .count();
        Some(matched as f64 / labeled_count as f64)
    };

    let leg_evals: Vec<LegEval> = bm
        .legs
        .iter()
        .map(|leg| evaluate_leg(leg, positions))
        .collect();

    let (mean_lag_bars, mean_lag_pips) = mean_lag(eval_bars, &leg_evals);
    let missed_legs = missed_legs(eval_bars, &leg_evals);

    ThetaMetrics {
        leg_count: bm.legs.len(),
        ideal_pips,
        realizable_pips,
        capture_rate,
        correct_side_ratio,
        mean_lag_bars,
        mean_lag_pips,
        missed_legs,
    }
}

/// 方向が一致した足を 1 本以上含む波についての平均ラグ（本数・pips）（spec 10 章）。
/// 対象の波が 1 つもなければ両方 `None`。
fn mean_lag(eval_bars: &[Bar], leg_evals: &[LegEval]) -> (Option<f64>, Option<f64>) {
    let lagged: Vec<&LegEval> = leg_evals
        .iter()
        .filter(|e| e.first_match_idx.is_some())
        .collect();
    if lagged.is_empty() {
        return (None, None);
    }

    let bars_sum: usize = lagged
        .iter()
        .map(|e| e.first_match_idx.expect("filtered to Some above") - (e.leg.a + 1))
        .sum();
    let pips_sum: f64 = lagged
        .iter()
        .map(|e| {
            let idx = e.first_match_idx.expect("filtered to Some above");
            let diff = eval_bars[idx].mid2_close() - eval_bars[e.leg.a].mid2_close();
            mid2_to_pips(diff.abs())
        })
        .sum();

    let count = lagged.len() as f64;
    (Some(bars_sum as f64 / count), Some(pips_sum / count))
}

/// 一致率が 0.5 未満の波を、実質上限の大きい順（同値は開始時刻の早い順）に最大 20 件
/// （spec 10 章）。
///
/// 一致率の比較は `matched * 2 < total` という整数演算で行い、`matched / total < 0.5` の
/// 浮動小数点比較を避ける（`total >= 1` は波の定義 `b > a` から常に成り立つため、ゼロ除算は
/// 起きない）。
fn missed_legs(eval_bars: &[Bar], leg_evals: &[LegEval]) -> Vec<MissedLeg> {
    let mut candidates: Vec<&LegEval> = leg_evals
        .iter()
        .filter(|e| e.matched * 2 < e.total)
        .collect();
    candidates.sort_by(|a, b| {
        b.leg
            .realizable_milli
            .cmp(&a.leg.realizable_milli)
            .then(a.leg.a.cmp(&b.leg.a))
    });
    candidates.truncate(20);

    candidates
        .into_iter()
        .map(|e| MissedLeg {
            start_time: rfc3339(eval_bars[e.leg.a + 1].open_time),
            end_time: rfc3339(eval_bars[e.leg.b].open_time),
            direction: e.leg.direction,
            realizable_pips: milli_to_pips(e.leg.realizable_milli),
            flat_bars: e.flat_bars,
            opposite_bars: e.opposite_bars,
        })
        .collect()
}

/// `open_time`（UTC エポック秒）を RFC 3339（UTC、秒精度、`Z` 表記）に変換する。
fn rfc3339(open_time: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(open_time, 0)
        .expect("eval_bars[].open_time must be a valid UTC timestamp (Dataset invariant)")
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::RunStatus;
    use crate::types::M5_SECS;

    // ---- test helpers --------------------------------------------------------

    /// `mid2_close` を直接指定するテスト用 `Bar`（`open_time` は添字から決める）。
    fn bar_mid2_close(idx: usize, mid2_close: i64) -> Bar {
        let bid = mid2_close / 2;
        let ask = mid2_close - bid;
        Bar {
            open_time: idx as i64 * M5_SECS,
            bid_open: bid,
            bid_high: bid,
            bid_low: bid,
            bid_close: bid,
            ask_open: ask,
            ask_high: ask,
            ask_low: ask,
            ask_close: ask,
        }
    }

    /// 値を気にしないテスト（`eval_bars` の中身が指標計算に関与しない箇所）用の平板な足。
    fn bars(n: usize) -> Vec<Bar> {
        (0..n).map(|i| bar_mid2_close(i, 300_000)).collect()
    }

    fn trade(pnl_pips: f64, exit_idx: usize, reason: ExitReason) -> SimTrade {
        SimTrade {
            direction: 1,
            entry_idx: exit_idx,
            exit_idx,
            entry_milli: 0,
            exit_milli: (pnl_pips * 10.0).round() as i64,
            pnl_milli: (pnl_pips * 10.0).round() as i64,
            reason,
        }
    }

    fn outcome(trades: Vec<SimTrade>, positions: Vec<i8>) -> SimOutcome {
        SimOutcome {
            status: RunStatus::Ok,
            trades,
            positions,
        }
    }

    fn empty_benchmark(theta_pips: i64, n: usize) -> Benchmark {
        Benchmark {
            theta_pips,
            legs: vec![],
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        }
    }

    // ---- total_pips / trade_count / win_rate / max_drawdown_pips -------------

    #[test]
    fn three_trades_produce_total_pips_trade_count_win_rate_and_drawdown() {
        let trades = vec![
            trade(30.0, 0, ExitReason::Signal),
            trade(-10.0, 1, ExitReason::Signal),
            trade(20.0, 2, ExitReason::Signal),
        ];
        let metrics = evaluate(&bars(3), &outcome(trades, vec![1, 1, 1]), &[]);

        assert_eq!(metrics.total_pips, 40.0);
        assert_eq!(metrics.trade_count, 3);
        assert!((metrics.win_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(metrics.max_drawdown_pips, 10.0);
    }

    #[test]
    fn drawdown_from_initial_zero_peak_when_first_trade_is_a_loss() {
        let trades = vec![trade(-15.0, 0, ExitReason::Signal)];
        let metrics = evaluate(&bars(1), &outcome(trades, vec![1]), &[]);

        assert_eq!(metrics.max_drawdown_pips, 15.0);
    }

    #[test]
    fn zero_trades_yield_zero_total_pips_win_rate_and_drawdown() {
        let metrics = evaluate(&bars(2), &outcome(vec![], vec![0, 0]), &[]);

        assert_eq!(metrics.total_pips, 0.0);
        assert_eq!(metrics.trade_count, 0);
        assert_eq!(metrics.win_rate, 0.0);
        assert_eq!(metrics.max_drawdown_pips, 0.0);
    }

    // ---- time_in_market --------------------------------------------------------

    #[test]
    fn time_in_market_is_the_fraction_of_bars_with_a_nonzero_position() {
        let positions = vec![0, 1, 1, 0, -1];
        let metrics = evaluate(&bars(5), &outcome(vec![], positions), &[]);

        assert_eq!(metrics.time_in_market, 0.6);
    }

    // ---- protective_stop_count --------------------------------------------------

    #[test]
    fn protective_stop_count_counts_only_protective_stop_exits() {
        let trades = vec![
            trade(10.0, 0, ExitReason::Signal),
            trade(-5.0, 1, ExitReason::ProtectiveStop),
            trade(3.0, 2, ExitReason::EndOfData),
            trade(-2.0, 3, ExitReason::ProtectiveStop),
        ];
        let metrics = evaluate(&bars(4), &outcome(trades, vec![1, 1, 1, 1]), &[]);

        assert_eq!(metrics.protective_stop_count, 2);
    }

    // ---- segments ----------------------------------------------------------------

    #[test]
    fn segments_split_evaluation_period_into_six_parts_with_remainder_in_the_last() {
        // 13 本 -> 2,2,2,2,2,3 (境界: 0,2,4,6,8,10,13)。各区間に 1 件ずつ決済を置く。
        let trades = vec![
            trade(1.0, 1, ExitReason::Signal),  // segment 0: [0,2)
            trade(2.0, 3, ExitReason::Signal),  // segment 1: [2,4)
            trade(3.0, 5, ExitReason::Signal),  // segment 2: [4,6)
            trade(4.0, 7, ExitReason::Signal),  // segment 3: [6,8)
            trade(5.0, 9, ExitReason::Signal),  // segment 4: [8,10)
            trade(6.0, 12, ExitReason::Signal), // segment 5: [10,13)
        ];
        let metrics = evaluate(&bars(13), &outcome(trades, vec![0; 13]), &[]);

        assert_eq!(
            metrics.segments,
            [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            "each trade must be attributed to the segment containing its exit_idx"
        );
    }

    #[test]
    fn fewer_than_six_bars_put_every_trade_in_the_last_segment() {
        // n=5 -> 区間は 0,0,0,0,0,5 本（余りは最後の区間）。
        let trades = vec![
            trade(1.0, 0, ExitReason::Signal),
            trade(2.0, 4, ExitReason::Signal),
        ];
        let metrics = evaluate(&bars(5), &outcome(trades, vec![0; 5]), &[]);

        assert_eq!(metrics.segments, [0.0, 0.0, 0.0, 0.0, 0.0, 3.0]);
    }

    // ---- 損益は整数で合計し、pips 変換は 1 回だけ（spec 3 章） ------------------

    fn trade_milli(pnl_milli: i64, exit_idx: usize) -> SimTrade {
        SimTrade {
            direction: 1,
            entry_idx: exit_idx,
            exit_idx,
            entry_milli: 0,
            exit_milli: pnl_milli,
            pnl_milli,
            reason: ExitReason::Signal,
        }
    }

    #[test]
    fn profits_are_summed_in_integer_milli_so_three_tenth_pips_total_exactly_0_3() {
        // f64 で 0.1 を 3 回足すと 0.30000000000000004 になる。
        // 18 本 -> 各区間 3 本。決済 6,7,8 はすべて区間 2（[6,9)）。
        let trades = vec![trade_milli(1, 6), trade_milli(1, 7), trade_milli(1, 8)];
        let metrics = evaluate(&bars(18), &outcome(trades, vec![0; 18]), &[]);

        assert_eq!(metrics.total_pips, 0.3);
        assert_eq!(metrics.segments[2], 0.3);
    }

    #[test]
    fn drawdown_is_accumulated_in_integer_milli_so_three_tenth_pip_losses_total_exactly_0_3() {
        let trades = vec![trade_milli(-1, 0), trade_milli(-1, 1), trade_milli(-1, 2)];
        let metrics = evaluate(&bars(3), &outcome(trades, vec![0; 3]), &[]);

        assert_eq!(metrics.max_drawdown_pips, 0.3);
    }

    // ---- capture_rate --------------------------------------------------------------

    #[test]
    fn capture_rate_divides_total_pips_by_realizable_pips_when_positive() {
        let trades = vec![
            trade(30.0, 0, ExitReason::Signal),
            trade(10.0, 1, ExitReason::Signal),
        ];
        let bm = Benchmark {
            theta_pips: 20,
            legs: vec![],
            ideal_milli: 0,
            realizable_milli: 500, // 50 pips
            labels: vec![0; 2],
        };
        let metrics = evaluate(&bars(2), &outcome(trades, vec![0, 0]), &[bm]);

        let theta = &metrics.by_theta["20"];
        assert_eq!(theta.realizable_pips, 50.0);
        assert_eq!(theta.capture_rate, Some(40.0 / 50.0));
    }

    #[test]
    fn capture_rate_is_none_when_realizable_pips_is_zero_or_negative() {
        let n = 2;
        for realizable_milli in [0i64, -100] {
            let bm = Benchmark {
                theta_pips: 20,
                legs: vec![],
                ideal_milli: 0,
                realizable_milli,
                labels: vec![0; n],
            };
            let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);
            assert_eq!(
                metrics.by_theta["20"].capture_rate, None,
                "realizable_milli={realizable_milli} must yield capture_rate = None"
            );
        }
    }

    // ---- correct_side_ratio -----------------------------------------------------

    #[test]
    fn correct_side_ratio_counts_matches_among_labeled_bars_only() {
        // 10本中 labels が付くのは添字2..=7(6本、方向1)。positions は 4本が一致、2本が不一致。
        let n = 10;
        let mut labels = vec![0i8; n];
        for l in labels.iter_mut().take(8).skip(2) {
            *l = 1;
        }
        let mut positions = vec![0i8; n];
        positions[2] = 1;
        positions[3] = 1;
        positions[4] = 1;
        positions[5] = 1;
        positions[6] = -1; // 不一致
        positions[7] = 0; // 不一致

        let bm = Benchmark {
            theta_pips: 20,
            legs: vec![],
            ideal_milli: 0,
            realizable_milli: 0,
            labels,
        };
        let metrics = evaluate(&bars(n), &outcome(vec![], positions), &[bm]);

        assert_eq!(metrics.by_theta["20"].correct_side_ratio, Some(4.0 / 6.0));
    }

    #[test]
    fn correct_side_ratio_is_none_when_no_bar_is_labeled() {
        let n = 5;
        let metrics = evaluate(
            &bars(n),
            &outcome(vec![], vec![0; n]),
            &[empty_benchmark(20, n)],
        );
        assert_eq!(metrics.by_theta["20"].correct_side_ratio, None);
    }

    // ---- missed_legs -----------------------------------------------------------

    #[test]
    fn missed_legs_includes_only_legs_with_match_ratio_below_half_and_excludes_exactly_half() {
        // 3波、各波 4本(a+1..=b)。一致率 1/4(未満), 2/4(ちょうど半分), 3/4(以上)。
        let n = 13;
        let mut positions = vec![0i8; n];
        // leg1: a=0,b=4 (range 1..=4) matched=1
        positions[1] = 1;
        positions[2] = -1;
        positions[3] = 0;
        positions[4] = 0;
        // leg2: a=4,b=8 (range 5..=8) matched=2 (ちょうど半分)
        positions[5] = 1;
        positions[6] = 1;
        positions[7] = 0;
        positions[8] = -1;
        // leg3: a=8,b=12 (range 9..=12) matched=3
        positions[9] = 1;
        positions[10] = 1;
        positions[11] = 1;
        positions[12] = 0;

        let legs = vec![
            Leg {
                a: 0,
                b: 4,
                direction: 1,
                ideal_milli: 100,
                realizable_milli: 100,
            },
            Leg {
                a: 4,
                b: 8,
                direction: 1,
                ideal_milli: 100,
                realizable_milli: 100,
            },
            Leg {
                a: 8,
                b: 12,
                direction: 1,
                ideal_milli: 100,
                realizable_milli: 100,
            },
        ];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 300,
            realizable_milli: 300,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars(n), &outcome(vec![], positions), &[bm]);

        let missed = &metrics.by_theta["20"].missed_legs;
        assert_eq!(
            missed.len(),
            1,
            "only the 1/4 leg is below the 0.5 threshold"
        );
        assert_eq!(missed[0].flat_bars, 2);
        assert_eq!(missed[0].opposite_bars, 1);
    }

    #[test]
    fn missed_legs_report_start_time_at_a_plus_1_and_end_time_at_b() {
        let n = 6;
        let legs = vec![Leg {
            a: 1,
            b: 4,
            direction: 1,
            ideal_milli: 100,
            realizable_milli: 100,
        }];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 100,
            realizable_milli: 100,
            labels: vec![0; n],
        };
        // 全区間 flat (matched=0) なので必ず missed。
        let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

        let missed = &metrics.by_theta["20"].missed_legs;
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].start_time, "1970-01-01T00:10:00Z"); // bar[2].open_time = 2*300s
        assert_eq!(missed[0].end_time, "1970-01-01T00:20:00Z"); // bar[4].open_time = 4*300s
        assert_eq!(missed[0].direction, 1);
    }

    #[test]
    fn missed_legs_sorted_by_realizable_pips_descending_and_capped_at_20() {
        // 21 波、すべて一致率 0 (完全に missed)。realizable_milli は作成順に昇順(10,20,...,210)
        // なので、出力は降順で 210 件目(最大)が先頭、10(最小)が切り捨てられるはず。
        let leg_count = 21;
        let n = leg_count * 2 + 1;
        let legs: Vec<Leg> = (0..leg_count)
            .map(|i| Leg {
                a: i * 2,
                b: i * 2 + 2,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: (i as i64 + 1) * 10,
            })
            .collect();
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

        let missed = &metrics.by_theta["20"].missed_legs;
        assert_eq!(missed.len(), 20, "21 candidates must be capped at 20");
        assert_eq!(missed[0].realizable_pips, milli_to_pips(210));
        assert_eq!(
            missed[19].realizable_pips,
            milli_to_pips(20),
            "the smallest candidate (realizable_milli=10) must be dropped"
        );
    }

    #[test]
    fn missed_legs_tie_break_by_start_time_ascending() {
        let n = 6;
        let legs = vec![
            Leg {
                a: 2,
                b: 4,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: 100,
            },
            Leg {
                a: 0,
                b: 2,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: 100,
            },
        ];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

        let missed = &metrics.by_theta["20"].missed_legs;
        assert_eq!(missed.len(), 2);
        assert_eq!(
            missed[0].start_time,
            rfc3339(M5_SECS),
            "the earlier-starting leg (a=0) must come first despite equal realizable_pips"
        );
    }

    // ---- mean_lag_bars / mean_lag_pips -----------------------------------------

    #[test]
    fn mean_lag_averages_over_legs_with_at_least_one_matching_bar() {
        let n = 35;
        // mid2_close[i] = 300000 + i*20 (1本あたり 1 pip のランプ)。
        let bars_vec: Vec<Bar> = (0..n)
            .map(|i| bar_mid2_close(i, 300_000 + i as i64 * 20))
            .collect();

        let mut positions = vec![0i8; n];
        // leg_a: a=10,b=15 (range 11..=15)。最初の一致は添字13 -> lag_bars=2。
        positions[13] = 1;
        // leg_b: a=20,b=24 (range 21..=24)。a+1 で一致 -> lag_bars=0。
        positions[21] = 1;
        // leg_c: a=30,b=34 (range 31..=34)。一致なし。

        let legs = vec![
            Leg {
                a: 10,
                b: 15,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: 10,
            },
            Leg {
                a: 20,
                b: 24,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: 10,
            },
            Leg {
                a: 30,
                b: 34,
                direction: 1,
                ideal_milli: 0,
                realizable_milli: 10,
            },
        ];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars_vec, &outcome(vec![], positions), &[bm]);

        let theta = &metrics.by_theta["20"];
        assert_eq!(theta.mean_lag_bars, Some((2.0 + 0.0) / 2.0));
        // leg_a: |mid2_close[13]-mid2_close[10]| = |260-200| = 60 -> 3.0 pips
        // leg_b: |mid2_close[21]-mid2_close[20]| = 20 -> 1.0 pips
        assert_eq!(theta.mean_lag_pips, Some((3.0 + 1.0) / 2.0));
    }

    #[test]
    fn mean_lag_pips_is_an_absolute_value_for_a_falling_leg() {
        let n = 10;
        // mid2_close[i] = 300000 - i*20 (下降ランプ)。一致した足の mid2_close は足 a より低い。
        let bars_vec: Vec<Bar> = (0..n)
            .map(|i| bar_mid2_close(i, 300_000 - i as i64 * 20))
            .collect();

        let mut positions = vec![0i8; n];
        // leg: a=2,b=6 (range 3..=6), direction=-1。最初の一致は添字4 -> lag_bars=1。
        positions[4] = -1;

        let legs = vec![Leg {
            a: 2,
            b: 6,
            direction: -1,
            ideal_milli: 0,
            realizable_milli: 10,
        }];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        };
        let metrics = evaluate(&bars_vec, &outcome(vec![], positions), &[bm]);

        let theta = &metrics.by_theta["20"];
        assert_eq!(theta.mean_lag_bars, Some(1.0));
        // mid2_close[4]-mid2_close[2] = -40 (負)。絶対値で 40 -> 2.0 pips。
        assert_eq!(
            theta.mean_lag_pips,
            Some(2.0),
            "mean_lag_pips must use the absolute price difference, not the signed one"
        );
    }

    #[test]
    fn mean_lag_is_none_when_no_leg_has_a_matching_bar() {
        let n = 6;
        let legs = vec![Leg {
            a: 0,
            b: 4,
            direction: 1,
            ideal_milli: 0,
            realizable_milli: 10,
        }];
        let bm = Benchmark {
            theta_pips: 20,
            legs,
            ideal_milli: 0,
            realizable_milli: 0,
            labels: vec![0; n],
        };
        // すべて flat (direction と一致する足が 1 本もない)。
        let metrics = evaluate(&bars(n), &outcome(vec![], vec![0; n]), &[bm]);

        let theta = &metrics.by_theta["20"];
        assert_eq!(theta.mean_lag_bars, None);
        assert_eq!(theta.mean_lag_pips, None);
    }

    // ---- metrics_json ------------------------------------------------------------

    #[test]
    fn metrics_json_matches_spec_shape_with_null_for_none_and_theta_pips_string_keys() {
        let mut by_theta = BTreeMap::new();
        by_theta.insert(
            "20".to_string(),
            ThetaMetrics {
                leg_count: 0,
                ideal_pips: 0.0,
                realizable_pips: 0.0,
                capture_rate: None,
                correct_side_ratio: None,
                mean_lag_bars: None,
                mean_lag_pips: None,
                missed_legs: vec![MissedLeg {
                    start_time: "2024-01-02T03:05:00Z".to_string(),
                    end_time: "2024-01-02T07:40:00Z".to_string(),
                    direction: 1,
                    realizable_pips: 0.0,
                    flat_bars: 0,
                    opposite_bars: 0,
                }],
            },
        );
        let metrics = Metrics {
            total_pips: 0.0,
            trade_count: 0,
            win_rate: 0.0,
            max_drawdown_pips: 0.0,
            time_in_market: 0.0,
            protective_stop_count: 0,
            segments: [0.0; 6],
            by_theta,
        };

        let json = metrics.metrics_json();
        let expected = serde_json::json!({
            "segments": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "by_theta": {
                "20": {
                    "leg_count": 0,
                    "ideal_pips": 0.0,
                    "realizable_pips": 0.0,
                    "capture_rate": null,
                    "correct_side_ratio": null,
                    "mean_lag_bars": null,
                    "mean_lag_pips": null,
                    "missed_legs": [
                        {
                            "start_time": "2024-01-02T03:05:00Z",
                            "end_time": "2024-01-02T07:40:00Z",
                            "direction": 1,
                            "realizable_pips": 0.0,
                            "flat_bars": 0,
                            "opposite_bars": 0
                        }
                    ]
                }
            }
        });
        assert_eq!(json, expected);
    }

    // ---- 波が 0 件 -----------------------------------------------------------------

    #[test]
    fn zero_leg_benchmark_does_not_panic_and_yields_none_options() {
        let n = 5;
        let metrics = evaluate(
            &bars(n),
            &outcome(vec![], vec![0; n]),
            &[empty_benchmark(20, n)],
        );

        let theta = &metrics.by_theta["20"];
        assert_eq!(theta.leg_count, 0);
        assert_eq!(theta.ideal_pips, 0.0);
        assert_eq!(theta.realizable_pips, 0.0);
        assert_eq!(theta.capture_rate, None);
        assert_eq!(theta.correct_side_ratio, None);
        assert_eq!(theta.mean_lag_bars, None);
        assert_eq!(theta.mean_lag_pips, None);
        assert!(theta.missed_legs.is_empty());
    }
}
