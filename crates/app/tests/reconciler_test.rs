use auto_trader::tasks::reconciler::{DbOpen, ExchangeOpen, compute_diff};
use rust_decimal_macros::dec;
use uuid::Uuid;

#[test]
fn no_diff_when_db_and_exchange_match() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let diff = compute_diff(&db, &exch);
    assert!(diff.db_orphan.is_empty());
    assert!(diff.exchange_orphan.is_empty());
    assert!(diff.quantity_mismatch.is_empty());
}

#[test]
fn detects_db_orphan_when_exchange_lacks_position() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch: Vec<ExchangeOpen> = vec![];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.db_orphan, vec![trade_id]);
}

#[test]
fn detects_exchange_orphan_when_db_lacks_position() {
    let db: Vec<DbOpen> = vec![];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "short".into(),
        quantity: dec!(0.02),
    }];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.exchange_orphan.len(), 1);
    assert_eq!(diff.exchange_orphan[0].pair, "FX_BTC_JPY");
    assert_eq!(diff.exchange_orphan[0].quantity, dec!(0.02));
}

#[test]
fn detects_quantity_mismatch_same_direction() {
    let trade_id = Uuid::new_v4();
    let db = vec![DbOpen {
        trade_id,
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.01),
    }];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.02),
    }];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.quantity_mismatch.len(), 1);
    let m = &diff.quantity_mismatch[0];
    assert_eq!(m.trade_id, trade_id);
    assert_eq!(m.db_qty, dec!(0.01));
    assert_eq!(m.exchange_qty, dec!(0.02));
}
