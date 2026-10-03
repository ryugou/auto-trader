//! 1 回のシミュレーション: 約定、保護ストップ、売買記録（spec 9 章）。
//!
//! 評価期間の各足を 1 本ずつ処理し、足ごとに「1. 約定 → 2. 保護ストップ → 3. 判断」の順で
//! 状態を更新する（spec 9.2）。価格の比較・加減算はすべて `i64`（ミリ円）で行う（spec 3 章）。
//!
//! `Decider::on_bar` の戻り値は「次の足」で約定する遅延実行のため、ループは直前の足で得た
//! 戻り値（`pending_signal`）を今の足の冒頭で使う。`on_bar` は最後の足を含む評価期間の全ての足で
//! 1 回ずつ呼ぶ（spec 8.1: 足が確定するたびに 1 回。最後の足でスクリプトが実行時エラーになる場合も
//! `ScriptError` として検出し、`max_operations_per_run` の累計も spec どおりになる）。ただし
//! 最後の足の戻り値は約定先の足が存在しないため使わず、残ったポジションを `EndOfData` として
//! 清算する（spec 9.2 の 3）。

use crate::config::SimConfig;
use crate::script::{BarState, Decider};
use crate::series::Dataset;
use crate::types::PIP_MILLI;

/// 決済理由（spec 9.2 章）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    Signal,
    ProtectiveStop,
    EndOfData,
}

/// 1 件の売買（spec 9.2 章）。`entry_idx`/`exit_idx` は評価期間内の添字（`Dataset::eval_bars()`
/// に対する添字であり、`BarState::t`＝`Dataset::bars()` に対する添字とは異なる）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimTrade {
    pub direction: i8,
    pub entry_idx: usize,
    pub exit_idx: usize,
    pub entry_milli: i64,
    pub exit_milli: i64,
    pub pnl_milli: i64,
    pub reason: ExitReason,
}

/// シミュレーションの終了状態（spec 8.4 章: `on_bar` の実行時エラーはシミュレーションを
/// 中断させ、既定値に置き換えて続行しない）。
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Ok,
    ScriptError { open_time: i64, message: String },
}

/// 1 回のシミュレーションの結果。
#[derive(Debug, Clone, PartialEq)]
pub struct SimOutcome {
    pub status: RunStatus,
    pub trades: Vec<SimTrade>,
    /// 評価期間の各足の終値時点のポジション（spec 9.2: 1→2 を処理した後、3 の判断を呼ぶ前の
    /// 状態）。`ScriptError` の場合は中断した足までを含む。
    pub positions: Vec<i8>,
}

/// ポジションを閉じるときの価格（spec 9.2: 買いの決済は `bid_open`、売りの決済は `ask_open`）。
fn closing_price(position: i8, bid_open: i64, ask_open: i64) -> i64 {
    if position == 1 { bid_open } else { ask_open }
}

/// 新規にポジションを建てるときの価格（spec 9.2: 買いの新規は `ask_open`、売りの新規は
/// `bid_open`）。
fn opening_price(signal: i8, bid_open: i64, ask_open: i64) -> i64 {
    if signal == 1 { ask_open } else { bid_open }
}

/// 売買 1 件の損益（spec 9.2: 買いが `決済価格 - 建値`、売りが `建値 - 決済価格`）。
fn pnl_milli(direction: i8, entry_milli: i64, exit_milli: i64) -> i64 {
    if direction == 1 {
        exit_milli - entry_milli
    } else {
        entry_milli - exit_milli
    }
}

