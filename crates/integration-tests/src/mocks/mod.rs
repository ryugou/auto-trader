use std::sync::{Mutex, MutexGuard};

pub mod bitflyer_ws;
pub mod exchange_api;
pub mod gemini;
pub mod gmo_fx_server;
pub mod oanda_server;
pub mod slack_webhook;
pub mod vegapunk;

pub(crate) fn lock_mock_state<'a, T>(
    mutex: &'a Mutex<T>,
    name: &str,
) -> anyhow::Result<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|e| anyhow::anyhow!("failed to lock {name}: {e}"))
}
