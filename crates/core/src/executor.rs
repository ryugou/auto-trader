use crate::types::{Position, Signal, Trade};

pub trait OrderExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        signal: &Signal,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
    fn open_positions(
        &self,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<Position>>> + Send;
    fn close_position(
        &self,
        id: &str,
        exit_reason: crate::types::ExitReason,
        exit_price: rust_decimal::Decimal,
    ) -> impl std::future::Future<Output = anyhow::Result<Trade>> + Send;
}
