use auto_trader_core::config::SwapRateEntry;
use auto_trader_core::types::{Direction, Pair, TradeStatus};
use chrono::Utc;
use rust_decimal_macros::dec;
use uuid::Uuid;

use super::*;

fn make_trade(pair: &str, direction: Direction, entry_price: Decimal, quantity: Decimal) -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "test".to_string(),
        pair: Pair::new(pair),
        exchange: Exchange::BitflyerCfd,
        direction,
        entry_price,
        exit_price: None,
        stop_loss: dec!(0),
        take_profit: None,
        quantity,
        leverage: dec!(1),
        fees: dec!(0),
        entry_at: Utc::now(),
        exit_at: None,
        pnl_amount: None,
        exit_reason: None,
        status: TradeStatus::Open,
        max_hold_until: None,
        exchange_position_id: None,
        stop_order_id: None,
    }
}

#[test]
fn compute_fee_bitflyer_cfd_truncates_toward_zero_per_case() {
    let cases = [
        ("no_remainder", dec!(0.01), dec!(40)),
        ("fractional_fee_truncated", dec!(0.012345), dec!(49)),
    ];
    let swap_config = GmoFxSwapConfig::default();

    for (name, quantity, expected) in cases {
        let trade = make_trade("FX_BTC_JPY", Direction::Long, dec!(10000000), quantity);

        let fee = compute_fee(Exchange::BitflyerCfd, &trade, &swap_config);

        assert_eq!(fee, Some(expected), "case {name}");
    }
}

#[test]
fn compute_fee_gmo_fx_matches_compute_daily_swap_when_rate_configured() {
    let rate = SwapRateEntry {
        long: dec!(-20),
        short: dec!(15),
    };
    let mut swap_config = GmoFxSwapConfig::default();
    swap_config.rates.insert("USD_JPY".to_string(), rate);
    let trade = make_trade("USD_JPY", Direction::Long, dec!(150), dec!(10000));

    let fee = compute_fee(Exchange::GmoFx, &trade, &swap_config);

    let expected = auto_trader_core::swap::compute_daily_swap(rate, Direction::Long, dec!(10000));
    assert_eq!(fee, Some(expected));
}

#[test]
fn compute_fee_gmo_fx_returns_none_when_rate_not_configured() {
    let swap_config = GmoFxSwapConfig::default();
    let trade = make_trade("USD_JPY", Direction::Long, dec!(150), dec!(10000));

    let fee = compute_fee(Exchange::GmoFx, &trade, &swap_config);

    assert_eq!(fee, None);
}
