use std::collections::VecDeque;

use super::*;
use crate::types::{Bar, M5_SECS};

fn flat(open_time: i64, bid: i64, ask: i64) -> Bar {
    Bar {
        open_time,
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

#[allow(clippy::too_many_arguments)]
fn bar_ohlc(
    open_time: i64,
    bid_open: i64,
    bid_high: i64,
    bid_low: i64,
    bid_close: i64,
    ask_open: i64,
    ask_high: i64,
    ask_low: i64,
    ask_close: i64,
) -> Bar {
    Bar {
        open_time,
        bid_open,
        bid_high,
        bid_low,
        bid_close,
        ask_open,
        ask_high,
        ask_low,
        ask_close,
    }
}

fn dataset_with_warmup(eval_bars: Vec<Bar>) -> Dataset {
    let warmup = flat(eval_bars[0].open_time - M5_SECS, 149_000, 149_010);
    let from = eval_bars[0].open_time;
    let to = eval_bars.last().unwrap().open_time + M5_SECS;
    let mut all = vec![warmup];
    all.extend(eval_bars);
    Dataset::new(all, from, to, 1, 64).expect("test dataset must build")
}

struct ScriptedDecider {
    responses: VecDeque<Result<i8, DeciderError>>,
    calls: Vec<BarState>,
}

impl ScriptedDecider {
    fn new(responses: Vec<Result<i8, DeciderError>>) -> Self {
        Self {
            responses: responses.into(),
            calls: Vec::new(),
        }
    }
}

impl Decider for ScriptedDecider {
    fn on_bar(&mut self, state: BarState) -> Result<i8, DeciderError> {
        self.calls.push(state);
        self.responses.pop_front().unwrap_or_else(|| {
            panic!(
                "ScriptedDecider ran out of scripted responses at call #{}",
                self.calls.len()
            )
        })
    }
}

fn signals(values: &[i8]) -> Vec<Result<i8, DeciderError>> {
    values.iter().map(|&v| Ok(v)).collect()
}

fn simulate_ok(dataset: &Dataset, decider: &mut dyn Decider, cfg: &SimConfig) -> SimOutcome {
    simulate(dataset, decider, cfg).expect("decider must not be Fatal in this test")
}

#[test]
fn out_of_range_on_bar_return_aborts_with_script_error_at_that_bar() {
    for bad in [2i8, -2] {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_220),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[0, bad, 0]));

        let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

        match &outcome.status {
            RunStatus::ScriptError { open_time, message } => {
                assert_eq!(*open_time, M5_SECS, "error must be at the offending bar");
                assert!(message.contains(&bad.to_string()), "message: {message}");
            }
            other => panic!("bad={bad}: expected ScriptError, got {other:?}"),
        }
        assert!(outcome.trades.is_empty(), "bad={bad}");
        assert_eq!(
            outcome.positions.len(),
            2,
            "bad={bad}: stops at the bad bar"
        );
    }
}

#[test]
fn out_of_range_return_on_the_last_bar_is_also_a_script_error() {
    let eval = vec![flat(0, 150_000, 150_010), flat(M5_SECS, 150_100, 150_120)];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[0, 2]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert!(matches!(
        outcome.status,
        RunStatus::ScriptError { open_time, .. } if open_time == M5_SECS
    ));
}

#[test]
fn no_execution_on_first_bar_and_long_opens_at_second_bar_ask_open() {
    let eval = vec![flat(0, 150_000, 150_010), flat(M5_SECS, 150_100, 150_120)];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.status, RunStatus::Ok);
    assert_eq!(
        outcome.positions[0], 0,
        "no execution must happen on the first bar of the evaluation period"
    );
    assert_eq!(outcome.trades.len(), 1);
    assert_eq!(outcome.trades[0].entry_idx, 1);
    assert_eq!(
        outcome.trades[0].entry_milli, 150_120,
        "a long entry must use the ask_open of the bar where it executes"
    );
}

