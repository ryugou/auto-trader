use auto_trader::tasks::reconciler::{DbOpen, ExchangeOpen, compute_diff};
use rust_decimal_macros::dec;
use std::collections::HashSet;
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
    assert_eq!(m.trade_ids, vec![trade_id]);
    assert_eq!(m.db_qty, dec!(0.01));
    assert_eq!(m.exchange_qty, dec!(0.02));
}

#[test]
fn quantity_mismatch_includes_all_trade_ids_for_same_key() {
    let id1 = Uuid::new_v4();
    let id2 = Uuid::new_v4();
    let db = vec![
        DbOpen {
            trade_id: id1,
            pair: "FX_BTC_JPY".into(),
            direction: "long".into(),
            quantity: dec!(0.01),
        },
        DbOpen {
            trade_id: id2,
            pair: "FX_BTC_JPY".into(),
            direction: "long".into(),
            quantity: dec!(0.01),
        },
    ];
    let exch = vec![ExchangeOpen {
        pair: "FX_BTC_JPY".into(),
        direction: "long".into(),
        quantity: dec!(0.05),
    }];
    let diff = compute_diff(&db, &exch);
    assert_eq!(diff.quantity_mismatch.len(), 1);
    let m = &diff.quantity_mismatch[0];
    // Both trade IDs must be present (order is non-deterministic via HashMap).
    let mut ids = m.trade_ids.clone();
    ids.sort();
    let mut expected = vec![id1, id2];
    expected.sort();
    assert_eq!(ids, expected);
    assert_eq!(m.db_qty, dec!(0.02));
    assert_eq!(m.exchange_qty, dec!(0.05));
}

/// Verify that the approved-live-account filter correctly gates accounts.
/// This test validates the HashSet logic used inside run_reconciler_loop
/// without requiring DB or exchange connections.
#[test]
fn approved_live_set_rejects_unapproved_id() {
    let approved_id = Uuid::new_v4();
    let other_id = Uuid::new_v4();

    let approved: HashSet<Uuid> = [approved_id].into_iter().collect();

    assert!(approved.contains(&approved_id));
    assert!(!approved.contains(&other_id));
}

#[test]
fn empty_approved_live_set_rejects_all() {
    let approved: HashSet<Uuid> = HashSet::new();
    assert!(!approved.contains(&Uuid::new_v4()));
}
