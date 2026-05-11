//! Phase 3: Price event routing tests (3.89-3.92).
//!
//! Verifies that PriceEvents are forwarded to the correct channels
//! based on their exchange field, replicating the routing logic in main.rs.

use auto_trader_core::event::PriceEvent;
use auto_trader_core::types::{Candle, Exchange, Pair};
use chrono::Utc;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Helper: create a PriceEvent for a given exchange.
fn make_price_event(exchange: Exchange) -> PriceEvent {
    PriceEvent {
        pair: Pair::new("USD_JPY"),
        exchange,
        candle: Candle {
            pair: Pair::new("USD_JPY"),
            exchange,
            timeframe: "M5".to_string(),
            open: dec!(150.0),
            high: dec!(151.0),
            low: dec!(149.0),
            close: dec!(150.5),
            volume: Some(100),
            best_bid: Some(dec!(150.4)),
            best_ask: Some(dec!(150.6)),
            timestamp: Utc::now(),
        },
        indicators: HashMap::new(),
        timestamp: Utc::now(),
    }
}

/// Replicate the routing logic from main.rs:
/// - Oanda -> price_monitor_tx
/// - BitflyerCfd | GmoFx -> crypto_price_tx
async fn route_event(
    event: &PriceEvent,
    price_monitor_tx: &mpsc::Sender<PriceEvent>,
    crypto_price_tx: &mpsc::Sender<PriceEvent>,
) -> Result<(), &'static str> {
    if event.exchange == Exchange::Oanda {
        price_monitor_tx
            .send(event.clone())
            .await
            .map_err(|_| "FX position monitor channel closed")?;
    }
    if event.exchange == Exchange::BitflyerCfd || event.exchange == Exchange::GmoFx {
        crypto_price_tx
            .send(event.clone())
            .await
            .map_err(|_| "position monitor channel closed")?;
    }
    Ok(())
}

// ─── 3.89: Oanda -> price_monitor_tx ─────────────────────────────────────

#[tokio::test]
async fn oanda_event_routes_to_fx_channel() {
    let (price_monitor_tx, mut price_monitor_rx) = mpsc::channel::<PriceEvent>(16);
    let (crypto_price_tx, mut crypto_price_rx) = mpsc::channel::<PriceEvent>(16);

    let event = make_price_event(Exchange::Oanda);
    route_event(&event, &price_monitor_tx, &crypto_price_tx)
        .await
        .expect("routing should succeed");

    // Oanda event should arrive on the FX channel
    let received = price_monitor_rx
        .try_recv()
        .expect("should receive on FX channel");
    assert_eq!(received.exchange, Exchange::Oanda);

    // Should NOT be on the crypto channel
    assert!(
        crypto_price_rx.try_recv().is_err(),
        "Oanda event must not appear on crypto channel"
    );
}

// ─── 3.90: BitflyerCfd -> crypto_price_tx ────────────────────────────────

#[tokio::test]
async fn bitflyer_event_routes_to_crypto_channel() {
    let (price_monitor_tx, mut price_monitor_rx) = mpsc::channel::<PriceEvent>(16);
    let (crypto_price_tx, mut crypto_price_rx) = mpsc::channel::<PriceEvent>(16);

    let event = make_price_event(Exchange::BitflyerCfd);
    route_event(&event, &price_monitor_tx, &crypto_price_tx)
        .await
        .expect("routing should succeed");

    // BitflyerCfd should arrive on the crypto channel
    let received = crypto_price_rx
        .try_recv()
        .expect("should receive on crypto channel");
    assert_eq!(received.exchange, Exchange::BitflyerCfd);

    // Should NOT be on the FX channel
    assert!(
        price_monitor_rx.try_recv().is_err(),
        "BitflyerCfd event must not appear on FX channel"
    );
}

// ─── 3.91: GmoFx -> crypto_price_tx ─────────────────────────────────────

#[tokio::test]
async fn gmo_fx_event_routes_to_crypto_channel() {
    let (price_monitor_tx, mut price_monitor_rx) = mpsc::channel::<PriceEvent>(16);
    let (crypto_price_tx, mut crypto_price_rx) = mpsc::channel::<PriceEvent>(16);

    let event = make_price_event(Exchange::GmoFx);
    route_event(&event, &price_monitor_tx, &crypto_price_tx)
        .await
        .expect("routing should succeed");

    // GmoFx should arrive on the crypto channel
    let received = crypto_price_rx
        .try_recv()
        .expect("should receive on crypto channel");
    assert_eq!(received.exchange, Exchange::GmoFx);

    // Should NOT be on the FX channel
    assert!(
        price_monitor_rx.try_recv().is_err(),
        "GmoFx event must not appear on FX channel"
    );
}

// ─── 3.92: Channel closed -> graceful stop ───────────────────────────────

#[tokio::test]
async fn channel_closed_is_detected_gracefully() {
    let (price_monitor_tx, price_monitor_rx) = mpsc::channel::<PriceEvent>(16);
    let (crypto_price_tx, crypto_price_rx) = mpsc::channel::<PriceEvent>(16);

    // Drop receivers to simulate shutdown
    drop(price_monitor_rx);
    drop(crypto_price_rx);

    // Verify sender.is_closed() returns true
    assert!(
        price_monitor_tx.is_closed(),
        "FX channel sender must report closed after receiver is dropped"
    );
    assert!(
        crypto_price_tx.is_closed(),
        "crypto channel sender must report closed after receiver is dropped"
    );

    // Verify send fails gracefully (SendError, not panic)
    let event = make_price_event(Exchange::Oanda);
    let result = price_monitor_tx.send(event.clone()).await;
    assert!(result.is_err(), "send to closed FX channel must return Err");

    let event = make_price_event(Exchange::BitflyerCfd);
    let result = crypto_price_tx.send(event.clone()).await;
    assert!(
        result.is_err(),
        "send to closed crypto channel must return Err"
    );
}
