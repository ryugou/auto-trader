//! Configurable [`ExchangeApi`] mock with builder pattern and failure injection.
//!
//! Each trait method can be pre-loaded with a success response via the builder.
//! Failure injection (`with_failures`) makes the first N calls return an error
//! before falling through to the configured response.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use rust_decimal_macros::dec;

use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse,
};
use auto_trader_market::exchange_api::ExchangeApi;

// ---------------------------------------------------------------------------
// CallCounter — tracks per-method invocation counts
// ---------------------------------------------------------------------------

/// Per-method call counters exposed via `Arc<AtomicU32>`.
#[derive(Debug, Default)]
pub struct CallCounters {
    pub send_child_order: AtomicU32,
    pub get_child_orders: AtomicU32,
    pub get_executions: AtomicU32,
    pub get_positions: AtomicU32,
    pub get_collateral: AtomicU32,
    pub cancel_child_order: AtomicU32,
}

// ---------------------------------------------------------------------------
// MethodConfig — per-method response + failure policy
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct MethodConfig<T: Clone> {
    response: T,
    fail_remaining: Arc<AtomicU32>,
}

impl<T: Clone> MethodConfig<T> {
    fn new(response: T) -> Self {
        Self {
            response,
            fail_remaining: Arc::new(AtomicU32::new(0)),
        }
    }

    fn with_failures(mut self, count: u32) -> Self {
        self.fail_remaining = Arc::new(AtomicU32::new(count));
        self
    }

