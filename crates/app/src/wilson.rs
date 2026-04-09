// Used from weekly_batch.rs (Task 6) and main.rs wiring (Task 7).
#![allow(dead_code)]

/// Wilson Score lower bound for a binomial proportion.
///
/// Gives a conservative estimate of the true win rate given
/// observed wins/total and a confidence level. Useful for comparing
/// win rates from small samples — a strategy with 10 wins out of 10
/// has a much lower lower-bound than 100 wins out of 100.
///
/// # Arguments
/// * `wins` — number of successes
/// * `total` — total trials
/// * `z` — z-score for confidence level (1.96 for 95%)
pub fn lower_bound(wins: u64, total: u64, z: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let p = wins as f64 / n;
    let z2 = z * z;
    let numerator = p + z2 / (2.0 * n)
        - z * ((p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt());
    let denominator = 1.0 + z2 / n;
    (numerator / denominator).max(0.0)
}

/// Convenience: 95% confidence Wilson lower bound (z = 1.96).
pub fn lower_bound_95(wins: u64, total: u64) -> f64 {
    lower_bound(wins, total, 1.96)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_total_returns_zero() {
        assert_eq!(lower_bound_95(0, 0), 0.0);
    }

    #[test]
    fn all_wins_small_sample() {
        // 3 wins out of 3 → lower bound should still be < 1.0
        let lb = lower_bound_95(3, 3);
        assert!(lb > 0.3, "lb={lb}");
        assert!(lb < 1.0, "lb={lb}");
    }

    #[test]
    fn no_wins_returns_near_zero() {
        let lb = lower_bound_95(0, 10);
        assert!(lb < 0.05, "lb={lb}");
    }

    #[test]
    fn fifty_percent_moderate_sample() {
        // 10 wins out of 20 → lb should be meaningfully below 0.5
        let lb = lower_bound_95(10, 20);
        assert!(lb > 0.25, "lb={lb}");
        assert!(lb < 0.50, "lb={lb}");
    }

    #[test]
    fn large_sample_converges() {
        // 700 wins out of 1000 → lb should be close to 0.70
        let lb = lower_bound_95(700, 1000);
        assert!(lb > 0.66, "lb={lb}");
        assert!(lb < 0.70, "lb={lb}");
    }
}