#[test]
fn long_round_trip_uses_ask_open_entry_and_bid_open_exit() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.entry_milli, 150_120, "long entry uses ask_open");
    assert_eq!(trade.exit_milli, 150_200, "long exit uses bid_open");
    assert_eq!(trade.reason, ExitReason::Signal);
    assert_eq!(trade.pnl_milli, 80);
}

#[test]
fn short_round_trip_uses_bid_open_entry_and_ask_open_exit() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[-1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.entry_milli, 150_100, "short entry uses bid_open");
    assert_eq!(trade.exit_milli, 150_220, "short exit uses ask_open");
    assert_eq!(trade.reason, ExitReason::Signal);
    assert_eq!(trade.pnl_milli, -120);
}

#[test]
fn doten_closes_and_reopens_at_the_same_bar_open_producing_two_trades() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_210),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, -1, -1]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 2);
    let close_long = outcome.trades[0];
    assert_eq!(close_long.reason, ExitReason::Signal);
    assert_eq!(close_long.entry_idx, 1);
    assert_eq!(close_long.exit_idx, 2);
    assert_eq!(close_long.entry_milli, 150_120, "long entry: ask_open[1]");
    assert_eq!(close_long.exit_milli, 150_200, "doten close: bid_open[2]");

    let close_short = outcome.trades[1];
    assert_eq!(close_short.reason, ExitReason::EndOfData);
    assert_eq!(close_short.entry_idx, 2);
    assert_eq!(
        close_short.entry_milli, 150_200,
        "doten reopen (short) uses the same bid_open[2] as the close"
    );
    assert_eq!(
        close_short.exit_milli, 150_210,
        "residual short closes at ask_close of the last bar"
    );

    assert_eq!(
        outcome.positions[2], -1,
        "positions records the post-doten short, independent of the later EndOfData close"
    );
}

#[test]
fn bar_state_reflects_execution_and_stop_processing_with_bars_held_lifecycle() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_050, 150_060),
        flat(2 * M5_SECS, 150_100, 150_110),
        flat(3 * M5_SECS, 150_150, 150_160),
        flat(4 * M5_SECS, 150_200, 150_210),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, -1, -1, -1]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());
    assert_eq!(outcome.status, RunStatus::Ok);

    let calls = &decider.calls;
    assert_eq!(calls.len(), 5, "on_bar is called once per evaluation bar");
    assert_eq!(
        calls[0],
        BarState {
            t: 1,
            position: 0,
            entry_price_milli: 0,
            bars_held: 0,
        }
    );
    assert_eq!(
        calls[1],
        BarState {
            t: 2,
            position: 1,
            entry_price_milli: 150_060,
            bars_held: 0,
        },
        "the bar a position was opened on must report bars_held == 0"
    );
    assert_eq!(
        calls[2],
        BarState {
            t: 3,
            position: 1,
            entry_price_milli: 150_060,
            bars_held: 1,
        },
        "the following bar must report bars_held == 1"
    );
    assert_eq!(
        calls[3],
        BarState {
            t: 4,
            position: -1,
            entry_price_milli: 150_150,
            bars_held: 0,
        },
        "bars_held must reset to 0 right after a doten"
    );
}

#[test]
fn protective_stop_closes_long_at_stop_price_when_crossed_intrabar() {
    let long_entry_ask_open = 150_120;
    let bid_open_above_stop_price_no_gap = 149_300;
    let bid_low_breaches_stop_price = 149_000;
    let stop_price_without_gap = 149_120;
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_110, long_entry_ask_open),
        bar_ohlc(
            2 * M5_SECS,
            bid_open_above_stop_price_no_gap,
            149_350,
            bid_low_breaches_stop_price,
            149_050,
            149_310,
            149_360,
            149_010,
            149_060,
        ),
        flat(3 * M5_SECS, 149_050, 149_060),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(trade.exit_idx, 2);
    assert_eq!(
        trade.exit_milli, stop_price_without_gap,
        "stop price itself is used when bid_open has not already gapped through it"
    );
    assert_eq!(outcome.positions[2], 0);
}

