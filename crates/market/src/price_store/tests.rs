use chrono::TimeZone;
use rust_decimal::Decimal;

use super::*;

fn key(ex: Exchange, p: &str) -> FeedKey {
    FeedKey::new(ex, Pair::new(p))
}

fn tick(price: i64, ts: DateTime<Utc>) -> LatestTick {
    LatestTick {
        price: Decimal::from(price),
        best_bid: None,
        best_ask: None,
        ts,
    }
}

#[tokio::test]
async fn update_then_get_returns_latest() {
    let store = PriceStore::new(vec![]);
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    store.update(k.clone(), tick(11_500_000, now)).await;
    let got = store.get(&k).await.expect("present");
    assert_eq!(got.price, Decimal::from(11_500_000));
    assert_eq!(got.ts, now);
}

#[tokio::test]
async fn update_overwrites_previous() {
    let store = PriceStore::new(vec![]);
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let t1 = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let t2 = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 30).unwrap();
    store.update(k.clone(), tick(11_500_000, t1)).await;
    store.update(k.clone(), tick(11_600_000, t2)).await;
    let got = store.get(&k).await.unwrap();
    assert_eq!(got.price, Decimal::from(11_600_000));
    assert_eq!(got.ts, t2);
}

#[tokio::test]
async fn update_drops_older_or_equal_timestamp() {
    let store = PriceStore::new(vec![]);
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let newer = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 30).unwrap();
    let older = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    store.update(k.clone(), tick(11_600_000, newer)).await;
    store.update(k.clone(), tick(11_500_000, older)).await;
    let got = store.get(&k).await.unwrap();
    assert_eq!(got.price, Decimal::from(11_600_000));
    assert_eq!(got.ts, newer);
    store.update(k.clone(), tick(11_700_000, newer)).await;
    let got = store.get(&k).await.unwrap();
    assert_eq!(got.price, Decimal::from(11_600_000));
}

#[tokio::test]
async fn health_missing_when_no_tick() {
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let report = store.health_at(now).await;
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].status, FeedStatus::Missing);
    assert_eq!(report[0].last_tick_age_secs, None);
}

#[tokio::test]
async fn health_healthy_when_tick_within_60s() {
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let recent = now - chrono::Duration::seconds(59);
    store.update(k.clone(), tick(11_500_000, recent)).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Healthy);
    assert_eq!(report[0].last_tick_age_secs, Some(59));
}

#[tokio::test]
async fn health_healthy_at_exactly_60s() {
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let exactly_60 = now - chrono::Duration::seconds(60);
    store.update(k.clone(), tick(11_500_000, exactly_60)).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Healthy);
    assert_eq!(report[0].last_tick_age_secs, Some(60));
}

#[tokio::test]
async fn health_stale_at_61s() {
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let old = now - chrono::Duration::seconds(61);
    store.update(k.clone(), tick(11_500_000, old)).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Stale);
    assert_eq!(report[0].last_tick_age_secs, Some(61));
}

#[tokio::test]
async fn health_clamps_future_timestamp_to_zero() {
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    let future = now + chrono::Duration::seconds(5);
    store.update(k.clone(), tick(11_500_000, future)).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Healthy);
    assert_eq!(report[0].last_tick_age_secs, Some(0));
}

#[tokio::test]
async fn last_tick_age_returns_small_for_just_inserted_tick() {
    let store = PriceStore::new(vec![]);
    let k = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let now = chrono::Utc::now();
    store.update(k.clone(), tick(11_500_000, now)).await;
    let age = store
        .last_tick_age(&k.pair)
        .await
        .expect("tick was just inserted");
    assert!(age <= 1, "age should be 0 or 1 second, got {age}");
}

#[tokio::test]
async fn last_tick_age_returns_none_for_unknown_pair() {
    let store = PriceStore::new(vec![]);
    let pair = Pair::new("UNKNOWN_PAIR");
    let age = store.last_tick_age(&pair).await;
    assert!(age.is_none(), "expected None for unknown pair");
}

#[tokio::test]
async fn last_tick_age_for_is_exchange_specific() {
    let store = PriceStore::new(vec![]);
    let bf = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let gmo = key(Exchange::GmoFx, "FX_BTC_JPY");
    let now = chrono::Utc::now();
    let old = now - chrono::Duration::seconds(120);
    store.update(bf.clone(), tick(11_500_000, now)).await;
    store.update(gmo.clone(), tick(11_500_000, old)).await;

    let bf_age = store.last_tick_age_for(&bf).await.expect("bf present");
    let gmo_age = store.last_tick_age_for(&gmo).await.expect("gmo present");

    assert!(
        bf_age < gmo_age,
        "bf age {bf_age} should be less than gmo age {gmo_age}"
    );
    assert!(
        gmo_age >= bf_age + 100,
        "gmo age {gmo_age} should be at least 100s older than bf age {bf_age}"
    );
}

#[tokio::test]
async fn last_tick_age_for_returns_none_for_missing_key() {
    let store = PriceStore::new(vec![]);
    let k = key(Exchange::GmoFx, "USD_JPY");
    let age = store.last_tick_age_for(&k).await;
    assert!(age.is_none());
}

#[tokio::test]
async fn health_market_closed_when_marked() {
    let k = key(Exchange::GmoFx, "USD_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Missing);

    store.mark_market_closed(k.clone()).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::MarketClosed);
}

#[tokio::test]
async fn mark_market_closed_records_recent_timestamp_in_health() {
    let k = key(Exchange::GmoFx, "USD_JPY");
    let store = PriceStore::new(vec![k.clone()]);

    let before = Utc::now();
    store.mark_market_closed(k.clone()).await;
    let after = Utc::now();

    let report = store.health_at(after).await;
    let closed_at = report[0]
        .market_closed_at
        .expect("market_closed_at must be set right after mark_market_closed");
    assert!(
        closed_at >= before && closed_at <= after,
        "closed_at={closed_at} should fall within [{before}, {after}]"
    );
}

#[tokio::test]
async fn health_market_closed_cleared_on_tick() {
    let k = key(Exchange::GmoFx, "USD_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 5, 4, 9, 0, 0).unwrap();

    store.mark_market_closed(k.clone()).await;
    store.update(k.clone(), tick(160_000, now)).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::Healthy);
}

#[tokio::test]
async fn health_market_closed_with_stale_tick() {
    let k = key(Exchange::GmoFx, "USD_JPY");
    let store = PriceStore::new(vec![k.clone()]);
    let old = Utc.with_ymd_and_hms(2026, 5, 1, 20, 0, 0).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();

    store.update(k.clone(), tick(160_000, old)).await;
    store.mark_market_closed(k.clone()).await;
    let report = store.health_at(now).await;
    assert_eq!(report[0].status, FeedStatus::MarketClosed);
    assert!(report[0].last_tick_age_secs.is_some());
}

#[tokio::test]
async fn health_only_reports_expected_feeds() {
    let bf = key(Exchange::BitflyerCfd, "FX_BTC_JPY");
    let oanda = key(Exchange::Oanda, "USD_JPY");
    let store = PriceStore::new(vec![bf.clone()]);
    let now = Utc.with_ymd_and_hms(2026, 4, 8, 12, 0, 0).unwrap();
    store.update(oanda.clone(), tick(15_000, now)).await;
    let report = store.health_at(now).await;
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].exchange, "bitflyer_cfd");
}
