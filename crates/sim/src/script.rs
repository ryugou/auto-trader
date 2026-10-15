use std::cell::RefCell;
use std::collections::HashSet;

use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    BasicStringPackage, LogicPackage, Package,
};
use rhai::{AST, CallFnOptions, Dynamic, Engine, EvalAltResult, Scope};
use sha2::{Digest, Sha256};

use crate::config::SimConfig;
use crate::error::SimError;

mod ctx;
mod run;
mod types;

pub use run::ScriptRun;
pub use types::{BarState, Decider, ParamKind, ParamSet, ParamSpec, ParamValue};

use ctx::register_ctx;

const MAX_SCRIPT_BYTES: usize = 32 * 1024;
const MAX_PARAM_COUNT: usize = 12;
const MAX_CALL_LEVELS: usize = 16;
const MAX_STRING_SIZE: usize = 4096;
const MAX_ARRAY_SIZE: usize = 1024;
const MAX_MAP_SIZE: usize = 256;
const FLOAT_TOLERANCE: f64 = 1e-9;

fn is_on_float_grid(value: f64, min: f64, step: f64) -> bool {
    let k = ((value - min) / step).round();
    k >= 0.0 && (value - (min + k * step)).abs() <= FLOAT_TOLERANCE
}

fn is_on_integer_grid(value: i64, min: i64, step: i64) -> bool {
    (value as i128 - min as i128) % step as i128 == 0
}

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