#[test]
fn protective_stop_closes_long_at_bid_open_when_gapped_through() {
    let long_entry_ask_open = 150_120;
    let bid_open_already_gapped_below_stop_price = 148_900;
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_110, long_entry_ask_open),
        bar_ohlc(
            2 * M5_SECS,
            bid_open_already_gapped_below_stop_price,
            148_950,
            148_800,
            148_820,
            148_910,
            148_960,
            148_810,
            148_830,
        ),
        flat(3 * M5_SECS, 148_820, 148_830),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(
        trade.exit_milli, bid_open_already_gapped_below_stop_price,
        "bid_open is used when it has already gapped through the stop price"
    );
}

#[test]
fn protective_stop_closes_short_at_stop_price_when_crossed_intrabar() {
    let short_entry_bid_open = 150_100;
    let ask_high_breaches_stop_price = 151_310;
    let stop_price_without_gap = 151_100;
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, short_entry_bid_open, 150_110),
        bar_ohlc(
            2 * M5_SECS,
            150_900,
            151_300,
            150_880,
            150_950,
            150_890,
            ask_high_breaches_stop_price,
            150_890,
            150_960,
        ),
        flat(3 * M5_SECS, 150_950, 150_960),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(
        trade.exit_milli, stop_price_without_gap,
        "stop price itself is used when ask_open has not already gapped through it"
    );
    assert_eq!(outcome.positions[2], 0);
}

#[test]
fn protective_stop_closes_short_at_ask_open_when_gapped_through() {
    let short_entry_bid_open = 150_100;
    let ask_open_already_gapped_above_stop_price = 151_300;
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, short_entry_bid_open, 150_110),
        bar_ohlc(
            2 * M5_SECS,
            151_280,
            151_350,
            151_270,
            151_300,
            ask_open_already_gapped_above_stop_price,
            151_360,
            151_280,
            151_310,
        ),
        flat(3 * M5_SECS, 151_290, 151_300),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(
        trade.exit_milli, ask_open_already_gapped_above_stop_price,
        "ask_open is used when it has already gapped through the stop price"
    );
}

#[test]
fn protective_stop_can_trigger_on_the_same_bar_the_position_was_opened() {
    let long_entry_ask_open = 150_120;
    let bid_low_breaches_stop_price = 148_800;
    let stop_price_without_gap = 149_120;
    let eval = vec![
        flat(0, 150_000, 150_010),
        bar_ohlc(
            M5_SECS,
            150_110,
            150_130,
            bid_low_breaches_stop_price,
            148_900,
            long_entry_ask_open,
            150_140,
            148_810,
            148_910,
        ),
        flat(2 * M5_SECS, 148_900, 148_910),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 0, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(
        trade.entry_idx, 1,
        "the position opened and was stopped out on the same bar"
    );
    assert_eq!(trade.exit_idx, 1);
    assert_eq!(trade.entry_milli, long_entry_ask_open);
    assert_eq!(trade.exit_milli, stop_price_without_gap);
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(outcome.positions[1], 0);
}

#[test]
fn decider_can_reenter_on_the_bar_after_the_bar_the_stop_happened_on() {
    let long_entry_ask_open = 150_060;
    let bid_low_breaches_stop_price = 148_900;
    let rebuild_entry_ask_open = 150_210;
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_050, long_entry_ask_open),
        bar_ohlc(
            2 * M5_SECS,
            150_100,
            150_150,
            bid_low_breaches_stop_price,
            148_950,
            150_110,
            150_160,
            148_910,
            148_960,
        ),
        flat(3 * M5_SECS, 150_150, 150_160),
        flat(4 * M5_SECS, 150_200, rebuild_entry_ask_open),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, 0, 1, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 2);
    let stop_trade = outcome.trades[0];
    assert_eq!(stop_trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(stop_trade.entry_idx, 1);
    assert_eq!(stop_trade.exit_idx, 2);
    assert_eq!(stop_trade.exit_milli, 149_060);
    assert_eq!(
        outcome.positions[3], 0,
        "no reentry happens on bar 3 itself"
    );

    let rebuild_trade = outcome.trades[1];
    assert_eq!(rebuild_trade.entry_idx, 4);
    assert_eq!(rebuild_trade.entry_milli, rebuild_entry_ask_open);
    assert_eq!(outcome.positions[4], 1);
}

