use std::collections::BTreeMap;

use crate::benchmark::{Benchmark, Leg};
use crate::engine::{ExitReason, SimOutcome, SimTrade};
use crate::types::{Bar, mid2_to_pips, milli_to_pips};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MissedLeg {
    pub start_time: String,
    pub end_time: String,
    pub direction: i8,
    pub realizable_pips: f64,
    pub flat_bars: u32,
    pub opposite_bars: u32,
}

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
    pub fn metrics_json(&self) -> serde_json::Value {
        serde_json::json!({
            "segments": self.segments,
            "by_theta": self.by_theta,
        })
    }
}

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

fn max_drawdown(trades: &[SimTrade]) -> f64 {
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

fn compute_segments(n: usize, trades: &[SimTrade]) -> [f64; 6] {
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

fn segment_index(exit_idx: usize, boundaries: &[usize; 7]) -> usize {
    for (i, window) in boundaries.windows(2).enumerate() {
        if exit_idx < window[1] {
            return i;
        }
    }
    5
}

struct LegEval<'a> {
    leg: &'a Leg,
    matched: usize,
    total: usize,
    flat_bars: u32,
    opposite_bars: u32,
    first_match_idx: Option<usize>,
}

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

fn mean_lag(eval_bars: &[Bar], leg_evals: &[LegEval]) -> (Option<f64>, Option<f64>) {
    let lagged: Vec<(&LegEval, usize)> = leg_evals
        .iter()
        .filter_map(|e| e.first_match_idx.map(|idx| (e, idx)))
        .collect();
    if lagged.is_empty() {
        return (None, None);
    }

    let bars_sum: usize = lagged.iter().map(|(e, idx)| idx - (e.leg.a + 1)).sum();
    let pips_sum: f64 = lagged
        .iter()
        .map(|(e, idx)| {
            let diff = eval_bars[*idx].mid2_close() - eval_bars[e.leg.a].mid2_close();
            mid2_to_pips(diff.abs())
        })
        .sum();

    let count = lagged.len() as f64;
    (Some(bars_sum as f64 / count), Some(pips_sum / count))
}

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

fn rfc3339(open_time: i64) -> String {
    match crate::data::format_epoch_secs_rfc3339(open_time) {
        Some(s) => s,
        None => unreachable!(
            "eval_bars[].open_time={open_time} is not a representable UTC instant, but \
             data::upsert_bars validates every open_time via epoch_secs_to_datetime before it \
             is written to sim_candles, so a bar loaded through Dataset cannot carry one"
        ),
    }
}

#[cfg(test)]
mod tests;
