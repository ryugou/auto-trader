use futures_util::SinkExt;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// Tick data for the mock BitFlyer WebSocket server.
#[derive(Clone, Debug)]
pub struct TickData {
    pub ltp: i64,
    pub best_bid: i64,
    pub best_ask: i64,
    pub volume: f64,
    pub timestamp: String,
}

impl TickData {
    /// Create a simple tick with default volume and a fixed timestamp.
    pub fn new(ltp: i64, best_bid: i64, best_ask: i64) -> Self {
        Self {
            ltp,
            best_bid,
            best_ask,
            volume: 0.01,
            timestamp: "2026-04-28T00:00:00.000".to_string(),
        }
    }

    /// Create a tick with a custom timestamp.
    pub fn with_timestamp(mut self, ts: &str) -> Self {
        self.timestamp = ts.to_string();
        self
    }

    /// Create a tick with a custom volume.
    pub fn with_volume(mut self, v: f64) -> Self {
        self.volume = v;
        self
    }
}

/// Scenario that the mock server will execute for each connected client.
#[derive(Clone)]
enum Scenario {
    /// Send all ticks then keep the connection open.
    NormalTicks {
        pair: String,
        ticks: Vec<TickData>,
        interval: std::time::Duration,
    },
    /// Send `n` ticks then close the connection.
    DisconnectAfter {
        pair: String,
        ticks: Vec<TickData>,
        n: usize,
        interval: std::time::Duration,
    },
    /// Send a single invalid (non-JSON) message.
    InvalidMessage,
}

/// A mock WebSocket server that simulates the BitFlyer lightning WebSocket API.
///
/// Starts on an ephemeral port and runs in a background tokio task.
/// Cleaned up when dropped (the task is aborted).
pub struct MockBitflyerWs {
    url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockBitflyerWs {
    /// Start a mock that sends a series of ticker messages at the given interval.
    pub async fn normal_ticks(pair: &str, ticks: Vec<TickData>) -> Self {
        Self::normal_ticks_with_interval(pair, ticks, std::time::Duration::from_millis(10)).await
    }

    /// Start a mock that sends ticks at a configurable interval.
    pub async fn normal_ticks_with_interval(
        pair: &str,
        ticks: Vec<TickData>,
        interval: std::time::Duration,
    ) -> Self {
        Self::start(Scenario::NormalTicks {
            pair: pair.to_string(),
            ticks,
            interval,
        })
        .await
    }

    /// Start a mock that sends `n` ticks then closes the connection.
    pub async fn disconnect_after(pair: &str, ticks: Vec<TickData>, n: usize) -> Self {
        Self::start(Scenario::DisconnectAfter {
            pair: pair.to_string(),
            ticks,
            n,
            interval: std::time::Duration::from_millis(10),
        })
        .await
    }

    /// Start a mock that sends a single invalid (non-JSON) message.
    pub async fn invalid_message() -> Self {
        Self::start(Scenario::InvalidMessage).await
    }

    /// Returns the `ws://127.0.0.1:{port}` URL for connecting to this mock.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    async fn start(scenario: Scenario) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("ws://127.0.0.1:{port}");

        let handle = tokio::spawn(async move {
            // Accept connections in a loop so we can handle reconnects in tests.
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let scenario = scenario.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = accept_async(stream).await else {
                        return;
                    };

                    match scenario {
                        Scenario::NormalTicks {
                            pair,
                            ticks,
                            interval,
                        } => {
                            for tick in &ticks {
                                let msg = make_ticker_message(&pair, tick);
                                if ws.send(Message::Text(msg)).await.is_err() {
                                    return;
                                }
                                tokio::time::sleep(interval).await;
                            }
                            // Keep connection open — client decides when to close.
                            let _ = futures_util::future::pending::<()>().await;
                        }
                        Scenario::DisconnectAfter {
                            pair,
                            ticks,
                            n,
                            interval,
                        } => {
                            for (i, tick) in ticks.iter().enumerate() {
                                if i >= n {
                                    break;
                                }
                                let msg = make_ticker_message(&pair, tick);
                                if ws.send(Message::Text(msg)).await.is_err() {
                                    return;
                                }
                                tokio::time::sleep(interval).await;
                            }
                            // Close the connection
                            let _ = ws.close(None).await;
                        }
                        Scenario::InvalidMessage => {
                            let _ = ws
                                .send(Message::Text("this is not valid json {{{{".to_string()))
                                .await;
                            let _ = futures_util::future::pending::<()>().await;
                        }
                    }
                });
            }
        });

        Self {
            url,
            _handle: handle,
        }
    }
}

impl Drop for MockBitflyerWs {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

/// Build a JSON-RPC channelMessage matching the BitFlyer lightning WS format.
fn make_ticker_message(pair: &str, tick: &TickData) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "channelMessage",
        "params": {
            "channel": format!("lightning_ticker_{pair}"),
            "message": {
                "product_code": pair,
                "ltp": tick.ltp,
                "best_bid": tick.best_bid,
                "best_ask": tick.best_ask,
                "volume": tick.volume,
                "timestamp": tick.timestamp,
            }
        }
    })
    .to_string()
}