const FRIDAY_2024_01_05: i64 = 1_704_412_800;
const FRIDAY_LAST_BAR: i64 = FRIDAY_2024_01_05 + 23 * 3600 + 55 * 60;
const MONDAY_2024_01_08: i64 = FRIDAY_2024_01_05 + 3 * 24 * 3600;

#[test]
fn execution_uses_the_next_existing_bar_open_across_a_weekend_gap() {
    let eval = vec![
        flat(FRIDAY_LAST_BAR, 150_000, 150_010),
        flat(MONDAY_2024_01_08, 150_100, 150_120),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    assert_eq!(
        outcome.trades[0].entry_milli, 150_120,
        "execution happens at the next existing bar's open despite the multi-day gap"
    );
    assert_eq!(outcome.trades[0].entry_idx, 1);
}

#[test]
fn long_protective_stop_across_weekend_gap_exits_at_monday_bid_open() {
    let long_entry_ask_open = 150_120;
    let monday_bid_open_already_gapped_below_stop_price = 148_900;
    let eval = vec![
        flat(FRIDAY_LAST_BAR - M5_SECS, 150_000, 150_010),
        flat(FRIDAY_LAST_BAR, 150_110, long_entry_ask_open),
        bar_ohlc(
            MONDAY_2024_01_08,
            monday_bid_open_already_gapped_below_stop_price,
            148_950,
            148_800,
            148_820,
            148_910,
            148_960,
            148_810,
            148_830,
        ),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(trade.entry_idx, 1);
    assert_eq!(
        trade.exit_idx, 2,
        "stopped out on the first bar after the gap"
    );
    assert_eq!(
        trade.exit_milli, monday_bid_open_already_gapped_below_stop_price,
        "a stop crossed during the closed market exits at Monday's bid_open"
    );
    assert_eq!(outcome.positions, vec![0, 1, 0]);
}

#[test]
fn short_protective_stop_across_weekend_gap_exits_at_monday_ask_open() {
    let short_entry_bid_open = 150_100;
    let monday_ask_open_already_gapped_above_stop_price = 151_300;
    let eval = vec![
        flat(FRIDAY_LAST_BAR - M5_SECS, 150_000, 150_010),
        flat(FRIDAY_LAST_BAR, short_entry_bid_open, 150_110),
        bar_ohlc(
            MONDAY_2024_01_08,
            151_280,
            151_350,
            151_270,
            151_300,
            monday_ask_open_already_gapped_above_stop_price,
            151_360,
            151_280,
            151_310,
        ),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::ProtectiveStop);
    assert_eq!(trade.entry_idx, 1);
    assert_eq!(
        trade.exit_idx, 2,
        "stopped out on the first bar after the gap"
    );
    assert_eq!(
        trade.exit_milli, monday_ask_open_already_gapped_above_stop_price,
        "a stop crossed during the closed market exits at Monday's ask_open"
    );
    assert_eq!(outcome.positions, vec![0, -1, 0]);
}

#[test]
fn long_position_still_open_at_end_closes_at_last_bar_bid_close() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[1, 1, 1]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::EndOfData);
    assert_eq!(trade.entry_idx, 1);
    assert_eq!(trade.exit_idx, 2);
    assert_eq!(
        trade.exit_milli, 150_200,
        "a residual long closes at the last bar's bid_close"
    );
}

#[test]
fn short_position_still_open_at_end_closes_at_last_bar_ask_close() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[-1, -1, -1]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.trades.len(), 1);
    let trade = outcome.trades[0];
    assert_eq!(trade.reason, ExitReason::EndOfData);
    assert_eq!(
        trade.exit_milli, 150_220,
        "a residual short closes at the last bar's ask_close"
    );
}

