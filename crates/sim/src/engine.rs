use crate::config::SimConfig;
use crate::error::SimError;
use crate::script::{BarState, Decider, DeciderError};
use crate::series::Dataset;
use crate::types::{Bar, PIP_MILLI};

/// Reason why a simulated trade was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// The script returned a signal that differs from the position.
    Signal,
    /// The bar reached the protective stop price.
    ProtectiveStop,
    /// The evaluation period ended while the position was open.
    EndOfData,
}

/// Closed trade of a simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimTrade {
    /// 1 for a long trade and -1 for a short trade.
    pub direction: i8,
    /// Index of the evaluation bar at which the trade was opened.
    pub entry_idx: usize,
    /// Index of the evaluation bar at which the trade was closed.
    pub exit_idx: usize,
    /// Entry price in milli units.
    pub entry_milli: i64,
    /// Exit price in milli units.
    pub exit_milli: i64,
    /// Profit in milli units, negative for a loss.
    pub pnl_milli: i64,
    /// Reason why the trade was closed.
    pub reason: ExitReason,
}

/// Status of a simulation run.
#[derive(Debug, Clone, PartialEq)]
pub enum RunStatus {
    /// The script ran through every evaluation bar.
    Ok,
    /// The script failed at a bar, which stopped the run.
    ScriptError {
        /// Open time of the bar at which the script failed, in Unix seconds.
        open_time: i64,
        /// Description of the failure.
        message: String,
    },
}

/// Result of a simulation run.
#[derive(Debug, Clone, PartialEq)]
pub struct SimOutcome {
    /// Whether the run reached the end of the evaluation bars.
    pub status: RunStatus,
    /// Trades closed during the run, in time order.
    pub trades: Vec<SimTrade>,
    /// Position held at each evaluation bar that the run reached: 1, -1 or 0.
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

/// Runs the decider over the evaluation bars of the dataset and returns the closed trades and the positions. A signal returned at a bar is executed at the open of the next bar, and a position is closed at the protective stop of `cfg.protective_stop_pips` or at the close of the last bar. A failure of the script, or a signal other than 1, -1 or 0, stops the run and is returned as `RunStatus::ScriptError`.
///
/// # Errors
///
/// - Returns an error if the decider reports a fatal failure.
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
