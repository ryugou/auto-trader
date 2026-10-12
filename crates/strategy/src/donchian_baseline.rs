use std::collections::VecDeque;

use auto_trader_core::types::Candle;
use auto_trader_market::indicators;
use rust_decimal::Decimal;

use crate::candle_history::{closes, highs, lows};

pub(crate) fn min_history_len_for_entry(
    entry_channel: usize,
    atr_baseline_bars: usize,
    atr_period: usize,
) -> usize {
    entry_channel + atr_baseline_bars + atr_period + 1
}

pub(crate) fn min_history_len_for_baseline_atr(
    atr_baseline_bars: usize,
    atr_period: usize,
) -> usize {
    atr_baseline_bars + atr_period + 2
}

pub(crate) fn channel_excluding_current_bar(
    highs: &[Decimal],
    lows: &[Decimal],
    channel: usize,
) -> Option<(Decimal, Decimal)> {
    indicators::donchian_channel(highs, lows, channel, false)
}

pub(crate) fn baseline_atr_excluding_current_bar(
    history: &VecDeque<Candle>,
    atr_baseline_bars: usize,
    atr_period: usize,
) -> Option<Decimal> {
    if history.len() < min_history_len_for_baseline_atr(atr_baseline_bars, atr_period) {
        return None;
    }
    let highs = highs(history);
    let lows = lows(history);
    let closes = closes(history);
    let latest_prior = history.len() - 2;
    let start = latest_prior + 1 - atr_baseline_bars;
    let mut sum = Decimal::ZERO;
    let mut count = 0u32;
    for end in start..=latest_prior {
        if end < atr_period + 1 {
            continue;
        }
        if let Some(v) = indicators::atr(&highs[..=end], &lows[..=end], &closes[..=end], atr_period)
        {
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        return None;
    }
    Some(sum / Decimal::from(count))
}
