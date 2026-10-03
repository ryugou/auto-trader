//! 折り返しの抽出、波、理論値、実質上限（spec 7 章）。
//!
//! 入力は評価期間の `mid2_close`（`Bar::mid2_close()`）だけで、ジグザグ式に方向未定 →
//! 上昇 → 下降 → ... と確定点を積み上げていく（spec 7.1 の 1〜4）。判定・加減算はすべて
//! `i64`（`mid2` 単位、または `bid_close`/`ask_close`/`bid_open`/`ask_open` のミリ円）で行う
//! （spec 3 章、計画 Global Constraints）。
//!
//! 確定点はこのアルゴリズムの性質上、常に「それを確定させた足より前」の位置にしかならない
//! （確定には必ず後続の足の処理が要る）。したがって `extract_turns` が返す確定点 `b` について
//! `b + 1` は常に評価期間内に存在する。それでも spec 7.1 は「足 `b+1` が評価期間内に存在しない
//! 波は扱わない」という契約を明示しているため、`build_legs` はこの不変条件を `extract_turns` の
//! 挙動に依存せず自前でガードする（`extract_turns` の将来の変更に対しても安全であるため）。

use crate::types::{Bar, PIP_MID2};

/// 連続する 2 つの確定した折り返し点の間の波（spec 7.1 章）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leg {
    /// 始点（評価期間内の添字）。
    pub a: usize,
    /// 終点。
    pub b: usize,
    /// 上昇 = 1、下降 = -1。
    pub direction: i8,
    /// 理論値（ミリ円、spec 7.2 章）。
    pub ideal_milli: i64,
    /// 実質上限（ミリ円、spec 7.2 章）。
    pub realizable_milli: i64,
}

/// 折り返し幅 `theta_pips` に対する基準値一式（spec 7 章）。
#[derive(Debug, Clone, PartialEq)]
pub struct Benchmark {
    pub theta_pips: i64,
    /// spec 7.1 で波として扱うものだけ。
    pub legs: Vec<Leg>,
    /// 全波の理論値の合計（ミリ円）。
    pub ideal_milli: i64,
    /// 全波の実質上限の合計（ミリ円）。
    pub realizable_milli: i64,
    /// 評価期間の各足の方向ラベル（上昇 = 1、下降 = -1）。ラベルなしは 0。
    pub labels: Vec<i8>,
}

/// 確定した折り返し点の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnKind {
    Low,
    High,
}

/// 確定した折り返し点（位置と種類）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Turn {
    pos: usize,
    kind: TurnKind,
}

/// 方向未定・上昇中・下降中のいずれか（spec 7.1 の 1〜3）。
enum Direction {
    Up,
    Down,
}

