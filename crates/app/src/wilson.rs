pub fn lower_bound(wins: u64, total: u64, z: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let p = wins as f64 / n;
    let z2 = z * z;
    let numerator = p + z2 / (2.0 * n) - z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt());
    let denominator = 1.0 + z2 / n;
    (numerator / denominator).max(0.0)
}

pub fn lower_bound_95(wins: u64, total: u64) -> f64 {
    lower_bound(wins, total, 1.96)
}

#[cfg(test)]
mod tests;
