//! Phase 3A: SwingLLM V1 strategy signal tests (3.40-3.43).
//!
//! SwingLLMv1 calls KnowledgeStore for context search and Gemini
//! (REST) for trade decisions. These tests use:
//! - MockVegapunkApi + VegapunkKnowledgeStore: lightweight in-process mock
//! - MockGemini: a wiremock HTTP server returning canned Gemini responses
//!
//! Each test constructs a SwingLLMv1 with the mock store and feeds
//! a PriceEvent to trigger a decision.

use auto_trader::knowledge::VegapunkKnowledgeStore;
use auto_trader_core::event::PriceEvent;
use auto_trader_core::knowledge::KnowledgeStore;
use auto_trader_core::strategy::Strategy;
use auto_trader_core::types::{Candle, Direction, Exchange, Pair};
use auto_trader_integration_tests::mocks::gemini::MockGemini;
use auto_trader_integration_tests::mocks::vegapunk::{MockVegapunkApiBuilder, SearchResult};
use auto_trader_strategy::swing_llm::SwingLLMv1;
use chrono::Utc;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;

const PAIR: &str = "USD_JPY";

fn make_price_event(pair: &str, close: Decimal) -> PriceEvent {
    let ts = Utc::now();
    PriceEvent {
        pair: Pair::new(pair),
        exchange: Exchange::GmoFx,
        timestamp: ts,
        candle: Candle {
            pair: Pair::new(pair),
            exchange: Exchange::GmoFx,
            timeframe: "H1".to_string(),
            open: close,
            high: close + dec!(0.050),
            low: close - dec!(0.050),
            close,
            volume: Some(0),
            best_bid: Some(close - dec!(0.005)),
            best_ask: Some(close + dec!(0.005)),
            timestamp: ts,
        },
        indicators: HashMap::new(),
    }
}

async fn create_strategy(gemini_url: &str, store: Arc<dyn KnowledgeStore>) -> SwingLLMv1 {
    SwingLLMv1::new(
        "swing_llm_v1".to_string(),
        vec![Pair::new(PAIR)],
        7, // holding_days_max
        store,
        gemini_url.to_string(),
        "test-api-key".to_string(),
        "gemini-2.0-flash".to_string(),
    )
}

fn make_store(search_text: &str) -> Arc<dyn KnowledgeStore> {
    let mock_api = Arc::new(
        MockVegapunkApiBuilder::new()
            .with_search_results(vec![SearchResult {
                text: search_text.to_string(),
                score: 0.9,
            }])
            .build(),
    );
    Arc::new(VegapunkKnowledgeStore::new(mock_api))
}

// ─── 3.40: Long entry ────────────────────────────────────────────────────

/// Gemini returns action=long with confidence >= 0.6 → Long signal.
#[tokio::test]
async fn swing_llm_long_entry() {
    let store = make_store("USD/JPY bullish trend detected");
    let gemini = MockGemini::start().await;
    gemini
        .swing_signal(r#"{"action":"long","confidence":0.8,"sl_pips":50,"tp_pips":100,"reason":"bullish momentum"}"#)
        .await;

    let mut strategy = create_strategy(&gemini.url(), store).await;
    let event = make_price_event(PAIR, dec!(150.000));
    let signal = strategy.on_price(&event).await;

    assert!(signal.is_some(), "expected Long signal from swing_llm");
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Long);
    assert_eq!(sig.strategy_name, "swing_llm_v1");
    assert_eq!(sig.pair, Pair::new(PAIR));
    // SL and TP should be positive fractions derived from pips
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.take_profit_pct.is_some());
    assert!(sig.take_profit_pct.unwrap() > Decimal::ZERO);
    // Allocation is 0.5 for swing_llm
    assert_eq!(sig.allocation_pct, dec!(0.5));
}

// ─── 3.41: Short entry ───────────────────────────────────────────────────

/// Gemini returns action=short with confidence >= 0.6 → Short signal.
#[tokio::test]
async fn swing_llm_short_entry() {
    let store = make_store("USD/JPY bearish reversal pattern");
    let gemini = MockGemini::start().await;
    gemini
        .swing_signal(r#"{"action":"short","confidence":0.75,"sl_pips":40,"tp_pips":80,"reason":"bearish divergence"}"#)
        .await;

    let mut strategy = create_strategy(&gemini.url(), store).await;
    let event = make_price_event(PAIR, dec!(150.000));
    let signal = strategy.on_price(&event).await;

    assert!(signal.is_some(), "expected Short signal from swing_llm");
    let sig = signal.unwrap();
    assert_eq!(sig.direction, Direction::Short);
    assert!(sig.stop_loss_pct > Decimal::ZERO);
    assert!(sig.take_profit_pct.is_some());
}

// ─── 3.42: no_trade ─────────────────────────────────────────────────────

/// Gemini returns action=none → no signal emitted.
#[tokio::test]
async fn swing_llm_no_trade() {
    let store = make_store("USD/JPY sideways consolidation");
    let gemini = MockGemini::start().await;
    gemini
        .swing_signal(r#"{"action":"none","confidence":0.3,"sl_pips":0,"tp_pips":0,"reason":"no clear direction"}"#)
        .await;

    let mut strategy = create_strategy(&gemini.url(), store).await;
    let event = make_price_event(PAIR, dec!(150.000));
    let signal = strategy.on_price(&event).await;

    assert!(signal.is_none(), "expected no signal for action=none");
}

// ─── 3.43: Invalid response ─────────────────────────────────────────────

/// Gemini returns malformed / non-JSON response → gracefully returns no signal.
#[tokio::test]
async fn swing_llm_invalid_response() {
    let store = make_store("some context");
    let gemini = MockGemini::start().await;
    gemini.invalid_response().await;

    let mut strategy = create_strategy(&gemini.url(), store).await;
    let event = make_price_event(PAIR, dec!(150.000));
    let signal = strategy.on_price(&event).await;

    assert!(
        signal.is_none(),
        "expected no signal when Gemini returns invalid JSON"
    );
}
