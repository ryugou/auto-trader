use crate::regime;
use auto_trader_core::types::{Direction, ExitReason, Trade};
use rust_decimal::Decimal;
use std::collections::HashMap;

/// Returns the Japanese text that announces the opening of `trade`: its exchange, pair, direction, strategy with `allocation_pct` as a percentage (N/A if none), prices, quantity, the regime classified from `indicators`, every indicator rounded to two places in key order, and the deviation of the entry price from `sma_20` in percent if that indicator is positive.
pub fn format_trade_open(
    trade: &Trade,
    indicators: &HashMap<String, Decimal>,
    allocation_pct: Option<Decimal>,
) -> String {
    let dir = match trade.direction {
        Direction::Long => "ロング",
        Direction::Short => "ショート",
    };
    let regime = regime::classify(indicators);
    let sma20_dev = indicators.get("sma_20").and_then(|sma| {
        if *sma > Decimal::ZERO {
            Some(((trade.entry_price - sma) / sma * Decimal::from(100)).round_dp(2))
        } else {
            None
        }
    });

    let alloc_display = allocation_pct
        .map(|a| format!("{}", (a * Decimal::from(100)).round()))
        .unwrap_or_else(|| "N/A".to_string());
    let mut text = format!(
        "[{}] {} {} エントリー。\n\
         ▸ 戦略: {} (allocation: {}%)\n\
         ▸ 価格: {} / SL: {} / TP: {}\n\
         ▸ 数量: {}\n\
         ▸ レジーム: {}\n\
         ▸ 指標:",
        trade.exchange,
        trade.pair,
        dir,
        trade.strategy_name,
        alloc_display,
        trade.entry_price,
        trade.stop_loss,
        trade
            .take_profit
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        trade.quantity,
        regime.as_str(),
    );

    let mut sorted: Vec<_> = indicators.iter().collect();
    sorted.sort_by_key(|(k, _)| k.as_str());
    for (key, val) in sorted {
        text.push_str(&format!(" {}={},", key, val.round_dp(2)));
    }
    if let Some(dev) = sma20_dev {
        text.push_str(&format!("\n▸ SMA20乖離: {}%", dev));
    }
    text
}

/// Returns the Japanese text that announces the close of `trade`: its result, PnL, fees, net PnL, holding time, entry and exit prices with the change rate, the account balance against `account_initial` when both are given and the initial one is positive, the regime and the RSI, ATR and ADX of `entry_indicators` that it holds, and a post-mortem note.
pub fn format_trade_close(
    trade: &Trade,
    entry_indicators: Option<&serde_json::Value>,
    account_balance: Option<Decimal>,
    account_initial: Option<Decimal>,
) -> String {
    let dir = match trade.direction {
        Direction::Long => "ロング",
        Direction::Short => "ショート",
    };
    let pnl = trade.pnl_amount.unwrap_or_default();
    let fees = trade.fees;
    let net_pnl = pnl - fees;
    let exit_reason = trade
        .exit_reason
        .map(|r| format!("{r:?}"))
        .unwrap_or_else(|| "unknown".to_string());

    let holding = trade
        .exit_at
        .map(|exit| {
            let dur = exit.signed_duration_since(trade.entry_at);
            let mins = dur.num_minutes();
            if mins < 60 {
                format!("{}分", mins)
            } else {
                format!("{}時間{}分", mins / 60, mins % 60)
            }
        })
        .unwrap_or_else(|| "-".to_string());

    let price_change_pct = trade.exit_price.map(|exit| {
        if trade.entry_price > Decimal::ZERO {
            ((exit - trade.entry_price) / trade.entry_price * Decimal::from(100)).round_dp(2)
        } else {
            Decimal::ZERO
        }
    });

    let mut text = format!(
        "[{}] {} {} 決済。\n\
         ▸ 戦略: {}\n\
         ▸ 結果: {} / PnL: {} / 手数料: {} / 純損益: {}\n\
         ▸ 保有時間: {}\n\
         ▸ エントリー: {} → 決済: {}",
        trade.exchange,
        trade.pair,
        dir,
        trade.strategy_name,
        exit_reason,
        pnl,
        fees,
        net_pnl,
        holding,
        trade.entry_price,
        trade
            .exit_price
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string()),
    );

    if let Some(pct) = price_change_pct {
        text.push_str(&format!(" (変動率: {}%)", pct));
    }

    if let (Some(bal), Some(init)) = (account_balance, account_initial)
        && init > Decimal::ZERO
    {
        let bal_pct = ((bal - init) / init * Decimal::from(100)).round_dp(1);
        text.push_str(&format!("\n▸ 口座残高: {} (初期比: {}%)", bal, bal_pct));
    }

    if let Some(ind) = entry_indicators {
        if let Some(regime_str) = ind.get("regime").and_then(|v| v.as_str()) {
            text.push_str(&format!("\n▸ エントリー時レジーム: {}", regime_str));
        }
        if let Some(rsi) = ind.get("rsi_14") {
            text.push_str(&format!(", RSI: {}", rsi));
        }
        if let Some(atr) = ind.get("atr_14") {
            text.push_str(&format!(", ATR: {}", atr));
        }
        if let Some(adx) = ind.get("adx_14") {
            text.push_str(&format!(", ADX: {}", adx));
        }
    }

    text.push_str(&format!(
        "\n▸ 反省材料: {}",
        post_mortem(trade, entry_indicators)
    ));

    text
}

