use super::*;

#[test]
fn zero_total_returns_zero() {
    assert_eq!(lower_bound_95(0, 0), 0.0);
}

#[test]
fn all_wins_small_sample() {
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
    let lb = lower_bound_95(10, 20);
    assert!(lb > 0.25, "lb={lb}");
    assert!(lb < 0.50, "lb={lb}");
}

#[test]
fn large_sample_converges() {
    let lb = lower_bound_95(700, 1000);
    assert!(lb > 0.66, "lb={lb}");
    assert!(lb < 0.70, "lb={lb}");
}
