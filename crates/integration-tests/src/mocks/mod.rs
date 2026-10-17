use std::sync::{Mutex, MutexGuard};

/// Mock of the bitFlyer lightning WebSocket API.
pub mod bitflyer_ws;
/// Mock of the exchange API trait.
pub mod exchange_api;
/// Mock of the Gemini API.
pub mod gemini;
/// Mock of the GMO FX public API.
pub mod gmo_fx_server;
/// Mock of the OANDA API.
pub mod oanda_server;
/// Mock of a Slack webhook.
pub mod slack_webhook;
/// Mock of the Vegapunk client.
pub mod vegapunk;

pub(crate) fn lock_mock_state<'a, T>(
    mutex: &'a Mutex<T>,
    name: &str,
) -> anyhow::Result<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("failed to lock {name}: {e}"))
}
