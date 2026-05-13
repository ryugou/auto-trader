//! Phase 4: External API integration tests.
//!
//! These tests connect to REAL external services and are gated behind
//! `--features external-api`. They validate that our parsing logic handles
//! real-world API responses (including maintenance / market-closed states).
//!
//! Run with:
//! ```bash
//! cargo test -p auto-trader-integration-tests --features external-api --test phase4_external
//! ```

#![cfg(feature = "external-api")]

use std::time::Duration;

// ─── GMO FX ────────────────────────────────────────────────────────────────

mod gmo_fx {
    use super::*;
    use serde::Deserialize;

    const TICKER_URL: &str = "https://forex-api.coin.z.com/public/v1/ticker";

    #[derive(Debug, Deserialize)]
    struct TickerResponse {
        status: i32,
        #[serde(default)]
        data: Vec<TickerData>,
    }

    #[derive(Debug, Deserialize)]
    struct TickerData {
        symbol: String,
        ask: String,
        bid: String,
        timestamp: String,
        status: String,
    }

    #[tokio::test]
    async fn ticker_fetch_and_parse() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");

        let resp = client
            .get(TICKER_URL)
            .send()
            .await
            .expect("GMO FX ticker request failed");

        assert!(
            resp.status().is_success(),
            "GMO FX ticker returned HTTP {}",
            resp.status()
        );

        let ticker: TickerResponse = resp.json().await.expect("GMO FX ticker JSON parse failed");

        // status=0 is normal, status=5 is maintenance — both are valid
        assert!(
            ticker.status == 0 || ticker.status == 5,
            "GMO FX: unexpected status {} (expected 0 or 5)",
            ticker.status
        );

        if ticker.status == 5 {
            println!("GMO FX: API is in maintenance mode (status=5), data is empty — OK");
            assert!(
                ticker.data.is_empty(),
                "GMO FX: maintenance mode should have empty data, got {} items",
                ticker.data.len()
            );
            return;
        }

        // status=0 — verify expected symbols are present
        println!(
            "GMO FX: ticker returned {} symbols (status=0)",
            ticker.data.len()
        );

        let symbols: Vec<&str> = ticker.data.iter().map(|d| d.symbol.as_str()).collect();
        assert!(
            symbols.contains(&"USD_JPY"),
            "GMO FX: USD_JPY not found in ticker data. symbols={symbols:?}"
        );
        assert!(
            symbols.contains(&"EUR_USD"),
            "GMO FX: EUR_USD not found in ticker data. symbols={symbols:?}"
        );
    }

    #[tokio::test]
    async fn market_status_detection() {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");

        let resp = client
            .get(TICKER_URL)
            .send()
            .await
            .expect("GMO FX ticker request failed");

        let ticker: TickerResponse = resp.json().await.expect("GMO FX ticker JSON parse failed");

        if ticker.status == 5 {
            println!("GMO FX: maintenance mode — market status detection skipped");
            return;
        }

        assert_eq!(
            ticker.status, 0,
            "GMO FX: unexpected status {}",
            ticker.status
        );

        for item in &ticker.data {
            match item.status.as_str() {
                "OPEN" => {
                    // Verify bid/ask are parseable decimals
                    let bid: f64 = item.bid.parse().unwrap_or_else(|e| {
                        panic!("GMO FX {}: invalid bid '{}': {e}", item.symbol, item.bid)
                    });
                    let ask: f64 = item.ask.parse().unwrap_or_else(|e| {
                        panic!("GMO FX {}: invalid ask '{}': {e}", item.symbol, item.ask)
                    });
                    assert!(
                        bid > 0.0,
                        "GMO FX {}: bid must be positive, got {bid}",
                        item.symbol
                    );
                    assert!(
                        ask > 0.0,
                        "GMO FX {}: ask must be positive, got {ask}",
                        item.symbol
                    );
                    assert!(
                        ask >= bid,
                        "GMO FX {}: ask ({ask}) must be >= bid ({bid})",
                        item.symbol
                    );

                    // Verify timestamp is parseable as RFC3339
                    chrono::DateTime::parse_from_rfc3339(&item.timestamp).unwrap_or_else(|e| {
                        panic!(
                            "GMO FX {}: invalid timestamp '{}': {e}",
                            item.symbol, item.timestamp
                        )
                    });

                    println!(
                        "GMO FX {}: OPEN  bid={} ask={}",
                        item.symbol, item.bid, item.ask
                    );
                }
                "CLOSE" => {
                    println!(
                        "GMO FX {}: CLOSE (market closed — normal for weekends/holidays)",
                        item.symbol
                    );
                }
                other => {
                    panic!(
                        "GMO FX {}: unknown status '{}' — expected OPEN or CLOSE",
                        item.symbol, other
                    );
                }
            }
        }

        println!(
            "GMO FX: observed {} symbols, statuses: {:?}",
            ticker.data.len(),
            ticker
                .data
                .iter()
                .map(|d| format!("{}={}", d.symbol, d.status))
                .collect::<Vec<_>>()
        );
    }
}

