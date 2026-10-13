use std::cell::RefCell;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    BasicStringPackage, LogicPackage, Package,
};
use rhai::{CallFnOptions, Dynamic, Engine, EvalAltResult, Scope};
use sha2::{Digest, Sha256};

use super::ctx::register_ctx;
use super::params::{CompiledScript, parse_param_specs};
use crate::config::SimConfig;
use crate::error::SimError;

const MAX_SCRIPT_BYTES: usize = 32 * 1024;
const MAX_CALL_LEVELS: usize = 16;
const MAX_STRING_SIZE: usize = 4096;
const MAX_ARRAY_SIZE: usize = 1024;
const MAX_MAP_SIZE: usize = 256;

#[derive(Default)]
struct OpsRelay {
    run_total_before_call: u64,
    run_max_total: u64,
    bar_max: u64,
    last_call_ops: u64,
}

thread_local! {
    static OPS_RELAY: RefCell<OpsRelay> = RefCell::new(OpsRelay::default());
}

pub(super) fn reset_ops_relay(run_total_before_call: u64, run_max_total: u64, bar_max: u64) {
    OPS_RELAY.with(|relay| {
        *relay.borrow_mut() = OpsRelay {
            run_total_before_call,
            run_max_total,
            bar_max,
            last_call_ops: 0,
        };
    });
}

fn reset_ops_relay_without_a_run_total_budget(bar_max: u64) {
    reset_ops_relay(0, u64::MAX, bar_max);
}

pub(super) fn take_last_call_ops() -> u64 {
    OPS_RELAY.with(|relay| relay.borrow().last_call_ops)
}

fn check_progress(ops: u64) -> Option<Dynamic> {
    OPS_RELAY.with(|relay| {
        let mut relay = relay.borrow_mut();
        relay.last_call_ops = ops;
        if ops > relay.bar_max {
            return Some(Dynamic::from(format!(
                "invalid operation count \"max_operations_per_bar\": exceeds the {}-operation limit",
                relay.bar_max
            )));
        }
        let run_total = relay.run_total_before_call.saturating_add(ops);
        if run_total > relay.run_max_total {
            return Some(Dynamic::from(format!(
                "invalid operation count \"max_operations_per_run\": exceeds the {}-operation limit",
                relay.run_max_total
            )));
        }
        None
    })
}

pub(super) fn describe_rhai_error(err: &EvalAltResult) -> String {
    match err {
        EvalAltResult::ErrorTerminated(token, pos) => {
            let detail = token
                .clone()
                .into_immutable_string()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "invalid operation count: limit exceeded".to_string());
            if pos.is_none() {
                detail
            } else {
                format!("{detail} ({pos})")
            }
        }
        other => other.to_string(),
    }
}

pub struct ScriptHost {
    engine: Engine,
    max_operations_per_bar: u64,
}

impl ScriptHost {
    pub(super) fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn new(cfg: &SimConfig) -> ScriptHost {
        let mut engine = Engine::new_raw();

        ArithmeticPackage::new().register_into_engine(&mut engine);
        LogicPackage::new().register_into_engine(&mut engine);
        BasicMathPackage::new().register_into_engine(&mut engine);
        BasicIteratorPackage::new().register_into_engine(&mut engine);
        BasicArrayPackage::new().register_into_engine(&mut engine);
        BasicMapPackage::new().register_into_engine(&mut engine);
        BasicStringPackage::new().register_into_engine(&mut engine);

        engine.set_module_resolver(DummyModuleResolver::new());
        engine.disable_symbol("eval");

        engine.set_max_call_levels(MAX_CALL_LEVELS);
        engine.set_max_string_size(MAX_STRING_SIZE);
        engine.set_max_array_size(MAX_ARRAY_SIZE);
        engine.set_max_map_size(MAX_MAP_SIZE);

        engine.on_progress(check_progress);

        register_ctx(&mut engine);

        ScriptHost {
            engine,
            max_operations_per_bar: cfg.max_operations_per_bar,
        }
    }

    pub fn compile(&self, source: &str) -> Result<CompiledScript, SimError> {
        if source.len() > MAX_SCRIPT_BYTES {
            return Err(SimError::InvalidScript(format!(
                "invalid script size: exceeds the {MAX_SCRIPT_BYTES}-byte limit"
            )));
        }

        let ast = self
            .engine
            .compile(source)
            .map_err(|e| SimError::InvalidScript(format!("failed to parse script: {e}")))?;

        let mut has_params_fn = false;
        let mut has_on_bar_fn = false;
        for f in ast.iter_functions() {
            if f.name == "params" && f.params.is_empty() {
                has_params_fn = true;
            }
            if f.name == "on_bar" && f.params.len() == 2 {
                has_on_bar_fn = true;
            }
        }
        if !has_params_fn {
            return Err(SimError::InvalidScript(
                "invalid script: missing `fn params()` with 0 arguments".to_string(),
            ));
        }
        if !has_on_bar_fn {
            return Err(SimError::InvalidScript(
                "invalid script: missing `fn on_bar(ctx, p)` with 2 arguments".to_string(),
            ));
        }

        reset_ops_relay_without_a_run_total_budget(self.max_operations_per_bar);
        let mut scope = Scope::new();
        let options = CallFnOptions::new().eval_ast(false);
        let map: rhai::Map = self
            .engine
            .call_fn_with_options(options, &mut scope, &ast, "params", ())
            .map_err(|e| {
                SimError::InvalidScript(format!(
                    "failed to run params(): {}",
                    describe_rhai_error(&e)
                ))
            })?;

        let params = parse_param_specs(&map)?;

        let sha256 = {
            let digest = Sha256::digest(source.as_bytes());
            hex::encode(digest)
        };

        Ok(CompiledScript::new(source.to_string(), sha256, params, ast))
    }
}