/// Returns the feedback rating of `trade` from 2 to 5: 5 for a profitable exit at the take profit or by a trailing channel, mean reach or trailing MA, 4 for a profitable exit by time limit, 2 for a stop loss with no profit, and 3 otherwise.
pub fn compute_feedback_rating(trade: &Trade) -> i32 {
    let is_profit = trade.pnl_amount.map(|p| p > Decimal::ZERO).unwrap_or(false);
    match trade.exit_reason {
        Some(ExitReason::TpHit)
        | Some(ExitReason::StrategyTrailingChannel)
        | Some(ExitReason::StrategyMeanReached)
        | Some(ExitReason::StrategyTrailingMa)
            if is_profit =>
        {
            5
        }
        Some(ExitReason::TpHit)
        | Some(ExitReason::StrategyTrailingChannel)
        | Some(ExitReason::StrategyMeanReached)
        | Some(ExitReason::StrategyTrailingMa) => 3,
        Some(ExitReason::StrategyTimeLimit) if is_profit => 4,
        Some(ExitReason::StrategyTimeLimit) => 3,
        Some(ExitReason::SlHit) if is_profit => 3,
        Some(ExitReason::SlHit) => 2,
        _ => 3,
    }
}

fn post_mortem(trade: &Trade, entry_indicators: Option<&serde_json::Value>) -> &'static str {
    let is_loss = trade.pnl_amount.map(|p| p < Decimal::ZERO).unwrap_or(false);
    let is_sl = trade.exit_reason == Some(ExitReason::SlHit);
    let rsi_high = entry_indicators
        .and_then(|i| i.get("rsi_14"))
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        })
        .map(|r| r > 65.0)
        .unwrap_or(false);
    let rsi_low = entry_indicators
        .and_then(|i| i.get("rsi_14"))
        .and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        })
        .map(|r| r < 35.0)
        .unwrap_or(false);

    if is_sl && rsi_high && trade.direction == Direction::Long {
        "RSI 過熱圏でのロング、逆行リスクが高い局面"
    } else if is_sl && rsi_low && trade.direction == Direction::Short {
        "RSI 売られすぎ圏でのショート、反発リスクが高い局面"
    } else if is_sl && is_loss {
        "損切り発動、SL 距離の見直しまたはエントリー条件の精査が必要"
    } else if trade.exit_reason == Some(ExitReason::StrategyTimeLimit) {
        "時間切れ。トレンド未発生、エントリー条件の見直し候補"
    } else if !is_loss {
        "想定通りの利益確定。条件の再現性あり"
    } else {
        "分析データ不足"
    }
}