// ─── BitFlyer WebSocket helper ─────────────────────────────────────────────

const BITFLYER_WS_URL: &str = "wss://ws.lightstream.bitflyer.com/json-rpc";
const BITFLYER_CHANNEL: &str = "lightning_ticker_FX_BTC_JPY";

/// Connect to BitFlyer WS, subscribe to the ticker channel, and return the
/// first `channelMessage` tick as a JSON value. Returns `None` if the
/// connection or receive fails (network issue / market inactive).
async fn connect_bitflyer_and_receive_tick() -> Option<serde_json::Value> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let ws_result =
        tokio::time::timeout(Duration::from_secs(10), connect_async(BITFLYER_WS_URL)).await;

    let (ws, _) = match ws_result {
        Ok(Ok(conn)) => conn,
        Ok(Err(e)) => {
            println!("BitFlyer WS: connection failed ({e}) — SKIPPED (network issue)");
            return None;
        }
        Err(_) => {
            println!("BitFlyer WS: connection timed out — SKIPPED (network issue)");
            return None;
        }
    };

    let (mut write, mut read) = ws.split();

    let subscribe = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "subscribe",
        "params": { "channel": BITFLYER_CHANNEL }
    });
    write
        .send(Message::Text(subscribe.to_string()))
        .await
        .expect("failed to send subscribe message");

    println!("BitFlyer WS: connected and subscribed to {BITFLYER_CHANNEL}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        if tokio::time::Instant::now() >= deadline {
            println!("BitFlyer WS: no tick received within 30s — SKIPPED (market may be inactive)");
            return None;
        }

        let msg = match tokio::time::timeout_at(deadline, read.next()).await {
            Ok(Some(Ok(msg))) => msg,
            Ok(Some(Err(e))) => {
                println!("BitFlyer WS: read error ({e}) — SKIPPED");
                return None;
            }
            Ok(None) => {
                println!("BitFlyer WS: stream ended unexpectedly — SKIPPED");
                return None;
            }
            Err(_) => {
                println!(
                    "BitFlyer WS: no tick received within 30s — SKIPPED (market may be inactive)"
                );
                return None;
            }
        };

        let Message::Text(text) = msg else {
            continue;
        };

        let parsed: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if parsed.get("method").and_then(|m| m.as_str()) != Some("channelMessage") {
            continue;
        }

        let params = parsed
            .get("params")
            .expect("channelMessage must have params");
        let message = params
            .get("message")
            .expect("params must have message")
            .clone();

        return Some(message);
    }
}

// ─── BitFlyer WebSocket ────────────────────────────────────────────────────

mod bitflyer_ws {
    use super::*;

    #[tokio::test]
    async fn ws_connection_and_tick_receive() {
        let message = match connect_bitflyer_and_receive_tick().await {
            Some(m) => m,
            None => {
                println!("BitFlyer WS: tick receive returned None — SKIPPED");
                return;
            }
        };

        // Verify required fields
        assert!(
            message.get("ltp").is_some(),
            "BitFlyer tick must contain 'ltp'"
        );
        assert!(
            message.get("best_bid").is_some(),
            "BitFlyer tick must contain 'best_bid'"
        );
        assert!(
            message.get("best_ask").is_some(),
            "BitFlyer tick must contain 'best_ask'"
        );

        let ltp = message["ltp"].as_f64().expect("ltp must be numeric");
        let best_bid = message["best_bid"]
            .as_f64()
            .expect("best_bid must be numeric");
        let best_ask = message["best_ask"]
            .as_f64()
            .expect("best_ask must be numeric");

        assert!(ltp > 0.0, "ltp must be positive, got {ltp}");
        assert!(best_bid > 0.0, "best_bid must be positive, got {best_bid}");
        assert!(best_ask > 0.0, "best_ask must be positive, got {best_ask}");

        println!("BitFlyer WS: received tick — ltp={ltp}, bid={best_bid}, ask={best_ask}");
        println!("BitFlyer WS: successfully received 1 tick");
    }

