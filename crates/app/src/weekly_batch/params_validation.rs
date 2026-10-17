use anyhow::Context as _;

pub(super) fn validate_params(
    strategy_name: &str,
    proposed: &serde_json::Value,
    current: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    match strategy_name {
        "donchian_trend_evolve_v1" => validate_donchian_trend_evolve_v1(proposed),
        _ => validate_permissive(strategy_name, proposed, current),
    }
}

fn validate_donchian_trend_evolve_v1(
    params: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let entry = params["entry_channel"]
        .as_i64()
        .context("invalid entry_channel: missing or non-integer")?;
    let exit = params["exit_channel"]
        .as_i64()
        .context("invalid exit_channel: missing or non-integer")?;
    let baseline = params["atr_baseline_bars"]
        .as_i64()
        .context("invalid atr_baseline_bars: missing or non-integer")?;

    if !(10..=30).contains(&entry) {
        anyhow::bail!("invalid entry_channel: out of range [10, 30]");
    }
    if !(5..=15).contains(&exit) {
        anyhow::bail!("invalid exit_channel: out of range [5, 15]");
    }
    if !(20..=100).contains(&baseline) {
        anyhow::bail!("invalid atr_baseline_bars: out of range [20, 100]");
    }
    if exit >= entry {
        anyhow::bail!("invalid exit_channel: must be less than entry_channel");
    }

    Ok(serde_json::json!({
        "entry_channel": entry,
        "exit_channel": exit,
        "atr_baseline_bars": baseline,
    }))
}

fn validate_permissive(
    strategy_name: &str,
    proposed: &serde_json::Value,
    current: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let proposed_obj = proposed
        .as_object()
        .with_context(|| format!("invalid params \"{strategy_name}\": must be a json object"))?;
    if proposed_obj.is_empty() {
        anyhow::bail!("invalid params \"{strategy_name}\": empty object");
    }
    if proposed_obj.values().any(|v| v.is_null()) {
        anyhow::bail!("invalid params \"{strategy_name}\": contains null values");
    }

    if let Some(current_obj) = current.as_object()
        && !current_obj.is_empty()
    {
        use std::collections::BTreeSet;
        let current_keys: BTreeSet<&str> = current_obj.keys().map(String::as_str).collect();
        let proposed_keys: BTreeSet<&str> = proposed_obj.keys().map(String::as_str).collect();
        let added: Vec<&&str> = proposed_keys.difference(&current_keys).collect();
        let removed: Vec<&&str> = current_keys.difference(&proposed_keys).collect();
        if !added.is_empty() || !removed.is_empty() {
            anyhow::bail!(
                "invalid params \"{strategy_name}\": key set differs from current (added={added:?}, removed={removed:?})"
            );
        }
        for (key, current_val) in current_obj {
            let proposed_val = &proposed_obj[key];
            if !json_types_compatible(current_val, proposed_val) {
                anyhow::bail!(
                    "invalid params \"{strategy_name}\": type mismatch for key \"{key}\" (current={current_val}, proposed={proposed_val})"
                );
            }
        }
    }

    tracing::warn!(
        strategy_name = %strategy_name,
        "uses permissive validator (no strict schema); add a dedicated validator before relying on these params in production"
    );
    Ok(proposed.clone())
}

fn json_types_compatible(current: &serde_json::Value, proposed: &serde_json::Value) -> bool {
    use serde_json::Value;
    match (current, proposed) {
        (Value::Number(c), Value::Number(p)) => {
            let current_is_int = c.is_i64() || c.is_u64();
            let proposed_is_int = p.is_i64() || p.is_u64();
            current_is_int == proposed_is_int
        }
        _ => std::mem::discriminant(current) == std::mem::discriminant(proposed),
    }
}

#[cfg(test)]
mod tests;
