use crate::types::{Bar, PIP_MID2};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    pub a: usize,
    pub b: usize,
    pub direction: i8,
    pub ideal_milli: i64,
    pub realizable_milli: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Benchmark {
    pub theta_pips: i64,
    pub legs: Vec<Leg>,
    pub ideal_milli: i64,
    pub realizable_milli: i64,
    pub labels: Vec<i8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnKind {
    Low,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Turn {
    pos: usize,
    kind: TurnKind,
}

enum Direction {
    Up,
    Down,
}

fn strictly_exceeds_so_ties_keep_earlier_position(
    candidate: i64,
    recorded_extreme: i64,
    direction_is_up: bool,
) -> bool {
    if direction_is_up {
        candidate > recorded_extreme
    } else {
        candidate < recorded_extreme
    }
}

fn extract_turns(eval_bars: &[Bar], theta: i64) -> Vec<Turn> {
    let n = eval_bars.len();
    let mut turns = Vec::new();
    if n == 0 {
        return turns;
    }
    let mid2 = |t: usize| eval_bars[t].mid2_close();

    let mut direction: Option<Direction> = None;
    let mut hi_val = mid2(0);
    let mut hi_pos = 0usize;
    let mut lo_val = mid2(0);
    let mut lo_pos = 0usize;
    let mut extreme_val = 0i64;
    let mut extreme_pos = 0usize;

    for t in 1..n {
        let v = mid2(t);
        match direction {
            None => {
                if strictly_exceeds_so_ties_keep_earlier_position(v, hi_val, true) {
                    hi_val = v;
                    hi_pos = t;
                }
                if strictly_exceeds_so_ties_keep_earlier_position(v, lo_val, false) {
                    lo_val = v;
                    lo_pos = t;
                }
                if v - lo_val >= theta {
                    turns.push(Turn {
                        pos: lo_pos,
                        kind: TurnKind::Low,
                    });
                    direction = Some(Direction::Up);
                    extreme_val = v;
                    extreme_pos = t;
                } else if hi_val - v >= theta {
                    turns.push(Turn {
                        pos: hi_pos,
                        kind: TurnKind::High,
                    });
                    direction = Some(Direction::Down);
                    extreme_val = v;
                    extreme_pos = t;
                }
            }
            Some(Direction::Up) => {
                if strictly_exceeds_so_ties_keep_earlier_position(v, extreme_val, true) {
                    extreme_val = v;
                    extreme_pos = t;
                } else if extreme_val - v >= theta {
                    turns.push(Turn {
                        pos: extreme_pos,
                        kind: TurnKind::High,
                    });
                    direction = Some(Direction::Down);
                    extreme_val = v;
                    extreme_pos = t;
                }
            }
            Some(Direction::Down) => {
                if strictly_exceeds_so_ties_keep_earlier_position(v, extreme_val, false) {
                    extreme_val = v;
                    extreme_pos = t;
                } else if v - extreme_val >= theta {
                    turns.push(Turn {
                        pos: extreme_pos,
                        kind: TurnKind::Low,
                    });
                    direction = Some(Direction::Up);
                    extreme_val = v;
                    extreme_pos = t;
                }
            }
        }
    }
    turns
}

fn excludes_a_candidate_wave_whose_b_plus_1_bar_does_not_exist(b: usize, n: usize) -> bool {
    b + 1 >= n
}

fn build_legs(eval_bars: &[Bar], turns: &[Turn]) -> (Vec<Leg>, Vec<i8>) {
    let n = eval_bars.len();
    let mut legs = Vec::new();
    let mut labels = vec![0i8; n];

    for pair in turns.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let a = start.pos;
        let b = end.pos;
        if excludes_a_candidate_wave_whose_b_plus_1_bar_does_not_exist(b, n) {
            continue;
        }
        let direction: i8 = match start.kind {
            TurnKind::Low => 1,
            TurnKind::High => -1,
        };
        let (ideal_milli, realizable_milli) = if direction == 1 {
            (
                eval_bars[b].bid_close - eval_bars[a].ask_close,
                eval_bars[b + 1].bid_open - eval_bars[a + 1].ask_open,
            )
        } else {
            (
                eval_bars[a].bid_close - eval_bars[b].ask_close,
                eval_bars[a + 1].bid_open - eval_bars[b + 1].ask_open,
            )
        };
        legs.push(Leg {
            a,
            b,
            direction,
            ideal_milli,
            realizable_milli,
        });
        for label in labels.iter_mut().take(b + 1).skip(a + 1) {
            *label = direction;
        }
    }
    (legs, labels)
}

fn convert_theta_pips_to_mid2_saturating_so_an_overflowing_theta_yields_no_legs(
    theta_pips: i64,
) -> i64 {
    theta_pips.saturating_mul(PIP_MID2)
}

pub fn compute(eval_bars: &[Bar], theta_pips: i64) -> Benchmark {
    let theta =
        convert_theta_pips_to_mid2_saturating_so_an_overflowing_theta_yields_no_legs(theta_pips);
    let turns = extract_turns(eval_bars, theta);
    let (legs, labels) = build_legs(eval_bars, &turns);
    let ideal_milli = legs.iter().map(|l| l.ideal_milli).sum();
    let realizable_milli = legs.iter().map(|l| l.realizable_milli).sum();
    Benchmark {
        theta_pips,
        legs,
        ideal_milli,
        realizable_milli,
        labels,
    }
}

#[cfg(test)]
mod tests;