    /// 4.5: Connect, receive tick, disconnect, reconnect, receive another tick.
    #[tokio::test]
    async fn ws_disconnect_and_reconnect() {
        // First connection + tick
        let tick1 = match connect_bitflyer_and_receive_tick().await {
            Some(m) => m,
            None => {
                println!("BitFlyer WS: first connection returned None — SKIPPED");
                return;
            }
        };
        let ltp1 = tick1["ltp"].as_f64().expect("ltp must be numeric");
        assert!(ltp1 > 0.0, "first tick ltp must be positive");
        println!("BitFlyer WS reconnect: tick1 ltp={ltp1}");

        // The first connection is dropped here (disconnect).
        // Now reconnect and receive another tick.
        let tick2 = match connect_bitflyer_and_receive_tick().await {
            Some(m) => m,
            None => {
                println!(
                    "BitFlyer WS: reconnection returned None — SKIPPED (market may be inactive)"
                );
                return;
            }
        };
        let ltp2 = tick2["ltp"].as_f64().expect("ltp must be numeric");
        assert!(ltp2 > 0.0, "second tick ltp must be positive");
        println!("BitFlyer WS reconnect: tick2 ltp={ltp2}");

        println!("BitFlyer WS: disconnect + reconnect succeeded");
    }
}

// ─── CandleBuilder with real tick ──────────────────────────────────────────

mod candle_builder_real_tick {
    use super::*;
    use auto_trader_core::types::{Exchange, Pair};
    use auto_trader_market::candle_builder::CandleBuilder;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[tokio::test]
    async fn candle_builder_with_real_tick() {
        let message = match connect_bitflyer_and_receive_tick().await {
            Some(m) => m,
            None => {
                println!("BitFlyer WS: tick receive returned None — SKIPPED");
                return;
            }
        };

        let ltp = message["ltp"].as_f64().expect("ltp must be numeric");
        let best_bid = message["best_bid"]
            .as_f64()
            .expect("best_bid must be numeric");
        let best_ask = message["best_ask"]
            .as_f64()
            .expect("best_ask must be numeric");
        let volume = message["volume"].as_f64().unwrap_or(0.0);
        let timestamp_str = message["timestamp"]
            .as_str()
            .expect("timestamp must be a string");
        let ts = chrono::DateTime::parse_from_rfc3339(timestamp_str)
            .expect("timestamp must be RFC3339")
            .with_timezone(&chrono::Utc);

        let price = Decimal::from_str(&ltp.to_string()).unwrap();
        let size = Decimal::from_str(&volume.to_string()).unwrap();
        let bid = Some(Decimal::from_str(&best_bid.to_string()).unwrap());
        let ask = Some(Decimal::from_str(&best_ask.to_string()).unwrap());

        let mut builder = CandleBuilder::new(
            Pair::new("FX_BTC_JPY"),
            Exchange::BitflyerCfd,
            "M5".to_string(),
        );

        // Feed into CandleBuilder — it won't emit a candle (same M5 period)
        // but internal state should be populated
        let _candle = builder.on_tick(price, size, ts, bid, ask);

        // Verify internal state via try_complete with a far-future timestamp
        // that forces the candle to complete
        let far_future = ts + chrono::Duration::minutes(10);
        let candle = builder.try_complete(far_future, bid, ask);

        assert!(
            candle.is_some(),
            "CandleBuilder should produce a candle when try_complete is called past period end"
        );

        let c = candle.unwrap();
        assert_eq!(c.pair.0, "FX_BTC_JPY");
        assert_eq!(c.exchange, Exchange::BitflyerCfd);
        assert_eq!(c.timeframe, "M5");
        assert!(c.open > Decimal::ZERO, "open must be positive: {}", c.open);
        assert!(c.high >= c.open, "high must be >= open");
        assert!(c.low <= c.open, "low must be <= open");
        assert!(c.close > Decimal::ZERO, "close must be positive");
        assert!(
            c.best_bid.is_some(),
            "best_bid should be set from real tick"
        );
        assert!(
            c.best_ask.is_some(),
            "best_ask should be set from real tick"
        );

        println!(
            "CandleBuilder: built candle from real tick — O={} H={} L={} C={} bid={:?} ask={:?}",
            c.open, c.high, c.low, c.close, c.best_bid, c.best_ask
        );
    }
}

