use super::*;

fn flat(open_time: i64, price: i64) -> Kline {
    Kline {
        open_time,
        open: price,
        high: price,
        low: price,
        close: price,
    }
}

#[test]
fn matching_open_time_joins_into_one_bar() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![flat(1_000, 149_610)];

    let outcome = join_and_validate(&bid, &ask);

    assert_eq!(outcome.bars.len(), 1);
    assert_eq!(outcome.one_sided, 0);
    assert_eq!(outcome.invalid, 0);
    assert_eq!(
        outcome.bars[0],
        Bar {
            open_time: 1_000,
            bid_open: 149_600,
            bid_high: 149_600,
            bid_low: 149_600,
            bid_close: 149_600,
            ask_open: 149_610,
            ask_high: 149_610,
            ask_low: 149_610,
            ask_close: 149_610,
        }
    );
}

#[test]
fn duplicate_open_time_on_bid_side_is_invalid_and_not_saved() {
    let bid = vec![
        flat(1_000, 149_600),
        flat(1_000, 149_601),
        flat(2_000, 149_605),
    ];
    let ask = vec![flat(1_000, 149_610), flat(2_000, 149_615)];

    let outcome = join_and_validate(&bid, &ask);

    assert_eq!(outcome.bars.len(), 1);
    assert_eq!(outcome.bars[0].open_time, 2_000);
    assert_eq!(outcome.invalid, 1);
    assert_eq!(outcome.one_sided, 0);
}

#[test]
fn duplicate_open_time_on_ask_side_only_is_invalid_not_one_sided() {
    let bid: Vec<Kline> = vec![];
    let ask = vec![flat(1_000, 149_610), flat(1_000, 149_611)];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
    assert_eq!(outcome.one_sided, 0);
}

#[test]
fn bid_only_and_ask_only_bars_are_counted_one_sided_and_excluded() {
    let bid = vec![flat(1_000, 149_600), flat(2_000, 149_605)];
    let ask = vec![flat(1_000, 149_610), flat(3_000, 149_615)];

    let outcome = join_and_validate(&bid, &ask);

    assert_eq!(outcome.bars.len(), 1);
    assert_eq!(outcome.one_sided, 2, "2000 (bid-only) + 3000 (ask-only)");
    assert_eq!(outcome.invalid, 0);
}

#[test]
fn zero_price_is_invalid() {
    let bid = vec![Kline {
        open_time: 1_000,
        open: 0,
        high: 149_600,
        low: 149_600,
        close: 149_600,
    }];
    let ask = vec![flat(1_000, 149_610)];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
    assert_eq!(outcome.one_sided, 0);
}

#[test]
fn negative_price_is_invalid() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![Kline {
        open_time: 1_000,
        open: 149_610,
        high: 149_610,
        low: 149_610,
        close: -10,
    }];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn high_below_max_of_open_and_close_is_invalid() {
    let bid = vec![Kline {
        open_time: 1_000,
        open: 149_600,
        high: 149_605,
        low: 149_550,
        close: 149_650,
    }];
    let ask = vec![flat(1_000, 149_700)];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn low_above_min_of_open_and_close_is_invalid() {
    let bid = vec![Kline {
        open_time: 1_000,
        open: 149_600,
        high: 149_650,
        low: 149_620,
        close: 149_550,
    }];
    let ask = vec![flat(1_000, 149_700)];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn ask_open_below_bid_open_is_invalid() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![Kline {
        open_time: 1_000,
        open: 149_590,
        high: 149_610,
        low: 149_590,
        close: 149_610,
    }];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn ask_high_below_bid_high_is_invalid() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![Kline {
        open_time: 1_000,
        open: 149_610,
        high: 149_590,
        low: 149_590,
        close: 149_610,
    }];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn ask_low_below_bid_low_is_invalid() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![Kline {
        open_time: 1_000,
        open: 149_610,
        high: 149_610,
        low: 149_590,
        close: 149_610,
    }];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn ask_close_below_bid_close_is_invalid() {
    let bid = vec![flat(1_000, 149_600)];
    let ask = vec![Kline {
        open_time: 1_000,
        open: 149_610,
        high: 149_610,
        low: 149_590,
        close: 149_590,
    }];

    let outcome = join_and_validate(&bid, &ask);

    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.invalid, 1);
}

#[test]
fn result_is_sorted_by_open_time_ascending_regardless_of_input_order() {
    let bid = vec![
        flat(3_000, 149_620),
        flat(1_000, 149_600),
        flat(2_000, 149_610),
    ];
    let ask = vec![
        flat(2_000, 149_615),
        flat(3_000, 149_625),
        flat(1_000, 149_605),
    ];

    let outcome = join_and_validate(&bid, &ask);

    assert_eq!(
        outcome.bars.iter().map(|b| b.open_time).collect::<Vec<_>>(),
        vec![1_000, 2_000, 3_000]
    );
}

#[test]
fn empty_inputs_produce_empty_outcome_without_panicking() {
    let outcome = join_and_validate(&[], &[]);
    assert!(outcome.bars.is_empty());
    assert_eq!(outcome.one_sided, 0);
    assert_eq!(outcome.invalid, 0);
}

#[test]
fn decimal_str_to_milli_accepts_exact_three_decimal_places() {
    assert_eq!(decimal_str_to_milli("149.605"), Some(149_605));
}

#[test]
fn decimal_str_to_milli_rejects_values_that_do_not_scale_to_an_integer() {
    assert_eq!(decimal_str_to_milli("149.6055"), None);
}

#[test]
fn decimal_str_to_milli_rejects_non_numeric_strings() {
    assert_eq!(decimal_str_to_milli("not-a-number"), None);
}

fn raw_with_open_time(open_time: &str) -> KlineRaw {
    KlineRaw {
        open_time: open_time.to_string(),
        open: "149.605".to_string(),
        high: "149.612".to_string(),
        low: "149.601".to_string(),
        close: "149.610".to_string(),
    }
}

#[test]
fn parse_kline_converts_whole_second_open_time_to_epoch_secs() {
    let k = parse_kline(&raw_with_open_time("1698451200000")).expect("whole seconds parse");
    assert_eq!(k.open_time, 1_698_451_200);
}

#[test]
fn parse_kline_rejects_open_time_with_sub_second_remainder() {
    assert_eq!(parse_kline(&raw_with_open_time("1698451200500")), None);
}

#[test]
fn parse_kline_rejects_negative_open_time() {
    assert_eq!(parse_kline(&raw_with_open_time("-1000")), None);
}

#[test]
fn parse_klines_skips_unparseable_entries_and_keeps_the_rest() {
    let raw = vec![
        raw_with_open_time("1698451200000"),
        raw_with_open_time("1698451200500"),
    ];
    let klines = parse_klines(&raw, "20231028", PriceType::Bid);
    assert_eq!(klines.len(), 1);
    assert_eq!(klines[0].open_time, 1_698_451_200);
}

#[test]
fn gmo_kline_min_date_is_2023_10_28() {
    assert_eq!(
        gmo_kline_min_date(),
        NaiveDate::from_ymd_opt(2023, 10, 28).unwrap()
    );
}

#[test]
fn new_client_uses_one_second_interval_and_2_4_8_second_retries() {
    let client = GmoKlineClient::new("http://x");
    assert_eq!(client.min_interval, Duration::from_secs(1));
    assert_eq!(
        client.retry_delays,
        vec![
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8)
        ]
    );
}
