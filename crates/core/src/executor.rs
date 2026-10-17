use crate::types::{ExitReason, Position, Signal, Trade};

/// Component that executes trading signals and manages the resulting positions.
pub trait OrderExecutor: Send + Sync + 'static {
    /// Executes the signal and returns the resulting trade.
    fn execute(
        &self,
        signal: &Signal,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
    /// Returns the open positions.
    fn open_positions(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Position>>> + Send;
    /// Closes the position identified by `id` for `exit_reason` and returns the closed trade.
    fn close_position(
        &self,
        id: &str,
        exit_reason: ExitReason,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
}