// ─── Vegapunk gRPC ─────────────────────────────────────────────────────────

mod vegapunk {
    use super::*;
    use auto_trader_core::vegapunk_port::{SearchMode, VegapunkApi};

    #[tokio::test]
    async fn vegapunk_connection_and_search() {
        let endpoint = std::env::var("VEGAPUNK_ENDPOINT")
            .unwrap_or_else(|_| "http://vegapunk.local:6840".to_string());
        let token = std::env::var("VEGAPUNK_AUTH_TOKEN").ok();

        // Try to connect — Vegapunk may not be running
        let connect_result = tokio::time::timeout(
            Duration::from_secs(10),
            auto_trader_vegapunk::client::VegapunkClient::connect(
                &endpoint,
                "fx-trading",
                token.as_deref(),
            ),
        )
        .await;

        let client = match connect_result {
            Ok(Ok(client)) => {
                println!("Vegapunk: connected to {endpoint}");
                client
            }
            Ok(Err(e)) => {
                println!(
                    "Vegapunk: connection to {endpoint} failed ({e}) — SKIPPED (Vegapunk may not be running)"
                );
                return;
            }
            Err(_) => {
                println!(
                    "Vegapunk: connection to {endpoint} timed out — SKIPPED (Vegapunk may not be running)"
                );
                return;
            }
        };

        // Try a search — even an empty result is fine.
        // Unauthenticated errors when VEGAPUNK_AUTH_TOKEN is unset are
        // expected (gRPC connect succeeds without auth, but RPCs require it).
        let search_result = tokio::time::timeout(
            Duration::from_secs(15),
            client.search("test query", SearchMode::Local, 5),
        )
        .await;

        match search_result {
            Ok(Ok(response)) => {
                println!(
                    "Vegapunk: search returned {} results (search_id={})",
                    response.hits.len(),
                    response.search_id
                );
            }
            Ok(Err(e)) => {
                let msg = e.to_string();
                if msg.contains("Unauthenticated") && token.is_none() {
                    println!(
                        "Vegapunk: search returned Unauthenticated (VEGAPUNK_AUTH_TOKEN not set) — SKIPPED"
                    );
                    return;
                }
                panic!("Vegapunk: search failed after successful connection ({e})");
            }
            Err(_) => {
                panic!("Vegapunk: search timed out after 15s despite successful connection");
            }
        }
    }

    /// Helper: connect to Vegapunk, returning None if unavailable or unauthenticated.
    async fn connect_vegapunk() -> Option<auto_trader_vegapunk::client::VegapunkClient> {
        let endpoint = std::env::var("VEGAPUNK_ENDPOINT")
            .unwrap_or_else(|_| "http://vegapunk.local:6840".to_string());
        let token = match std::env::var("VEGAPUNK_AUTH_TOKEN") {
            Ok(t) if !t.is_empty() => Some(t),
            _ => {
                println!("Vegapunk: VEGAPUNK_AUTH_TOKEN not set — SKIPPED");
                return None;
            }
        };

        let connect_result = tokio::time::timeout(
            Duration::from_secs(10),
            auto_trader_vegapunk::client::VegapunkClient::connect(
                &endpoint,
                "fx-trading-test",
                token.as_deref(),
            ),
        )
        .await;

        match connect_result {
            Ok(Ok(client)) => {
                println!("Vegapunk: connected to {endpoint}");
                Some(client)
            }
            Ok(Err(e)) => {
                println!("Vegapunk: connection failed ({e}) — SKIPPED");
                None
            }
            Err(_) => {
                println!("Vegapunk: connection timed out — SKIPPED");
                None
            }
        }
    }

    /// 4.8 partial: Vegapunk ingest_raw — ingest text and verify success.
    #[tokio::test]
    async fn vegapunk_ingest_raw() {
        let client = match connect_vegapunk().await {
            Some(c) => c,
            None => return,
        };

        let timestamp = chrono::Utc::now().to_rfc3339();
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            client.ingest_raw(
                "integration test: USD/JPY trending up on strong NFP data",
                "test_event",
                "integration-test",
                &timestamp,
            ),
        )
        .await;

