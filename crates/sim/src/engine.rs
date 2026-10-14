use crate::config::SimConfig;
use crate::error::SimError;
use crate::script::{BarState, Decider, DeciderError};
use crate::series::Dataset;
use crate::types::{Bar, PIP_MILLI};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    Signal,
    ProtectiveStop,
    EndOfData,
}

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

#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    Ok,
    ScriptError { open_time: i64, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimOutcome {
    pub status: RunStatus,
    pub trades: Vec<SimTrade>,
    pub positions: Vec<i8>,
}

fn closed_trade(
    direction: i8,
    entry_idx: usize,
    exit_idx: usize,
    entry_milli: i64,
    exit_milli: i64,
    reason: ExitReason,
) -> SimTrade {
    SimTrade {
        direction,
        entry_idx,
        exit_idx,
        entry_milli,
        exit_milli,
        pnl_milli: pnl_milli(direction, entry_milli, exit_milli),
        reason,
    }
}

fn closing_price(position: i8, bid_open: i64, ask_open: i64) -> i64 {
    if position == 1 { bid_open } else { ask_open }
}

fn opening_price(signal: i8, bid_open: i64, ask_open: i64) -> i64 {
    if signal == 1 { ask_open } else { bid_open }
}

fn pnl_milli(direction: i8, entry_milli: i64, exit_milli: i64) -> i64 {
    if direction == 1 {
        exit_milli - entry_milli
    } else {
        entry_milli - exit_milli
    }
}

fn apply_signal_pending_from_previous_bar(
    signal: i8,
    t: usize,
    bar: &Bar,
    position: i8,
    entry_price_milli: i64,
    entry_idx: usize,
    trades: &mut Vec<SimTrade>,
) -> (i8, i64, usize) {
    if signal == position {
        return (position, entry_price_milli, entry_idx);
    }
    let mut position = position;
    let mut entry_price_milli = entry_price_milli;
    let mut entry_idx = entry_idx;
    if position != 0 {
        let exit_milli = closing_price(position, bar.bid_open, bar.ask_open);
        trades.push(closed_trade(
            position,
            entry_idx,
            t,
            entry_price_milli,
            exit_milli,
            ExitReason::Signal,
        ));
        position = 0;
    }
    if signal != 0 {
        entry_price_milli = opening_price(signal, bar.bid_open, bar.ask_open);
        entry_idx = t;
        position = signal;
    }
    (position, entry_price_milli, entry_idx)
}

fn apply_protective_stop_which_can_trigger_on_the_entry_bar(
    position: i8,
    entry_price_milli: i64,
    entry_idx: usize,
    stop_offset_milli: i64,
    t: usize,
    bar: &Bar,
    trades: &mut Vec<SimTrade>,
) -> i8 {
    if position == 0 {
        return position;
    }
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
    if !hit {
        return position;
    }
    let exit_milli = if position == 1 {
        stop_price.min(bar.bid_open)
    } else {
        stop_price.max(bar.ask_open)
    };
    trades.push(closed_trade(
        position,
        entry_idx,
        t,
        entry_price_milli,
        exit_milli,
        ExitReason::ProtectiveStop,
    ));
    0
}

fn build_bar_state(
    offset: usize,
    t: usize,
    position: i8,
    entry_price_milli: i64,
    entry_idx: usize,
) -> BarState {
    let bars_held = if position == 0 {
        0
    } else {
        (t - entry_idx) as u32
    };
    BarState {
        t: offset + t,
        position,
        entry_price_milli,
        bars_held,
    }
}

fn close_at_period_end(
    position: i8,
    entry_price_milli: i64,
    entry_idx: usize,
    eval_bars: &[Bar],
    trades: &mut Vec<SimTrade>,
) {
    if position == 0 {
        return;
    }
    let n = eval_bars.len();
    let last = &eval_bars[n - 1];
    let exit_milli = if position == 1 {
        last.bid_close
    } else {
        last.ask_close
    };
    trades.push(closed_trade(
        position,
        entry_idx,
        n - 1,
        entry_price_milli,
        exit_milli,
        ExitReason::EndOfData,
    ));
}

pub fn simulate(
    dataset: &Dataset,
    decider: &mut dyn Decider,
    cfg: &SimConfig,
) -> Result<SimOutcome, SimError> {
    let offset = dataset.eval_start();
    let eval_bars = dataset.eval_bars();
    let n = eval_bars.len();

    let mut trades: Vec<SimTrade> = Vec::new();
    let mut positions: Vec<i8> = Vec::with_capacity(n);

    let mut position: i8 = 0;
    let mut entry_price_milli: i64 = 0;
    let mut entry_idx: usize = 0;
    let mut pending_signal: Option<i8> = None;

    let stop_offset_milli = cfg.protective_stop_pips * PIP_MILLI;

    for (t, bar) in eval_bars.iter().enumerate() {
        if let Some(signal) = pending_signal {
            (position, entry_price_milli, entry_idx) = apply_signal_pending_from_previous_bar(
                signal,
                t,
                bar,
                position,
                entry_price_milli,
                entry_idx,
                &mut trades,
            );
        }

        position = apply_protective_stop_which_can_trigger_on_the_entry_bar(
            position,
            entry_price_milli,
            entry_idx,
            stop_offset_milli,
            t,
            bar,
            &mut trades,
        );

        positions.push(position);

        let state = build_bar_state(offset, t, position, entry_price_milli, entry_idx);
        let message = match decider.on_bar(state) {
            Ok(signal) if (-1..=1).contains(&signal) => {
                pending_signal = (t + 1 < n).then_some(signal);
                continue;
            }
            Ok(signal) => format!("on_bar returned {signal} (expected 1, -1, or 0)"),
            Err(DeciderError::Script(message)) => message,
            Err(DeciderError::Fatal(err)) => return Err(err),
        };
        return Ok(SimOutcome {
            status: RunStatus::ScriptError {
                open_time: bar.open_time,
                message,
            },
            trades,
            positions,
        });
    }

    close_at_period_end(
        position,
        entry_price_milli,
        entry_idx,
        eval_bars,
        &mut trades,
    );

    Ok(SimOutcome {
        status: RunStatus::Ok,
        trades,
        positions,
    })
}

#[cfg(test)]
mod tests;
