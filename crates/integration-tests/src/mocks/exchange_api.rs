use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use auto_trader_market::bitflyer_private::{
    ChildOrder, Collateral, ExchangePosition, Execution, SendChildOrderRequest,
    SendChildOrderResponse, Side,
};
use auto_trader_market::exchange_api::{ExchangeApi, StopOrderStatus};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use super::lock_mock_state;

const STOP_ORDER_STATUS_DRIVING_CLOSE_THROUGH_CANCEL: StopOrderStatus = StopOrderStatus::Active;

/// Counters of the calls to each method of the mock, including the calls that fail.
#[derive(Debug, Default)]
pub struct CallCounters {
    /// Number of calls to `send_child_order`.
    pub send_child_order: AtomicU32,
    /// Number of calls to `get_child_orders`.
    pub get_child_orders: AtomicU32,
    /// Number of calls to `get_executions`.
    pub get_executions: AtomicU32,
    /// Number of calls to `get_positions`.
    pub get_positions: AtomicU32,
    /// Number of calls to `get_collateral`.
    pub get_collateral: AtomicU32,
    /// Number of calls to `cancel_child_order`.
    pub cancel_child_order: AtomicU32,
    /// Number of calls to `place_stop_order`.
    pub place_stop_order: AtomicU32,
    /// Number of calls to `cancel_stop_order`.
    pub cancel_stop_order: AtomicU32,
    /// Number of calls to `stop_order_status`.
    pub stop_order_status: AtomicU32,
}

/// Arguments of a `place_stop_order` call.
#[derive(Debug, Clone)]
pub struct StopOrderCall {
    /// Product code of the stop order.
    pub product_code: String,
    /// Side of the order that closes the position.
    pub close_side: Side,
    /// Size of the stop order.
    pub size: Decimal,
    /// Price at which the stop order triggers.
    pub trigger_price: Decimal,
    /// Id of the position to close, if the call named one.
    pub position_id: Option<String>,
}

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

    fn try_respond(&self) -> anyhow::Result<T> {
        let prev = self
            .fail_remaining
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                if n > 0 { Some(n - 1) } else { None }
            });
        match prev {
            Ok(_) => anyhow::bail!("failed to call exchange api: injected failure"),
            Err(_) => Ok(self.response.clone()),
        }
    }
}

/// Mock of `ExchangeApi` that returns configured responses, injects failures and counts the calls it receives.
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
    stop_order_status_seq: Arc<Mutex<VecDeque<StopOrderStatus>>>,
    /// Arguments of every `place_stop_order` call, in call order.
    pub place_stop_calls: Arc<Mutex<Vec<StopOrderCall>>>,
    /// Counters of the calls to each method.
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
        lock_mock_state(&self.place_stop_calls, "place_stop_calls")?.push(StopOrderCall {
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
        if let Some(next) =
            lock_mock_state(&self.stop_order_status_seq, "stop_order_status_seq")?.pop_front()
        {
            return Ok(next);
        }
        self.stop_order_status_cfg.try_respond()
    }
}

/// Builder of `MockExchangeApi`.
pub struct MockExchangeApiBuilder {
    send_child_order_resp: SendChildOrderResponse,
    get_child_orders_resp: Vec<ChildOrder>,
    get_executions_resp: Vec<Execution>,
    get_positions_resp: Vec<ExchangePosition>,
    get_collateral_resp: Collateral,
    place_stop_order_resp: String,
    stop_order_status_resp: StopOrderStatus,
    stop_order_status_seq: Vec<StopOrderStatus>,
    failures: Vec<(String, u32)>,
}

impl MockExchangeApiBuilder {
    /// Returns a builder whose mock returns empty lists, a zero collateral, the order id `mock-order-001`, the stop order id `mock-stop-001` and the stop order status `Active`, and injects no failures.
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
            stop_order_status_resp: STOP_ORDER_STATUS_DRIVING_CLOSE_THROUGH_CANCEL,
            stop_order_status_seq: vec![],
            failures: vec![],
        }
    }

    /// Sets the statuses that successive `stop_order_status` calls return, one per call.
    ///
    /// Once the statuses are used up, the calls return the status set by `with_stop_order_status_response`, and injected failures apply to them.
    pub fn with_stop_order_status_sequence(mut self, seq: Vec<StopOrderStatus>) -> Self {
        self.stop_order_status_seq = seq;
        self
    }

    /// Sets the stop order id that `place_stop_order` returns.
    pub fn with_place_stop_order_response(mut self, id: impl Into<String>) -> Self {
        self.place_stop_order_resp = id.into();
        self
    }

    /// Sets the status that `stop_order_status` returns.
    pub fn with_stop_order_status_response(mut self, status: StopOrderStatus) -> Self {
        self.stop_order_status_resp = status;
        self
    }

    /// Sets the response that `send_child_order` returns.
    pub fn with_send_child_order_response(mut self, resp: SendChildOrderResponse) -> Self {
        self.send_child_order_resp = resp;
        self
    }

    /// Sets the positions that `get_positions` returns.
    pub fn with_get_positions_response(mut self, positions: Vec<ExchangePosition>) -> Self {
        self.get_positions_resp = positions;
        self
    }

    /// Sets the executions that `get_executions` returns.
    pub fn with_get_executions_response(mut self, executions: Vec<Execution>) -> Self {
        self.get_executions_resp = executions;
        self
    }

    /// Sets the collateral that `get_collateral` returns.
    pub fn with_get_collateral_response(mut self, collateral: Collateral) -> Self {
        self.get_collateral_resp = collateral;
        self
    }

    /// Sets the orders that `get_child_orders` returns.
    pub fn with_get_child_orders_response(mut self, orders: Vec<ChildOrder>) -> Self {
        self.get_child_orders_resp = orders;
        self
    }

    /// Makes the first `count` calls to `method` return an error.
    ///
    /// `method` must be one of `"send_child_order"`, `"get_child_orders"`, `"get_executions"`, `"get_positions"`, `"get_collateral"`, `"cancel_child_order"`, `"place_stop_order"`, `"cancel_stop_order"` and `"stop_order_status"`.
    pub fn with_failures(mut self, method: &str, count: u32) -> Self {
        self.failures.push((method.to_string(), count));
        self
    }

    /// Builds the mock.
    ///
    /// # Panics
    ///
    /// Panics if the method name given to `with_failures` is not one of the nine methods.
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
                other => panic!("unsupported method: \"{other}\""),
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
            stop_order_status_seq: Arc::new(Mutex::new(self.stop_order_status_seq.into())),
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