    /// Returns `Ok(response)` or `Err` depending on remaining failure count.
    fn try_respond(&self) -> anyhow::Result<T> {
        let prev = self
            .fail_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                if n > 0 { Some(n - 1) } else { None }
            });
        match prev {
            Ok(_) => anyhow::bail!("MockExchangeApi: injected failure"),
            Err(_) => Ok(self.response.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// MockExchangeApi
// ---------------------------------------------------------------------------

pub struct MockExchangeApi {
    send_child_order_cfg: MethodConfig<SendChildOrderResponse>,
    get_child_orders_cfg: MethodConfig<Vec<ChildOrder>>,
    get_executions_cfg: MethodConfig<Vec<Execution>>,
    get_positions_cfg: MethodConfig<Vec<ExchangePosition>>,
    get_collateral_cfg: MethodConfig<Collateral>,
    cancel_child_order_cfg: MethodConfig<()>,
    pub counters: Arc<CallCounters>,
}

#[async_trait]
impl ExchangeApi for MockExchangeApi {
    async fn send_child_order(
        &self,
        _req: SendChildOrderRequest,
    ) -> anyhow::Result<SendChildOrderResponse> {
        self.counters
            .send_child_order
            .fetch_add(1, Ordering::SeqCst);
        self.send_child_order_cfg.try_respond()
    }

    async fn get_child_orders(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<ChildOrder>> {
        self.counters
            .get_child_orders
            .fetch_add(1, Ordering::SeqCst);
        self.get_child_orders_cfg.try_respond()
    }

    async fn get_executions(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<Vec<Execution>> {
        self.counters.get_executions.fetch_add(1, Ordering::SeqCst);
        self.get_executions_cfg.try_respond()
    }

    async fn get_positions(&self, _product_code: &str) -> anyhow::Result<Vec<ExchangePosition>> {
        self.counters.get_positions.fetch_add(1, Ordering::SeqCst);
        self.get_positions_cfg.try_respond()
    }

    async fn get_collateral(&self) -> anyhow::Result<Collateral> {
        self.counters.get_collateral.fetch_add(1, Ordering::SeqCst);
        self.get_collateral_cfg.try_respond()
    }

    async fn cancel_child_order(
        &self,
        _product_code: &str,
        _child_order_acceptance_id: &str,
    ) -> anyhow::Result<()> {
        self.counters
            .cancel_child_order
            .fetch_add(1, Ordering::SeqCst);
        self.cancel_child_order_cfg.try_respond()
    }

    async fn resolve_position_id(
        &self,
        _product_code: &str,
        _after: chrono::DateTime<chrono::Utc>,
        _expected_side: auto_trader_market::bitflyer_private::Side,
        _expected_size: rust_decimal::Decimal,
    ) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Builder for [`MockExchangeApi`].
///
/// All responses have sensible zero-value defaults so callers only need to
/// configure the methods they actually exercise.
pub struct MockExchangeApiBuilder {
    send_child_order_resp: SendChildOrderResponse,
    get_child_orders_resp: Vec<ChildOrder>,
    get_executions_resp: Vec<Execution>,
    get_positions_resp: Vec<ExchangePosition>,
    get_collateral_resp: Collateral,
    failures: Vec<(String, u32)>,
}

impl MockExchangeApiBuilder {
    pub fn new() -> Self {
        Self {
            send_child_order_resp: SendChildOrderResponse {
                child_order_acceptance_id: "mock-order-001".to_string(),
            },
            get_child_orders_resp: vec![],
            get_executions_resp: vec![],
            get_positions_resp: vec![],
            get_collateral_resp: Collateral {
                collateral: dec!(0),
                open_position_pnl: dec!(0),
                require_collateral: dec!(0),
                keep_rate: dec!(0),
            },
            failures: vec![],
        }
    }

    pub fn with_send_child_order_response(mut self, resp: SendChildOrderResponse) -> Self {
        self.send_child_order_resp = resp;
        self
    }

    pub fn with_get_positions_response(mut self, positions: Vec<ExchangePosition>) -> Self {
        self.get_positions_resp = positions;
        self
    }

    pub fn with_get_executions_response(mut self, executions: Vec<Execution>) -> Self {
        self.get_executions_resp = executions;
        self
    }

    pub fn with_get_collateral_response(mut self, collateral: Collateral) -> Self {
        self.get_collateral_resp = collateral;
        self
    }

    pub fn with_get_child_orders_response(mut self, orders: Vec<ChildOrder>) -> Self {
        self.get_child_orders_resp = orders;
        self
    }

    /// Register failure injection for a method.
    ///
    /// `method` must be one of: `"send_child_order"`, `"get_child_orders"`,
    /// `"get_executions"`, `"get_positions"`, `"get_collateral"`,
    /// `"cancel_child_order"`.
    pub fn with_failures(mut self, method: &str, count: u32) -> Self {
        self.failures.push((method.to_string(), count));
        self
    }

    pub fn build(self) -> Arc<MockExchangeApi> {
        let mut send_child_order_cfg = MethodConfig::new(self.send_child_order_resp);
        let mut get_child_orders_cfg = MethodConfig::new(self.get_child_orders_resp);
        let mut get_executions_cfg = MethodConfig::new(self.get_executions_resp);
        let mut get_positions_cfg = MethodConfig::new(self.get_positions_resp);
        let mut get_collateral_cfg = MethodConfig::new(self.get_collateral_resp);
        let mut cancel_child_order_cfg = MethodConfig::new(());

        for (method, count) in &self.failures {
            match method.as_str() {
                "send_child_order" => {
                    send_child_order_cfg = send_child_order_cfg.with_failures(*count);
                }
                "get_child_orders" => {
                    get_child_orders_cfg = get_child_orders_cfg.with_failures(*count);
                }
                "get_executions" => {
                    get_executions_cfg = get_executions_cfg.with_failures(*count);
                }
                "get_positions" => {
                    get_positions_cfg = get_positions_cfg.with_failures(*count);
                }
                "get_collateral" => {
                    get_collateral_cfg = get_collateral_cfg.with_failures(*count);
                }
                "cancel_child_order" => {
                    cancel_child_order_cfg = cancel_child_order_cfg.with_failures(*count);
                }
                other => panic!("MockExchangeApiBuilder: unknown method '{other}'"),
            }
        }

        Arc::new(MockExchangeApi {
            send_child_order_cfg,
            get_child_orders_cfg,
            get_executions_cfg,
            get_positions_cfg,
            get_collateral_cfg,
            cancel_child_order_cfg,
            counters: Arc::new(CallCounters::default()),
        })
    }
}

impl Default for MockExchangeApiBuilder {
    fn default() -> Self {
        Self::new()
    }
}
