use std::collections::HashSet;

use rhai::Dynamic;

use super::types::{ParamKind, ParamSet, ParamSpec, ParamValue};
use crate::error::SimError;
use crate::types::FLOAT_GRID_TOLERANCE;

const MAX_PARAM_COUNT: usize = 12;

fn is_on_float_grid(value: f64, min: f64, step: f64) -> bool {
    let k = ((value - min) / step).round();
    k >= 0.0 && (value - (min + k * step)).abs() <= FLOAT_GRID_TOLERANCE
}

fn is_on_integer_grid(value: i64, min: i64, step: i64) -> bool {
    (value as i128 - min as i128) % step as i128 == 0
}

pub struct CompiledScript {
    pub source: String,
    pub sha256: String,
    pub params: Vec<ParamSpec>,
    ast: rhai::AST,
}

impl CompiledScript {
    pub(super) fn new(
        source: String,
        sha256: String,
        params: Vec<ParamSpec>,
        ast: rhai::AST,
    ) -> CompiledScript {
        CompiledScript {
            source,
            sha256,
            params,
            ast,
        }
    }

    pub(super) fn ast(&self) -> &rhai::AST {
        &self.ast
    }

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
            if f < spec.min - FLOAT_GRID_TOLERANCE || f > spec.max + FLOAT_GRID_TOLERANCE {
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

pub(super) fn param_set_to_rhai_map(params: &ParamSet) -> rhai::Map {
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

pub(super) fn parse_param_specs(map: &rhai::Map) -> Result<Vec<ParamSpec>, SimError> {
    if map.len() > MAX_PARAM_COUNT {
        return Err(SimError::InvalidScript(format!(
            "invalid parameter count: exceeds the {MAX_PARAM_COUNT}-parameter limit"
        )));
    }

    let mut specs = Vec::with_capacity(map.len());
    for (name, value) in map.iter() {
        specs.push(parse_one_param_spec(name.as_ref(), value)?);
    }

    sort_by_name_independent_of_map_iteration_order(&mut specs);
    Ok(specs)
}

fn parse_one_param_spec(name: &str, value: &Dynamic) -> Result<ParamSpec, SimError> {
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

    let (kind, min, max, step, default) = if let Some((min_i, max_i, step_i, default_i)) =
        extract_int_fields(min_d, max_d, step_d, default_d)
    {
        let (min, max, step, default) = validate_int_param(name, min_i, max_i, step_i, default_i)?;
        (ParamKind::Int, min, max, step, default)
    } else if let Some((min_f, max_f, step_f, default_f)) =
        extract_float_fields(min_d, max_d, step_d, default_d)
    {
        let (min, max, step, default) =
            validate_float_param(name, min_f, max_f, step_f, default_f)?;
        (ParamKind::Float, min, max, step, default)
    } else {
        return Err(SimError::InvalidScript(format!(
            "invalid parameter \"{name}\": min/max/step/default must all be integers or all be floats"
        )));
    };
    Ok(ParamSpec {
        name: name.to_string(),
        kind,
        min,
        max,
        step,
        default,
    })
}

fn extract_int_fields(
    min_d: &Dynamic,
    max_d: &Dynamic,
    step_d: &Dynamic,
    default_d: &Dynamic,
) -> Option<(i64, i64, i64, i64)> {
    Some((
        min_d.as_int().ok()?,
        max_d.as_int().ok()?,
        step_d.as_int().ok()?,
        default_d.as_int().ok()?,
    ))
}

fn extract_float_fields(
    min_d: &Dynamic,
    max_d: &Dynamic,
    step_d: &Dynamic,
    default_d: &Dynamic,
) -> Option<(f64, f64, f64, f64)> {
    Some((
        min_d.as_float().ok()?,
        max_d.as_float().ok()?,
        step_d.as_float().ok()?,
        default_d.as_float().ok()?,
    ))
}

fn validate_int_param(
    name: &str,
    min_i: i64,
    max_i: i64,
    step_i: i64,
    default_i: i64,
) -> Result<(f64, f64, f64, f64), SimError> {
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
    Ok((min_i as f64, max_i as f64, step_i as f64, default_i as f64))
}

fn validate_float_param(
    name: &str,
    min_f: f64,
    max_f: f64,
    step_f: f64,
    default_f: f64,
) -> Result<(f64, f64, f64, f64), SimError> {
    if !(min_f.is_finite() && max_f.is_finite() && step_f.is_finite() && default_f.is_finite()) {
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
            "invalid parameter \"{name}\": default must equal min + k*step for a non-negative integer k (value difference within {FLOAT_GRID_TOLERANCE:e})"
        )));
    }
    Ok((min_f, max_f, step_f, default_f))
}

fn sort_by_name_independent_of_map_iteration_order(specs: &mut [ParamSpec]) {
    specs.sort_by(|a, b| a.name.cmp(&b.name));
}
