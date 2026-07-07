//! Configurable [`ExchangeApi`] mock with builder pattern and failure injection.
//!
//! Each trait method can be pre-loaded with a success response via the builder.
//! Failure injection (`with_failures`) makes the first N calls return an error
//! before falling through to the configured response.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use auto_trader_market::exchange_api::{ExchangeApi, StopOrderStatus};

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
    pub place_stop_order: AtomicU32,
    pub cancel_stop_order: AtomicU32,
    pub stop_order_status: AtomicU32,
}

/// Recorded arguments for a single `place_stop_order` invocation. Tests assert
/// the trader placed the stop with the rounded trigger price / close side.
#[derive(Debug, Clone)]
pub struct StopOrderCall {
    pub product_code: String,
    pub close_side: Side,
    pub size: Decimal,
    pub trigger_price: Decimal,
    pub position_id: Option<String>,
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
    place_stop_order_cfg: MethodConfig<String>,
    cancel_stop_order_cfg: MethodConfig<()>,
    stop_order_status_cfg: MethodConfig<StopOrderStatus>,
    /// Recorded `place_stop_order` args (in call order).
    pub place_stop_calls: Arc<Mutex<Vec<StopOrderCall>>>,
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

    async fn place_stop_order(
        &self,
        product_code: &str,
        close_side: Side,
        size: Decimal,
        trigger_price: Decimal,
        position_id: Option<&str>,
    ) -> anyhow::Result<String> {
        self.counters
            .place_stop_order
            .fetch_add(1, Ordering::SeqCst);
        self.place_stop_calls
            .lock()
            .expect("place_stop_calls mutex poisoned")
            .push(StopOrderCall {
                product_code: product_code.to_string(),
                close_side,
                size,
                trigger_price,
                position_id: position_id.map(|s| s.to_string()),
            });
        self.place_stop_order_cfg.try_respond()
    }

    async fn cancel_stop_order(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<()> {
        self.counters
            .cancel_stop_order
            .fetch_add(1, Ordering::SeqCst);
        self.cancel_stop_order_cfg.try_respond()
    }

    async fn stop_order_status(
        &self,
        _product_code: &str,
        _stop_order_id: &str,
    ) -> anyhow::Result<StopOrderStatus> {
        self.counters
            .stop_order_status
            .fetch_add(1, Ordering::SeqCst);
        self.stop_order_status_cfg.try_respond()
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
    place_stop_order_resp: String,
    stop_order_status_resp: StopOrderStatus,
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
            place_stop_order_resp: "mock-stop-001".to_string(),
            // 既定は Active: close 経路が cancel → 成行 close に進む。
            stop_order_status_resp: StopOrderStatus::Active,
            failures: vec![],
        }
    }

    pub fn with_place_stop_order_response(mut self, id: impl Into<String>) -> Self {
        self.place_stop_order_resp = id.into();
        self
    }

    pub fn with_stop_order_status_response(mut self, status: StopOrderStatus) -> Self {
        self.stop_order_status_resp = status;
        self
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
    /// `"cancel_child_order"`, `"place_stop_order"`, `"cancel_stop_order"`,
    /// `"stop_order_status"`.
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
        let mut place_stop_order_cfg = MethodConfig::new(self.place_stop_order_resp);
        let mut cancel_stop_order_cfg = MethodConfig::new(());
        let mut stop_order_status_cfg = MethodConfig::new(self.stop_order_status_resp);

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
                "place_stop_order" => {
                    place_stop_order_cfg = place_stop_order_cfg.with_failures(*count);
                }
                "cancel_stop_order" => {
                    cancel_stop_order_cfg = cancel_stop_order_cfg.with_failures(*count);
                }
                "stop_order_status" => {
                    stop_order_status_cfg = stop_order_status_cfg.with_failures(*count);
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
            place_stop_order_cfg,
            cancel_stop_order_cfg,
            stop_order_status_cfg,
            place_stop_calls: Arc::new(Mutex::new(Vec::new())),
            counters: Arc::new(CallCounters::default()),
        })
    }
}

impl Default for MockExchangeApiBuilder {
    fn default() -> Self {
        Self::new()
    }
}
