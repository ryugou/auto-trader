use super::*;

fn bar(bid_close: i64, ask_close: i64) -> Bar {
    Bar {
        open_time: 0,
        bid_open: 0,
        bid_high: 0,
        bid_low: 0,
        bid_close,
        ask_open: 0,
        ask_high: 0,
        ask_low: 0,
        ask_close,
    }
}

#[test]
fn mid2_close_sums_bid_and_ask() {
    let b = bar(150_000, 150_004);
    assert_eq!(b.mid2_close(), 300_004);
}

#[test]
fn mid2_to_yen_divides_by_2000() {
    assert_eq!(mid2_to_yen(300_004), 150.002);
}

#[test]
fn milli_to_pips_divides_by_10() {
    assert_eq!(milli_to_pips(25), 2.5);
}

#[test]
fn mid2_to_pips_divides_by_20() {
    assert_eq!(mid2_to_pips(50), 2.5);
}

#[test]
fn pip_constants_match_spec_3() {
    assert_eq!(PIP_MILLI, 10);
    assert_eq!(PIP_MID2, 20);
    assert_eq!(M5_SECS, 300);
}