/// `dataset` の評価期間に `decider` を適用して 1 回のシミュレーションを行う（spec 9 章）。
///
/// `dataset` は `Dataset::new` で構築済みであり、評価期間に足が 1 本以上あることが保証されて
/// いる（`Dataset::new` 自身が 0 本を拒否する）ため、ここでは空の評価期間を想定しない。
pub fn simulate(dataset: &Dataset, decider: &mut dyn Decider, cfg: &SimConfig) -> SimOutcome {
    let offset = dataset.eval_start();
    let eval_bars = dataset.eval_bars();
    let n = eval_bars.len();

    let mut trades: Vec<SimTrade> = Vec::new();
    let mut positions: Vec<i8> = Vec::with_capacity(n);

    let mut position: i8 = 0;
    let mut entry_price_milli: i64 = 0;
    let mut entry_idx: usize = 0;
    // 直前の足で得た on_bar の戻り値。先頭の足（t == 0）では約定しないため未使用。
    let mut pending_signal: Option<i8> = None;

    let stop_offset_milli = cfg.protective_stop_pips * PIP_MILLI;

    for (t, bar) in eval_bars.iter().enumerate() {
        // 1. 約定（spec 9.2 の 1）: 先頭の足では行わない。
        if t > 0 {
            let signal = pending_signal.expect(
                "pending_signal must be Some from t==1 onward: every bar before the last calls \
                 the decider before the loop advances to the next bar",
            );
            if signal != position {
                if position != 0 {
                    let exit_milli = closing_price(position, bar.bid_open, bar.ask_open);
                    trades.push(SimTrade {
                        direction: position,
                        entry_idx,
                        exit_idx: t,
                        entry_milli: entry_price_milli,
                        exit_milli,
                        pnl_milli: pnl_milli(position, entry_price_milli, exit_milli),
                        reason: ExitReason::Signal,
                    });
                    position = 0;
                }
                if signal != 0 {
                    entry_price_milli = opening_price(signal, bar.bid_open, bar.ask_open);
                    entry_idx = t;
                    position = signal;
                }
            }
        }

        // 2. 保護ストップ（spec 9.2 の 2）。新規建てと同じ足の内部で判定するため、1 の直後に
        // 行う（始値で建てた直後にその足の高安がストップを割り込む場合、同じ足で決済する）。
        if position != 0 {
            let stop_price = if position == 1 {
                entry_price_milli - stop_offset_milli
            } else {
                entry_price_milli + stop_offset_milli
            };
            let hit = if position == 1 {
                bar.bid_low <= stop_price
            } else {
                bar.ask_high >= stop_price
            };
            if hit {
                let exit_milli = if position == 1 {
                    stop_price.min(bar.bid_open)
                } else {
                    stop_price.max(bar.ask_open)
                };
                trades.push(SimTrade {
                    direction: position,
                    entry_idx,
                    exit_idx: t,
                    entry_milli: entry_price_milli,
                    exit_milli,
                    pnl_milli: pnl_milli(position, entry_price_milli, exit_milli),
                    reason: ExitReason::ProtectiveStop,
                });
                position = 0;
            }
        }

        positions.push(position);

        // 3. 判断（spec 9.2 の 3）: 最後の足でも呼ぶ。最後の足の戻り値は約定しないので保持しない。
        let bars_held = if position == 0 {
            0
        } else {
            (t - entry_idx) as u32
        };
        let state = BarState {
            t: offset + t,
            position,
            entry_price_milli,
            bars_held,
        };
        match decider.on_bar(state) {
            // spec 8.4: 1・-1・0 以外は既定値に置き換えず中断する。放置すると position が
            // 不正値になり、opening/closing_price が 1 以外をすべて売り扱いして続行してしまう。
            Ok(signal) if !(-1..=1).contains(&signal) => {
                return SimOutcome {
                    status: RunStatus::ScriptError {
                        open_time: bar.open_time,
                        message: format!("on_bar returned {signal} (expected 1, -1, or 0)"),
                    },
                    trades,
                    positions,
                };
            }
            Ok(signal) => {
                if t + 1 < n {
                    pending_signal = Some(signal);
                }
            }
            Err(message) => {
                return SimOutcome {
                    status: RunStatus::ScriptError {
                        open_time: bar.open_time,
                        message,
                    },
                    trades,
                    positions,
                };
            }
        }
    }

    // 最後の足の on_bar の戻り値は約定しない。残ったポジションは期末で清算する（spec 9.2）。
    if position != 0 {
        let last = &eval_bars[n - 1];
        let exit_milli = if position == 1 {
            last.bid_close
        } else {
            last.ask_close
        };
        trades.push(SimTrade {
            direction: position,
            entry_idx,
            exit_idx: n - 1,
            entry_milli: entry_price_milli,
            exit_milli,
            pnl_milli: pnl_milli(position, entry_price_milli, exit_milli),
            reason: ExitReason::EndOfData,
        });
    }

    SimOutcome {
        status: RunStatus::Ok,
        trades,
        positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Bar, M5_SECS};
    use std::collections::VecDeque;

    // ---- test helpers -------------------------------------------------------

    /// 高安が始値・終値と同じフラットな `Bar`（スプレッドは `bid`/`ask` の差として直接渡す）。
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

    /// OHLC を個別に指定できる `Bar`（保護ストップのテストで高安を作るのに使う）。
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

    /// `eval_bars` の前に 1 本の warmup 足を置いた `Dataset`（計画 Task 6 Step 1: `warmup_bars
    /// = 1` で作る）。warmup 足の値はシミュレーションに現れないので意味を持たない。
    fn dataset_with_warmup(eval_bars: Vec<Bar>) -> Dataset {
        let warmup = flat(eval_bars[0].open_time - M5_SECS, 149_000, 149_010);
        let from = eval_bars[0].open_time;
        let to = eval_bars.last().unwrap().open_time + M5_SECS;
        let mut all = vec![warmup];
        all.extend(eval_bars);
        Dataset::new(all, from, to, 1, 64).expect("test dataset must build")
    }

    /// テスト用の `Decider`: 呼び出しごとに指定した応答を順番に返し、渡された `BarState` を
    /// 記録する（約定・保護ストップ処理後の状態をアサーションで検証するため）。
    struct ScriptedDecider {
        responses: VecDeque<Result<i8, String>>,
        calls: Vec<BarState>,
    }

    impl ScriptedDecider {
        fn new(responses: Vec<Result<i8, String>>) -> Self {
            Self {
                responses: responses.into(),
                calls: Vec::new(),
            }
        }
    }

    impl Decider for ScriptedDecider {
        fn on_bar(&mut self, state: BarState) -> Result<i8, String> {
            self.calls.push(state);
            self.responses.pop_front().unwrap_or_else(|| {
                panic!(
                    "ScriptedDecider ran out of scripted responses at call #{}",
                    self.calls.len()
                )
            })
        }
    }

    fn signals(values: &[i8]) -> Vec<Result<i8, String>> {
        values.iter().map(|&v| Ok(v)).collect()
    }

    // ---- on_bar の戻り値検証（spec 8.4） -----------------------------------------

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

            let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert!(matches!(
            outcome.status,
            RunStatus::ScriptError { open_time, .. } if open_time == M5_SECS
        ));
    }

    // ---- 約定: 先頭の足では約定しない -----------------------------------------

    #[test]
    fn no_execution_on_first_bar_and_long_opens_at_second_bar_ask_open() {
        let eval = vec![flat(0, 150_000, 150_010), flat(M5_SECS, 150_100, 150_120)];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

    // ---- 約定: 買いの決済が bid_open、売りの新規・決済 -------------------------

    #[test]
    fn long_round_trip_uses_ask_open_entry_and_bid_open_exit() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_220),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, 0, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.entry_milli, 150_100, "short entry uses bid_open");
        assert_eq!(trade.exit_milli, 150_220, "short exit uses ask_open");
        assert_eq!(trade.reason, ExitReason::Signal);
        assert_eq!(trade.pnl_milli, -120);
    }

    // ---- ドテン ---------------------------------------------------------------

    #[test]
    fn doten_closes_and_reopens_at_the_same_bar_open_producing_two_trades() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_210),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, -1, -1]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

    // ---- BarState のポジション・bars_held ライフサイクル -----------------------

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
        // t=0: flat -> open long. t=1: hold (bars_held を 1 へ進ませる)。
        // t=2: doten (-1) -> t=3 で決済・売り直し。t=3: hold。
        let mut decider = ScriptedDecider::new(signals(&[1, 1, -1, -1, -1]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());
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

    // ---- 保護ストップ: 買い ----------------------------------------------------

    #[test]
    fn protective_stop_closes_long_at_stop_price_when_crossed_intrabar() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_110, 150_120), // long opens at ask_open = 150_120
            bar_ohlc(
                2 * M5_SECS,
                149_300, // bid_open: ストップ価格(149_120)より上 = ギャップではない
                149_350,
                149_000, // bid_low: 149_120 を割り込む
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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(trade.exit_idx, 2);
        assert_eq!(
            trade.exit_milli, 149_120,
            "stop price itself is used when bid_open has not already gapped through it"
        );
        assert_eq!(outcome.positions[2], 0);
    }

    #[test]
    fn protective_stop_closes_long_at_bid_open_when_gapped_through() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_110, 150_120), // long opens at ask_open = 150_120
            bar_ohlc(
                2 * M5_SECS,
                148_900, // bid_open はすでにストップ価格(149_120)より下
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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(
            trade.exit_milli, 148_900,
            "bid_open is used when it has already gapped through the stop price"
        );
    }

    // ---- 保護ストップ: 売り（対称） --------------------------------------------

    #[test]
    fn protective_stop_closes_short_at_stop_price_when_crossed_intrabar() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_110), // short opens at bid_open = 150_100
            bar_ohlc(
                2 * M5_SECS,
                150_900,
                151_300, // ask_high: ストップ価格(151_100)を上抜く
                150_880,
                150_950,
                150_890,
                151_310,
                150_890,
                150_960,
            ),
            flat(3 * M5_SECS, 150_950, 150_960),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(
            trade.exit_milli, 151_100,
            "stop price itself is used when ask_open has not already gapped through it"
        );
        assert_eq!(outcome.positions[2], 0);
    }

    #[test]
    fn protective_stop_closes_short_at_ask_open_when_gapped_through() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_110), // short opens at bid_open = 150_100
            bar_ohlc(
                2 * M5_SECS,
                151_280,
                151_350, // ask_open はすでにストップ価格(151_100)より上
                151_270,
                151_300,
                151_300,
                151_360,
                151_280,
                151_310,
            ),
            flat(3 * M5_SECS, 151_290, 151_300),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(
            trade.exit_milli, 151_300,
            "ask_open is used when it has already gapped through the stop price"
        );
    }

    // ---- 保護ストップ: 始値で建てた足の中で到達 ---------------------------------

    #[test]
    fn protective_stop_can_trigger_on_the_same_bar_the_position_was_opened() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            bar_ohlc(
                M5_SECS, 150_110, 150_130,
                148_800, // bid_low: ストップ(149_120)を割り込む
                148_900, 150_120, // ask_open: long entry
                150_140, 148_810, 148_910,
            ),
            flat(2 * M5_SECS, 148_900, 148_910),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, 0, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(
            trade.entry_idx, 1,
            "the position opened and was stopped out on the same bar"
        );
        assert_eq!(trade.exit_idx, 1);
        assert_eq!(trade.entry_milli, 150_120);
        assert_eq!(trade.exit_milli, 149_120);
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(outcome.positions[1], 0);
    }

    // ---- 保護ストップの直後の建て直し ------------------------------------------

    #[test]
    fn decider_can_reenter_on_the_bar_after_the_bar_the_stop_happened_on() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_050, 150_060), // long opens at ask_open = 150_060
            bar_ohlc(
                2 * M5_SECS,
                150_100,
                150_150,
                148_900, // bid_low: ストップ(149_060)を割り込む
                148_950,
                150_110,
                150_160,
                148_910,
                148_960,
            ),
            flat(3 * M5_SECS, 150_150, 150_160),
            flat(4 * M5_SECS, 150_200, 150_210), // rebuild opens at ask_open = 150_210
        ];
        let dataset = dataset_with_warmup(eval);
        // t=0: open. t=1: hold. t=2(stop 発生足): いったん 0 を返す。
        // t=3(ストップの直後の足): 1 を返す -> t=4 で建て直す。
        let mut decider = ScriptedDecider::new(signals(&[1, 1, 0, 1, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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
        assert_eq!(rebuild_trade.entry_milli, 150_210);
        assert_eq!(outcome.positions[4], 1);
    }

    // ---- 足が飛ぶ区間をまたぐ約定 ------------------------------------------------

    /// 2024-01-05（金）00:00 UTC のエポック秒。
    const FRIDAY_2024_01_05: i64 = 1_704_412_800;
    /// 金曜の最後の M5 足（23:55 UTC）。
    const FRIDAY_LAST_BAR: i64 = FRIDAY_2024_01_05 + 23 * 3600 + 55 * 60;
    /// 翌週月曜（2024-01-08）00:00 UTC。金曜の最後の足から土日を挟んで足が飛ぶ。
    const MONDAY_2024_01_08: i64 = FRIDAY_2024_01_05 + 3 * 24 * 3600;

    #[test]
    fn execution_uses_the_next_existing_bar_open_across_a_weekend_gap() {
        let eval = vec![
            flat(FRIDAY_LAST_BAR, 150_000, 150_010),
            flat(MONDAY_2024_01_08, 150_100, 150_120),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        assert_eq!(
            outcome.trades[0].entry_milli, 150_120,
            "execution happens at the next existing bar's open despite the multi-day gap"
        );
        assert_eq!(outcome.trades[0].entry_idx, 1);
    }

    #[test]
    fn long_protective_stop_across_weekend_gap_exits_at_monday_bid_open() {
        let eval = vec![
            flat(FRIDAY_LAST_BAR - M5_SECS, 150_000, 150_010),
            flat(FRIDAY_LAST_BAR, 150_110, 150_120), // long opens at ask_open = 150_120
            bar_ohlc(
                MONDAY_2024_01_08,
                148_900, // 月曜の bid_open はすでにストップ価格(149_120)より下
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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(trade.entry_idx, 1);
        assert_eq!(
            trade.exit_idx, 2,
            "stopped out on the first bar after the gap"
        );
        assert_eq!(
            trade.exit_milli, 148_900,
            "a stop crossed during the closed market exits at Monday's bid_open"
        );
        assert_eq!(outcome.positions, vec![0, 1, 0]);
    }

    #[test]
    fn short_protective_stop_across_weekend_gap_exits_at_monday_ask_open() {
        let eval = vec![
            flat(FRIDAY_LAST_BAR - M5_SECS, 150_000, 150_010),
            flat(FRIDAY_LAST_BAR, 150_100, 150_110), // short opens at bid_open = 150_100
            bar_ohlc(
                MONDAY_2024_01_08,
                151_280,
                151_350, // 月曜の ask_open はすでにストップ価格(151_100)より上
                151_270,
                151_300,
                151_300,
                151_360,
                151_280,
                151_310,
            ),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[-1, -1, 0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::ProtectiveStop);
        assert_eq!(trade.entry_idx, 1);
        assert_eq!(
            trade.exit_idx, 2,
            "stopped out on the first bar after the gap"
        );
        assert_eq!(
            trade.exit_milli, 151_300,
            "a stop crossed during the closed market exits at Monday's ask_open"
        );
        assert_eq!(outcome.positions, vec![0, -1, 0]);
    }

    // ---- 期末の残ポジション清算 --------------------------------------------------

    #[test]
    fn long_position_still_open_at_end_closes_at_last_bar_bid_close() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_220),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[1, 1, 1]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.trades.len(), 1);
        let trade = outcome.trades[0];
        assert_eq!(trade.reason, ExitReason::EndOfData);
        assert_eq!(
            trade.exit_milli, 150_220,
            "a residual short closes at the last bar's ask_close"
        );
    }

    // ---- script_error ------------------------------------------------------

    #[test]
    fn script_error_aborts_at_the_failing_bar_and_keeps_partial_trades_and_positions() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_220),
        ];
        let dataset = dataset_with_warmup(eval);
        let mut decider =
            ScriptedDecider::new(vec![Ok(1), Err("on_bar: division by zero".to_string())]);

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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

    // ---- 決定性・境界値 ----------------------------------------------------

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
        let outcome_a = simulate(&dataset, &mut decider_a, &cfg);

        let mut decider_b = ScriptedDecider::new(signals(&[1, 1, -1, -1]));
        let outcome_b = simulate(&dataset, &mut decider_b, &cfg);

        assert_eq!(outcome_a, outcome_b);
    }

    #[test]
    fn single_bar_evaluation_period_does_not_panic() {
        let eval = vec![flat(0, 150_000, 150_010)];
        let dataset = dataset_with_warmup(eval);
        let mut decider = ScriptedDecider::new(signals(&[0]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

        assert_eq!(outcome.status, RunStatus::Ok);
        assert!(outcome.trades.is_empty());
        assert_eq!(outcome.positions, vec![0]);
        assert_eq!(
            decider.calls.len(),
            1,
            "on_bar is called even on the only (= last) bar"
        );
    }

    // ---- 最後の足の on_bar（spec 8.1 / 9.2 の 3） ---------------------------------

    #[test]
    fn script_error_on_the_last_bar_is_reported_and_skips_end_of_data_liquidation() {
        let eval = vec![
            flat(0, 150_000, 150_010),
            flat(M5_SECS, 150_100, 150_120),
            flat(2 * M5_SECS, 150_200, 150_220),
        ];
        let dataset = dataset_with_warmup(eval);
        // t=0 で買い -> t=1 で建つ。t=2（最後の足）で on_bar が失敗する。
        let mut decider = ScriptedDecider::new(vec![
            Ok(1),
            Ok(1),
            Err("on_bar: boom on last bar".to_string()),
        ]);

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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
        // 最後の足（t=2）で 0 のまま 1 を返しても、約定先の足がないので建たない。
        let mut decider = ScriptedDecider::new(signals(&[0, 0, 1]));

        let outcome = simulate(&dataset, &mut decider, &SimConfig::default());

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
}
