use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// Ticker message that the mock bitFlyer WebSocket server sends.
#[derive(Clone, Debug)]
pub struct TickData {
    /// Last traded price.
    pub ltp: i64,
    /// Best bid price.
    pub best_bid: i64,
    /// Best ask price.
    pub best_ask: i64,
    /// Traded volume.
    pub volume: f64,
    /// Timestamp of the tick, as the string that the message carries.
    pub timestamp: String,
}

impl TickData {
    /// Returns a tick with a volume of 0.01 and the timestamp `2026-04-28T00:00:00.000`.
    pub fn new(ltp: i64, best_bid: i64, best_ask: i64) -> Self {
        Self {
            ltp,
            best_bid,
            best_ask,
            volume: 0.01,
            timestamp: "2026-04-28T00:00:00.000".to_string(),
        }
    }

    /// Sets the timestamp of the tick.
    pub fn with_timestamp(mut self, ts: &str) -> Self {
        self.timestamp = ts.to_string();
        self
    }

    /// Sets the volume of the tick.
    pub fn with_volume(mut self, v: f64) -> Self {
        self.volume = v;
        self
    }
}

#[derive(Clone)]
enum Scenario {
    NormalTicks {
        pair: String,
        ticks: Vec<TickData>,
        interval: std::time::Duration,
    },
    DisconnectAfter {
        pair: String,
        ticks: Vec<TickData>,
        n: usize,
        interval: std::time::Duration,
    },
    InvalidMessage,
}

/// Mock of the bitFlyer lightning WebSocket API that plays one scenario to every client that connects, on an ephemeral port in a background task that is aborted on drop.
pub struct MockBitflyerWs {
    url: String,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockBitflyerWs {
    /// Starts a mock that sends the ticks as ticker messages of `pair`, 10 ms apart, and then keeps the connection open.
    ///
    /// # Errors
    ///
    /// - Returns an error if binding the port fails.
    /// - Returns an error if reading the local address of the listener fails.
    pub async fn normal_ticks(pair: &str, ticks: Vec<TickData>) -> anyhow::Result<Self> {
        Self::normal_ticks_with_interval(pair, ticks, std::time::Duration::from_millis(10)).await
    }

    /// Starts a mock that sends the ticks as ticker messages of `pair`, one per `interval`, and then keeps the connection open.
    ///
    /// # Errors
    ///
    /// - Returns an error if binding the port fails.
    /// - Returns an error if reading the local address of the listener fails.
    pub async fn normal_ticks_with_interval(
        pair: &str,
        ticks: Vec<TickData>,
        interval: std::time::Duration,
    ) -> anyhow::Result<Self> {
        Self::start(Scenario::NormalTicks {
            pair: pair.to_string(),
            ticks,
            interval,
        })
        .await
    }

    /// Starts a mock that sends the first `n` of the ticks as ticker messages of `pair`, 10 ms apart, and then closes the connection.
    ///
    /// # Errors
    ///
    /// - Returns an error if binding the port fails.
    /// - Returns an error if reading the local address of the listener fails.
    pub async fn disconnect_after(
        pair: &str,
        ticks: Vec<TickData>,
        n: usize,
    ) -> anyhow::Result<Self> {
        Self::start(Scenario::DisconnectAfter {
            pair: pair.to_string(),
            ticks,
            n,
            interval: std::time::Duration::from_millis(10),
        })
        .await
    }

    /// Starts a mock that sends one message that is not valid JSON and then keeps the connection open.
    ///
    /// # Errors
    ///
    /// - Returns an error if binding the port fails.
    /// - Returns an error if reading the local address of the listener fails.
    pub async fn invalid_message() -> anyhow::Result<Self> {
        Self::start(Scenario::InvalidMessage).await
    }

    /// Returns the URL `ws://127.0.0.1:{port}` that clients connect to.
    pub fn url(&self) -> String {
        self.url.clone()
    }

    async fn start(scenario: Scenario) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind ephemeral port")?;
        let port = listener
            .local_addr()
            .context("failed to get local addr")?
            .port();
        let url = format!("ws://127.0.0.1:{port}");

        let handle = tokio::spawn(serve_scenario_to_every_reconnecting_client(
            listener, scenario,
        ));

        Ok(Self {
            url,
            _handle: handle,
        })
    }
}

impl Drop for MockBitflyerWs {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

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

async fn serve_scenario_to_every_reconnecting_client(listener: TcpListener, scenario: Scenario) {
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
                    drain_until_client_disconnects(&mut ws).await;
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
                    let _ = ws.close(None).await;
                }
                Scenario::InvalidMessage => {
                    let _ = ws
                        .send(Message::Text("this is not valid json {{{{".to_string()))
                        .await;
                    drain_until_client_disconnects(&mut ws).await;
                }
            }
        });
    }
}

async fn drain_until_client_disconnects(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) {
    while let Some(Ok(_)) = ws.next().await {}
}
