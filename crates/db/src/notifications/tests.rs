use auto_trader_core::types::{Direction, Exchange, ExitReason, Pair, TradeStatus};
use rust_decimal_macros::dec;

use super::*;

#[test]
fn exit_reason_as_str_no_quotes() {
    let s = ExitReason::SlHit.as_str();
    assert!(!s.starts_with('"'));
    assert!(!s.ends_with('"'));
    assert!(!s.is_empty());
}

fn minimal_open_trade() -> Trade {
    Trade {
        id: Uuid::new_v4(),
        account_id: Uuid::new_v4(),
        strategy_name: "test_strategy".to_string(),
        pair: Pair::new("FX_BTC_JPY"),
        exchange: Exchange::BitflyerCfd,
        direction: Direction::Long,
        entry_price: dec!(100),
        exit_price: None,
        stop_loss: dec!(90),
        take_profit: None,
        quantity: dec!(0.01),
        leverage: dec!(2),
        fees: Decimal::ZERO,
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

#[sqlx::test(migrations = "../../migrations")]
async fn insert_trade_closed_rejects_trade_missing_exit_price(pool: PgPool) {
    let trade = minimal_open_trade();
    let err = insert_trade_closed(&pool, &trade)
        .await
        .expect_err("missing exit_price must be rejected");
    assert!(
        err.to_string().contains("exit_price"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn insert_trade_closed_rejects_trade_missing_pnl_amount(pool: PgPool) {
    let mut trade = minimal_open_trade();
    trade.exit_price = Some(dec!(110));
    let err = insert_trade_closed(&pool, &trade)
        .await
        .expect_err("missing pnl_amount must be rejected");
    assert!(
        err.to_string().contains("pnl_amount"),
        "unexpected error: {err}"
    );
}

#[sqlx::test(migrations = "../../migrations")]
async fn insert_trade_closed_rejects_trade_missing_exit_reason(pool: PgPool) {
    let mut trade = minimal_open_trade();
    trade.exit_price = Some(dec!(110));
    trade.pnl_amount = Some(dec!(10));
    let err = insert_trade_closed(&pool, &trade)
        .await
        .expect_err("missing exit_reason must be rejected");
    assert!(
        err.to_string().contains("exit_reason"),
        "unexpected error: {err}"
    );
}