        match result {
            Ok(Ok(())) => {
                println!("Vegapunk ingest: success");
            }
            Ok(Err(e)) => {
                panic!("Vegapunk ingest failed: {e}");
            }
            Err(_) => {
                panic!("Vegapunk ingest timed out after 15s");
            }
        }
    }

    /// 4.8 partial: Vegapunk feedback — search then submit feedback.
    #[tokio::test]
    async fn vegapunk_feedback() {
        let client = match connect_vegapunk().await {
            Some(c) => c,
            None => return,
        };

        // First do a search to get a search_id
        let search_result = tokio::time::timeout(
            Duration::from_secs(15),
            client.search("USD/JPY trend analysis", SearchMode::Local, 3),
        )
        .await;

        let search_id = match search_result {
            Ok(Ok(response)) => {
                println!(
                    "Vegapunk feedback: search returned {} results (search_id={})",
                    response.hits.len(),
                    response.search_id
                );
                response.search_id
            }
            Ok(Err(e)) => {
                println!("Vegapunk feedback: search failed ({e}) — SKIPPED");
                return;
            }
            Err(_) => {
                panic!("Vegapunk feedback: search timed out");
            }
        };

        // Submit feedback on the search
        let feedback_result = tokio::time::timeout(
            Duration::from_secs(15),
            client.feedback(&search_id, 4, "integration test feedback"),
        )
        .await;

        match feedback_result {
            Ok(Ok(())) => {
                println!("Vegapunk feedback: submitted successfully for search_id={search_id}");
            }
            Ok(Err(e)) => {
                panic!("Vegapunk feedback failed: {e}");
            }
            Err(_) => {
                panic!("Vegapunk feedback timed out after 15s");
            }
        }
    }
}

// ─── OANDA (skipped without API key) ──────────────────────────────────────

mod oanda {
    use super::*;

    #[tokio::test]
    async fn oanda_rest_polling() {
        let api_key = match std::env::var("OANDA_API_KEY") {
            Ok(key) if !key.is_empty() => key,
            _ => {
                println!("OANDA: OANDA_API_KEY not set — SKIPPED");
                return;
            }
        };

        let account_id = match std::env::var("OANDA_ACCOUNT_ID") {
            Ok(id) if !id.is_empty() => id,
            _ => {
                println!("SKIPPED: OANDA_ACCOUNT_ID not set");
                return;
            }
        };

        let base_url = std::env::var("OANDA_BASE_URL")
            .unwrap_or_else(|_| "https://api-fxpractice.oanda.com".to_string());

        let client = auto_trader_market::oanda::OandaClient::new(&base_url, &account_id, &api_key)
            .expect("failed to create OANDA client");

        let pair = auto_trader_core::types::Pair::new("USD_JPY");

        let candles_result =
            tokio::time::timeout(Duration::from_secs(30), client.get_candles(&pair, "M5", 5)).await;

        match candles_result {
            Ok(Ok(candles)) => {
                assert!(
                    !candles.is_empty(),
                    "OANDA returned zero candles — expected at least 1 for recent M5"
                );
                println!("OANDA: fetched {} candles for USD_JPY M5", candles.len());
                for c in &candles {
                    assert!(c.open > rust_decimal::Decimal::ZERO);
                    assert!(c.high >= c.low);
                    println!(
                        "  {} O={} H={} L={} C={}",
                        c.timestamp, c.open, c.high, c.low, c.close
                    );
                }
            }
            Ok(Err(e)) => panic!("OANDA candle fetch failed despite valid credentials: {e}"),
            Err(_) => {
                panic!("OANDA: request timed out after 30s despite valid credentials");
            }
        }
    }
}

// ─── Gemini (skipped without API key) ─────────────────────────────────────

mod gemini {
    #[tokio::test]
    async fn gemini_api_connection() {
        let api_key = match std::env::var("GEMINI_API_KEY") {
            Ok(key) if !key.is_empty() => key,
            _ => {
                println!("Gemini: GEMINI_API_KEY not set — SKIPPED");
                return;
            }
        };

        // Simple connectivity check — send a minimal request to the models endpoint
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build HTTP client");

        let resp = client
            .get("https://generativelanguage.googleapis.com/v1beta/models")
            .header("x-goog-api-key", &api_key)
            .send()
            .await;

        match resp {
            Ok(r) => {
                println!("Gemini: API responded with HTTP {}", r.status());
                // 429 (rate limit) is acceptable — proves the API endpoint is reachable
                // and our credentials are valid. The actual generation test is in
                // Phase 3 with MockGemini.
                assert!(
                    r.status().is_success() || r.status().as_u16() == 429,
                    "Gemini: unexpected status {}",
                    r.status()
                );
            }
            Err(e) => {
                panic!("Gemini: request failed with GEMINI_API_KEY set ({e}) — transport error");
            }
        }
    }
}