/// spec 7.1 の 1〜4 のとおりに `mid2_close` の折り返しを抽出する。
///
/// - 極値の更新は厳密な不等号で行う（同値の極値が 2 回現れた場合、先に現れた位置を保持する:
///   spec 7.1 の 4）。
/// - 確定は `>= theta` で行う（ちょうど `theta` で確定し、`theta` 未満では確定しない）。
fn extract_turns(eval_bars: &[Bar], theta: i64) -> Vec<Turn> {
    let n = eval_bars.len();
    let mut turns = Vec::new();
    if n == 0 {
        return turns;
    }
    let mid2 = |t: usize| eval_bars[t].mid2_close();

    let mut direction: Option<Direction> = None;
    // 方向未定の間の、先頭からの最高値・最安値とその位置。
    let mut hi_val = mid2(0);
    let mut hi_pos = 0usize;
    let mut lo_val = mid2(0);
    let mut lo_pos = 0usize;
    // 方向確定後の暫定の極値とその位置。方向未定の間は未使用。
    let mut extreme_val = 0i64;
    let mut extreme_pos = 0usize;

    for t in 1..n {
        let v = mid2(t);
        match direction {
            None => {
                if v > hi_val {
                    hi_val = v;
                    hi_pos = t;
                }
                if v < lo_val {
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
                if v > extreme_val {
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
                if v < extreme_val {
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

/// 確定した折り返し点の列から波を作り、理論値・実質上限・方向ラベルを計算する（spec 7.1 の
/// 波の定義、除外規則、7.2 の基準値の式）。
///
/// `turns` の隣接するすべてのペアを波の候補とし、足 `b+1` が `eval_bars` に存在しないものだけを
/// 除外する（「最後に確定した折り返し点より後の区間」は、そもそも `turns` に 2 つ目の点が
/// 現れないため候補にならず、ここでの判定は不要）。
fn build_legs(eval_bars: &[Bar], turns: &[Turn]) -> (Vec<Leg>, Vec<i8>) {
    let n = eval_bars.len();
    let mut legs = Vec::new();
    let mut labels = vec![0i8; n];

    for pair in turns.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let a = start.pos;
        let b = end.pos;
        if b + 1 >= n {
            // spec 7.1: 足 b+1 が評価期間内に存在しない波は扱わない。
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

/// 評価期間の足と折り返し幅（pips）から基準値一式を計算する（spec 7 章）。
///
/// `eval_bars` が空、または 1 本だけでも panic しない（折り返しが 1 つも確定しないため
/// `legs` が空になるだけである）。
///
/// `theta_pips` は 1 以上であること（呼び出し元は `SimConfig::validate` 済みの
/// `thetas_pips` を渡す）。0 以下は想定しない。
pub fn compute(eval_bars: &[Bar], theta_pips: i64) -> Benchmark {
    // 設定値に上限検証が無いため飽和乗算にする。飽和した theta ではどの値動きも折り返しに
    // ならず legs が空になり、意味的にも正しい。
    let theta = theta_pips.saturating_mul(PIP_MID2);
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
mod tests {
    use super::*;
    use crate::types::M5_SECS;

    /// spec 7 章のテスト用 `Bar` ヘルパー。中値の終値（円）の列とスプレッド（ミリ円、偶数）
    /// から `Vec<Bar>` を作る。始値は直前の足の終値と同じにする（計画 Task 4 Step 1 の指示）。
    /// 高値・安値は `compute` が参照しないため、始値・終値のうち大きい方・小さい方を入れる。
    fn bars_from_mid_closes(mid_closes: &[f64], spread_milli: i64) -> Vec<Bar> {
        assert_eq!(spread_milli % 2, 0, "spread_milli must split evenly");
        let half = spread_milli / 2;
        let close_milli: Vec<i64> = mid_closes
            .iter()
            .map(|&v| (v * 1000.0).round() as i64)
            .collect();
        (0..close_milli.len())
            .map(|i| {
                let close_mid = close_milli[i];
                let open_mid = if i == 0 {
                    close_milli[0]
                } else {
                    close_milli[i - 1]
                };
                let bid_open = open_mid - half;
                let ask_open = open_mid + half;
                let bid_close = close_mid - half;
                let ask_close = close_mid + half;
                Bar {
                    open_time: i as i64 * M5_SECS,
                    bid_open,
                    bid_high: bid_open.max(bid_close),
                    bid_low: bid_open.min(bid_close),
                    bid_close,
                    ask_open,
                    ask_high: ask_open.max(ask_close),
                    ask_low: ask_open.min(ask_close),
                    ask_close,
                }
            })
            .collect()
    }

    /// `mid2_close` を直接指定するテスト用 `Bar`（折り返し判定の境界値テスト専用）。
    /// 円の丸めを経由しないので、`mid2` の 1 単位まで厳密に制御できる。始値・高値・安値は
    /// `compute` が参照しないため終値と同じにする。
    fn bar_mid2_close(idx: i64, mid2_close: i64) -> Bar {
        let bid = mid2_close / 2;
        let ask = mid2_close - bid;
        Bar {
            open_time: idx * M5_SECS,
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

    const MAIN_EXAMPLE_MID_CLOSES: [f64; 8] = [
        150.00, 150.10, 150.30, 150.20, 150.05, 150.15, 150.40, 150.10,
    ];

    #[test]
    fn huge_theta_pips_saturates_without_overflow_and_yields_no_legs() {
        let bars = bars_from_mid_closes(&MAIN_EXAMPLE_MID_CLOSES, 0);
        let bm = compute(&bars, i64::MAX);

        assert!(bm.legs.is_empty());
        assert_eq!(bm.ideal_milli, 0);
        assert_eq!(bm.realizable_milli, 0);
        assert_eq!(bm.theta_pips, i64::MAX);
    }

    #[test]
    fn main_example_extracts_three_legs_with_expected_ideal_realizable_and_labels() {
        let bars = bars_from_mid_closes(&MAIN_EXAMPLE_MID_CLOSES, 0);
        let bm = compute(&bars, 20);

        assert_eq!(bm.legs.len(), 3);
        assert_eq!(
            bm.legs[0],
            Leg {
                a: 0,
                b: 2,
                direction: 1,
                ideal_milli: 300,
                realizable_milli: 300,
            }
        );
        assert_eq!(
            bm.legs[1],
            Leg {
                a: 2,
                b: 4,
                direction: -1,
                ideal_milli: 250,
                realizable_milli: 250,
            }
        );
        assert_eq!(
            bm.legs[2],
            Leg {
                a: 4,
                b: 6,
                direction: 1,
                ideal_milli: 350,
                realizable_milli: 350,
            }
        );
        assert_eq!(bm.ideal_milli, 900);
        assert_eq!(bm.realizable_milli, 900);
        assert_eq!(bm.labels, vec![0, 1, 1, -1, -1, 1, 1, 0]);
    }

    #[test]
    fn spread_reduces_ideal_by_the_spread_per_leg_and_realizable_matches_the_open_formula() {
        // スプレッドを 4 ミリ円（往復で 4 ミリ円 = entry と exit それぞれ半分の 2 ミリ円ずつ）
        // にすると、理論値は波 1 つにつき 4 ミリ円ずつ減る（計画 Task 4 Step 1）。
        let bars = bars_from_mid_closes(&MAIN_EXAMPLE_MID_CLOSES, 4);
        let bm = compute(&bars, 20);

        assert_eq!(bm.legs.len(), 3);
        assert_eq!(bm.legs[0].ideal_milli, 300 - 4);
        assert_eq!(bm.legs[1].ideal_milli, 250 - 4);
        assert_eq!(bm.legs[2].ideal_milli, 350 - 4);
        assert_eq!(bm.ideal_milli, 900 - 4 * 3);

        // 実質上限は spec 7.2 の式どおり、足 a+1 と b+1 の始値（買値・売値）から計算される。
        // このテストのヘルパーは「始値は直前の足の終値と同じにする」ため、bid_open[t] ==
        // bid_close[t-1] かつ ask_open[t] == ask_close[t-1] が常に成り立ち、結果として
        // realizable_milli は ideal_milli と代数的に一致する（ヘルパーの構造によるものであり、
        // 実装のバグでそう見えているわけではない）。以下は 7.2 の式をそのまま手計算した値である:
        // leg0 (a=0,b=2,up): bid_open[3] - ask_open[1] = 150300 - 2 - (150000 + 2) = 296
        // leg1 (a=2,b=4,down): bid_open[3] - ask_open[5] = 150300 - 2 - (150050 + 2) = 246
        // leg2 (a=4,b=6,up): bid_open[7] - ask_open[5] = 150400 - 2 - (150050 + 2) = 346
        assert_eq!(bm.legs[0].realizable_milli, 296);
        assert_eq!(bm.legs[1].realizable_milli, 246);
        assert_eq!(bm.legs[2].realizable_milli, 346);
        assert_eq!(bm.realizable_milli, 296 + 246 + 346);
    }

    #[test]
    fn realizable_uses_the_open_prices_of_bars_a_plus_1_and_b_plus_1_not_the_closes() {
        // 始値を終値と異なる値にして、実装が close[a]/close[b] を誤って使えば検出できるようにする。
        // 足 1・3・5・7 は各波の a+1 / b+1 に当たる。bid_open は -7、ask_open は +9 ずらす
        // (上昇・下降で非対称になる)。折り返し判定は mid2_close のみなので legs の a/b は変わらない。
        let mut bars = bars_from_mid_closes(&MAIN_EXAMPLE_MID_CLOSES, 0);
        for i in [1usize, 3, 5, 7] {
            bars[i].bid_open -= 7;
            bars[i].ask_open += 9;
        }
        let bm = compute(&bars, 20);

        assert_eq!(bm.legs.len(), 3);
        assert_eq!((bm.legs[0].a, bm.legs[0].b), (0, 2));
        assert_eq!((bm.legs[1].a, bm.legs[1].b), (2, 4));
        assert_eq!((bm.legs[2].a, bm.legs[2].b), (4, 6));

        // 理論値は終値のみ参照するので変化しない。
        assert_eq!(bm.legs[0].ideal_milli, 300);
        assert_eq!(bm.legs[1].ideal_milli, 250);
        assert_eq!(bm.legs[2].ideal_milli, 350);
        assert_eq!(bm.ideal_milli, 900);

        // spec 7.2:
        // leg0 (up, a=0,b=2): bid_open[3] - ask_open[1] = (150300-7) - (150000+9) = 284
        // leg1 (down, a=2,b=4): bid_open[3] - ask_open[5] = (150300-7) - (150050+9) = 234
        // leg2 (up, a=4,b=6): bid_open[7] - ask_open[5] = (150400-7) - (150050+9) = 334
        assert_eq!(bm.legs[0].realizable_milli, 284);
        assert_eq!(bm.legs[1].realizable_milli, 234);
        assert_eq!(bm.legs[2].realizable_milli, 334);
        assert_eq!(bm.realizable_milli, 284 + 234 + 334);
    }

    #[test]
    fn reversal_of_exactly_theta_confirms_but_one_mid2_unit_short_does_not() {
        // theta_pips = 20 → theta = 400 (mid2 単位)。400 の逆行で確定し、399（19.95 pips）
        // では確定しない。
        let theta_pips = 20;

        // 399 (19.95 pips) の逆行: 0 → +500 で方向確定(上昇、確定点は添字0の安値)、
        // その後 399 戻しても 2 つ目の折り返し点は確定しない。
        let bars_399 = vec![
            bar_mid2_close(0, 300_000),
            bar_mid2_close(1, 300_500),
            bar_mid2_close(2, 300_500 - 399),
        ];
        let bm_399 = compute(&bars_399, theta_pips);
        assert!(
            bm_399.legs.is_empty(),
            "399 mid2 units (19.95 pips) must not confirm a second turning point"
        );

        // 同じ系列に、ちょうど 400 の逆行を 1 本追加すると確定する。
        let mut bars_400 = bars_399.clone();
        bars_400.push(bar_mid2_close(3, 300_500 - 400));
        let bm_400 = compute(&bars_400, theta_pips);
        assert_eq!(bm_400.legs.len(), 1);
        assert_eq!(bm_400.legs[0].a, 0);
        assert_eq!(bm_400.legs[0].b, 1);
        assert_eq!(bm_400.legs[0].direction, 1);
    }

    #[test]
    fn a_tied_extreme_keeps_the_earlier_position() {
        // 0 → +500 で方向確定(上昇、確定点は添字0)、暫定の極値は添字1(300500)。
        // 添字2で同値(300500)が再び現れても、極値の位置は添字1のまま更新されない。
        // その後ちょうど theta 戻すと、添字1(先に現れた位置)が高値として確定するはず。
        let bars = vec![
            bar_mid2_close(0, 300_000),
            bar_mid2_close(1, 300_500),
            bar_mid2_close(2, 300_500), // 同値の極値(2回目)
            bar_mid2_close(3, 300_500 - 400),
        ];
        let bm = compute(&bars, 20);
        assert_eq!(bm.legs.len(), 1);
        assert_eq!(
            bm.legs[0].b, 1,
            "the tied extreme must keep the earlier position (1), not the later one (2)"
        );
    }

    #[test]
    fn legs_are_empty_when_price_never_moves_by_theta() {
        let mid_closes = [150.00, 150.05, 150.03, 150.08, 150.02, 150.06];
        let bars = bars_from_mid_closes(&mid_closes, 0);
        let bm = compute(&bars, 20); // 最大の振れ幅は 8 pips で theta(20 pips) 未満

        assert!(bm.legs.is_empty());
        assert_eq!(bm.ideal_milli, 0);
        assert_eq!(bm.realizable_milli, 0);
        assert_eq!(bm.labels, vec![0; mid_closes.len()]);
    }

    #[test]
    fn empty_input_does_not_panic() {
        let bm = compute(&[], 20);
        assert!(bm.legs.is_empty());
        assert_eq!(bm.ideal_milli, 0);
        assert_eq!(bm.realizable_milli, 0);
        assert!(bm.labels.is_empty());
    }

    #[test]
    fn single_bar_input_does_not_panic() {
        let bars = vec![bar_mid2_close(0, 300_000)];
        let bm = compute(&bars, 20);
        assert!(bm.legs.is_empty());
        assert_eq!(bm.labels, vec![0]);
    }

    #[test]
    fn build_legs_excludes_a_candidate_wave_whose_b_plus_1_bar_does_not_exist() {
        // extract_turns の確定ロジック自体は、確定点が常に「それを確定させた足より前」に
        // あるという性質上、b+1 が存在しない turns を作らない(確定には必ず後続の足の処理が
        // 要るため、最短でも b+1 の足で確定し、その足自体が存在する)。それでも spec 7.1 は
        // 「足 b+1 が評価期間内に存在しない波は扱わない」契約を明示しているため、build_legs が
        // この不変条件を turns の入力内容によらず守ることを、turns を直接組み立てて確認する。
        let bars: Vec<Bar> = (0..6)
            .map(|i| bar_mid2_close(i, 300_000 + i * 100))
            .collect();
        let turns = vec![
            Turn {
                pos: 1,
                kind: TurnKind::Low,
            },
            Turn {
                pos: 5, // bars.len() - 1 なので b+1 = 6 は存在しない
                kind: TurnKind::High,
            },
        ];
        let (legs, labels) = build_legs(&bars, &turns);
        assert!(legs.is_empty());
        assert_eq!(labels, vec![0; 6]);
    }
}