#[test]
fn script_error_aborts_at_the_failing_bar_and_keeps_partial_trades_and_positions() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(vec![
        Ok(1),
        Err(DeciderError::Script("on_bar: division by zero".to_string())),
    ]);

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    match outcome.status {
        RunStatus::ScriptError { open_time, message } => {
            assert_eq!(
                open_time, M5_SECS,
                "open_time must be that of the failing bar (t=1)"
            );
            assert_eq!(message, "on_bar: division by zero");
        }
        RunStatus::Ok => panic!("expected ScriptError, got Ok"),
    }
    assert!(
        outcome.trades.is_empty(),
        "no trade closed before the error"
    );
    assert_eq!(
        outcome.positions,
        vec![0, 1],
        "positions must include the bar the error occurred on"
    );
}

#[test]
fn simulate_is_deterministic_across_repeated_runs() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_050, 150_060),
        flat(2 * M5_SECS, 150_100, 150_110),
        flat(3 * M5_SECS, 150_150, 150_160),
    ];
    let dataset = dataset_with_warmup(eval);
    let cfg = SimConfig::default();

    let mut decider_a = ScriptedDecider::new(signals(&[1, 1, -1, -1]));
    let outcome_a = simulate_ok(&dataset, &mut decider_a, &cfg);

    let mut decider_b = ScriptedDecider::new(signals(&[1, 1, -1, -1]));
    let outcome_b = simulate_ok(&dataset, &mut decider_b, &cfg);

    assert_eq!(outcome_a, outcome_b);
}

#[test]
fn single_bar_evaluation_period_does_not_panic() {
    let eval = vec![flat(0, 150_000, 150_010)];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[0]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.status, RunStatus::Ok);
    assert!(outcome.trades.is_empty());
    assert_eq!(outcome.positions, vec![0]);
    assert_eq!(
        decider.calls.len(),
        1,
        "on_bar is called even on the only (= last) bar"
    );
}

#[test]
fn script_error_on_the_last_bar_is_reported_and_skips_end_of_data_liquidation() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(vec![
        Ok(1),
        Ok(1),
        Err(DeciderError::Script("on_bar: boom on last bar".to_string())),
    ]);

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    match outcome.status {
        RunStatus::ScriptError { open_time, message } => {
            assert_eq!(
                open_time,
                2 * M5_SECS,
                "open_time must be that of the last bar"
            );
            assert_eq!(message, "on_bar: boom on last bar");
        }
        RunStatus::Ok => panic!("expected ScriptError on the last bar, got Ok"),
    }
    assert!(
        outcome.trades.is_empty(),
        "an aborted run must not liquidate the open position as EndOfData"
    );
    assert_eq!(outcome.positions, vec![0, 1, 1]);
}

#[test]
fn last_bar_signal_is_never_executed() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(signals(&[0, 0, 1]));

    let outcome = simulate_ok(&dataset, &mut decider, &SimConfig::default());

    assert_eq!(outcome.status, RunStatus::Ok);
    assert!(
        outcome.trades.is_empty(),
        "the last bar's on_bar result must not open a position"
    );
    assert_eq!(outcome.positions, vec![0, 0, 0]);
    assert_eq!(
        decider.calls.len(),
        3,
        "on_bar must be called on every evaluation bar including the last"
    );
}

#[test]
fn on_bar_returning_fatal_makes_simulate_return_err_and_produce_no_trades() {
    let eval = vec![
        flat(0, 150_000, 150_010),
        flat(M5_SECS, 150_100, 150_120),
        flat(2 * M5_SECS, 150_200, 150_220),
    ];
    let dataset = dataset_with_warmup(eval);
    let mut decider = ScriptedDecider::new(vec![
        Ok(1),
        Err(DeciderError::Fatal(SimError::Other(anyhow::anyhow!(
            "indicator cache mutex poisoned"
        )))),
    ]);

    let result = simulate(&dataset, &mut decider, &SimConfig::default());

    assert!(
        result.is_err(),
        "a Fatal decider error must make simulate return Err, got {result:?}"
    );
}
