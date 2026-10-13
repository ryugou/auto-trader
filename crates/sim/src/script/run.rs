use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use rhai::{CallFnOptions, Dynamic, Scope};

use super::ctx::RhaiCtx;
use super::types::{BarState, Decider, ParamSet};
use super::{CompiledScript, param_set_to_rhai_map};
use super::{ScriptHost, describe_rhai_error, reset_ops_relay, take_last_call_ops};
use crate::config::SimConfig;
use crate::indicators::IndicatorKey;
use crate::series::Dataset;

pub struct ScriptRun<'a> {
    host: &'a ScriptHost,
    script: &'a CompiledScript,
    dataset: Arc<Dataset>,
    params_dynamic: Dynamic,
    this: Dynamic,
    requested_keys: Arc<Mutex<HashSet<IndicatorKey>>>,
    max_operations_per_bar: u64,
    max_operations_per_run: u64,
    run_total_ops: u64,
}

impl<'a> ScriptRun<'a> {
    pub fn new(
        host: &'a ScriptHost,
        script: &'a CompiledScript,
        params: &ParamSet,
        dataset: Arc<Dataset>,
        cfg: &SimConfig,
    ) -> ScriptRun<'a> {
        ScriptRun {
            host,
            script,
            dataset,
            params_dynamic: Dynamic::from_map(param_set_to_rhai_map(params)),
            this: Dynamic::from_map(rhai::Map::new()),
            requested_keys: Arc::new(Mutex::new(HashSet::new())),
            max_operations_per_bar: cfg.max_operations_per_bar,
            max_operations_per_run: cfg.max_operations_per_run,
            run_total_ops: 0,
        }
    }
}

impl Decider for ScriptRun<'_> {
    fn on_bar(&mut self, state: BarState) -> Result<i8, String> {
        let ctx = RhaiCtx::new(
            self.dataset.clone(),
            state.t,
            state.position,
            state.entry_price_milli,
            state.bars_held,
            self.requested_keys.clone(),
        );
        let args = (Dynamic::from(ctx), self.params_dynamic.clone());

        reset_ops_relay(
            self.run_total_ops,
            self.max_operations_per_run,
            self.max_operations_per_bar,
        );
        let mut scope = Scope::new();
        let options = CallFnOptions::new()
            .eval_ast(false)
            .bind_this_ptr(&mut self.this);
        let result = self.host.engine.call_fn_with_options::<Dynamic>(
            options,
            &mut scope,
            &self.script.ast,
            "on_bar",
            args,
        );

        self.run_total_ops = self.run_total_ops.saturating_add(take_last_call_ops());

        match result {
            Ok(value) => {
                if let Ok(n) = value.as_int() {
                    match n {
                        -1..=1 => Ok(n as i8),
                        other => Err(format!(
                            "unsupported on_bar return value: {other} (expected 1, -1, or 0)"
                        )),
                    }
                } else {
                    Err(format!(
                        "unsupported on_bar return type: \"{}\" (expected an integer 1, -1, or 0)",
                        value.type_name()
                    ))
                }
            }
            Err(err) => Err(describe_rhai_error(&err)),
        }
    }
}
