use super::*;
use crate::types::M5_SECS;

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
    let bars = bars_from_mid_closes(&MAIN_EXAMPLE_MID_CLOSES, 4);
    let bm = compute(&bars, 20);

    assert_eq!(bm.legs.len(), 3);
    assert_eq!(bm.legs[0].ideal_milli, 300 - 4);
    assert_eq!(bm.legs[1].ideal_milli, 250 - 4);
    assert_eq!(bm.legs[2].ideal_milli, 350 - 4);
    assert_eq!(bm.ideal_milli, 900 - 4 * 3);

    assert_eq!(bm.legs[0].realizable_milli, 296);
    assert_eq!(bm.legs[1].realizable_milli, 246);
    assert_eq!(bm.legs[2].realizable_milli, 346);
    assert_eq!(bm.realizable_milli, 296 + 246 + 346);
}

#[test]
fn realizable_uses_the_open_prices_of_bars_a_plus_1_and_b_plus_1_not_the_closes() {
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

    assert_eq!(bm.legs[0].ideal_milli, 300);
    assert_eq!(bm.legs[1].ideal_milli, 250);
    assert_eq!(bm.legs[2].ideal_milli, 350);
    assert_eq!(bm.ideal_milli, 900);

    assert_eq!(bm.legs[0].realizable_milli, 284);
    assert_eq!(bm.legs[1].realizable_milli, 234);
    assert_eq!(bm.legs[2].realizable_milli, 334);
    assert_eq!(bm.realizable_milli, 284 + 234 + 334);
}

#[test]
fn reversal_of_exactly_theta_confirms_but_one_mid2_unit_short_does_not() {
    let theta_pips = 20;

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
    let bars = vec![
        bar_mid2_close(0, 300_000),
        bar_mid2_close(1, 300_500),
        bar_mid2_close(2, 300_500),
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
    let bm = compute(&bars, 20);

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
    let bars: Vec<Bar> = (0..6)
        .map(|i| bar_mid2_close(i, 300_000 + i * 100))
        .collect();
    let turns = vec![
        Turn {
            pos: 1,
            kind: TurnKind::Low,
        },
        Turn {
            pos: 5,
            kind: TurnKind::High,
        },
    ];
    let (legs, labels) = build_legs(&bars, &turns);
    assert!(legs.is_empty());
    assert_eq!(labels, vec![0; 6]);
}