fn reset_ops_relay(run_total_before_call: u64, run_max_total: u64, bar_max: u64) {
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

fn take_last_call_ops() -> u64 {
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

fn describe_rhai_error(err: &EvalAltResult) -> String {
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

pub struct CompiledScript {
    pub source: String,
    pub sha256: String,
    pub params: Vec<ParamSpec>,
    ast: AST,
}

impl CompiledScript {
    pub fn default_params(&self) -> ParamSet {
        self.params
            .iter()
            .map(|p| (p.name.clone(), resolve_default(p)))
            .collect()
    }

    pub fn resolve_params(
        &self,
        given: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ParamSet, SimError> {
        let known: HashSet<&str> = self.params.iter().map(|p| p.name.as_str()).collect();
        for key in given.keys() {
            if !known.contains(key.as_str()) {
                return Err(SimError::Args(format!(
                    "invalid parameter \"{key}\": unknown"
                )));
            }
        }

        let mut result = ParamSet::new();
        for spec in &self.params {
            let value = match given.get(&spec.name) {
                None => resolve_default(spec),
                Some(json_value) => resolve_one_param(spec, json_value)?,
            };
            result.insert(spec.name.clone(), value);
        }
        Ok(result)
    }
}

fn resolve_default(spec: &ParamSpec) -> ParamValue {
    match spec.kind {
        ParamKind::Int => ParamValue::Int(spec.default.round() as i64),
        ParamKind::Float => ParamValue::Float(spec.default),
    }
}

fn resolve_one_param(
    spec: &ParamSpec,
    json_value: &serde_json::Value,
) -> Result<ParamValue, SimError> {
    match spec.kind {
        ParamKind::Int => {
            let n = json_value.as_i64().ok_or_else(|| {
                SimError::Args(format!(
                    "invalid parameter \"{}\": must be an integer",
                    spec.name
                ))
            })?;
            let (min, max, step) = (spec.min as i64, spec.max as i64, spec.step as i64);
            if n < min || n > max {
                return Err(SimError::Args(format!(
                    "invalid parameter \"{}\": out of range [{min}, {max}]",
                    spec.name
                )));
            }
            if step != 0 && !is_on_integer_grid(n, min, step) {
                return Err(SimError::Args(format!(
                    "invalid parameter \"{}\": not on the step grid (min={min}, step={step})",
                    spec.name
                )));
            }
            Ok(ParamValue::Int(n))
        }
        ParamKind::Float => {
            let f = json_value.as_f64().ok_or_else(|| {
                SimError::Args(format!(
                    "invalid parameter \"{}\": must be a number",
                    spec.name
                ))
            })?;
            if f < spec.min - FLOAT_TOLERANCE || f > spec.max + FLOAT_TOLERANCE {
                return Err(SimError::Args(format!(
                    "invalid parameter \"{}\": out of range [{}, {}]",
                    spec.name, spec.min, spec.max
                )));
            }
            if !is_on_float_grid(f, spec.min, spec.step) {
                return Err(SimError::Args(format!(
                    "invalid parameter \"{}\": not on the step grid (min={}, step={})",
                    spec.name, spec.min, spec.step
                )));
            }
            Ok(ParamValue::Float(f))
        }
    }
}

fn param_set_to_rhai_map(params: &ParamSet) -> rhai::Map {
    params
        .iter()
        .map(|(name, value)| {
            let dynamic = match value {
                ParamValue::Int(n) => Dynamic::from(*n),
                ParamValue::Float(f) => Dynamic::from(*f),
            };
            (name.as_str().into(), dynamic)
        })
        .collect()
}

pub struct ScriptHost {
    engine: Engine,
    max_operations_per_bar: u64,
}

impl ScriptHost {
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

        Ok(CompiledScript {
            source: source.to_string(),
            sha256,
            params,
            ast,
        })
    }
}

fn parse_param_specs(map: &rhai::Map) -> Result<Vec<ParamSpec>, SimError> {
    if map.len() > MAX_PARAM_COUNT {
        return Err(SimError::InvalidScript(format!(
            "invalid parameter count: exceeds the {MAX_PARAM_COUNT}-parameter limit"
        )));
    }

    let mut specs = Vec::with_capacity(map.len());
    for (name, value) in map.iter() {
        let name = name.to_string();
        let spec_map = value.clone().try_cast::<rhai::Map>().ok_or_else(|| {
            SimError::InvalidScript(format!(
                "invalid parameter \"{name}\": must be a map with min/max/step/default"
            ))
        })?;

        let field = |key: &str| -> Result<&Dynamic, SimError> {
            spec_map.get(key).ok_or_else(|| {
                SimError::InvalidScript(format!("invalid parameter \"{name}\": missing \"{key}\""))
            })
        };
        let min_d = field("min")?;
        let max_d = field("max")?;
        let step_d = field("step")?;
        let default_d = field("default")?;

        let all_int = min_d.is_int() && max_d.is_int() && step_d.is_int() && default_d.is_int();
        let all_float =
            min_d.is_float() && max_d.is_float() && step_d.is_float() && default_d.is_float();
        if !all_int && !all_float {
            return Err(SimError::InvalidScript(format!(
                "invalid parameter \"{name}\": min/max/step/default must all be integers or all be floats"
            )));
        }

        let (kind, min, max, step, default) = if all_int {
            let min_i = min_d.as_int().expect("checked is_int above");
            let max_i = max_d.as_int().expect("checked is_int above");
            let step_i = step_d.as_int().expect("checked is_int above");
            let default_i = default_d.as_int().expect("checked is_int above");
            if !(min_i <= default_i && default_i <= max_i) {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": must satisfy min <= default <= max"
                )));
            }
            if step_i <= 0 {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": step must be > 0"
                )));
            }
            if !is_on_integer_grid(default_i, min_i, step_i) {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": default must equal min + k*step for a non-negative integer k"
                )));
            }
            (
                ParamKind::Int,
                min_i as f64,
                max_i as f64,
                step_i as f64,
                default_i as f64,
            )
        } else {
            let min_f = min_d.as_float().expect("checked is_float above");
            let max_f = max_d.as_float().expect("checked is_float above");
            let step_f = step_d.as_float().expect("checked is_float above");
            let default_f = default_d.as_float().expect("checked is_float above");
            if !(min_f.is_finite()
                && max_f.is_finite()
                && step_f.is_finite()
                && default_f.is_finite())
            {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": min/max/step/default must be finite"
                )));
            }
            if !(min_f <= default_f && default_f <= max_f) {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": must satisfy min <= default <= max"
                )));
            }
            if step_f <= 0.0 {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": step must be > 0"
                )));
            }
            if !is_on_float_grid(default_f, min_f, step_f) {
                return Err(SimError::InvalidScript(format!(
                    "invalid parameter \"{name}\": default must equal min + k*step for a non-negative integer k (value difference within 1e-9)"
                )));
            }
            (ParamKind::Float, min_f, max_f, step_f, default_f)
        };

        specs.push(ParamSpec {
            name,
            kind,
            min,
            max,
            step,
            default,
        });
    }

    sort_by_name_independent_of_map_iteration_order(&mut specs);
    Ok(specs)
}

fn sort_by_name_independent_of_map_iteration_order(specs: &mut [ParamSpec]) {
    specs.sort_by(|a, b| a.name.cmp(&b.name));
}

#[cfg(test)]
mod tests;
