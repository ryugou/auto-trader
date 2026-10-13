use super::*;

#[test]
fn effective_jobs_keeps_the_requested_value_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(2, 10, 8), 2);
}

#[test]
fn effective_jobs_clamps_to_available_parallelism_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(16, 10, 8), 8);
}

#[test]
fn effective_jobs_clamps_to_the_run_count_when_it_is_the_smallest() {
    assert_eq!(effective_jobs(16, 3, 8), 3);
}

#[test]
fn effective_jobs_returns_one_when_requested_is_zero() {
    assert_eq!(effective_jobs(0, 5, 8), 1);
}

#[test]
fn effective_jobs_returns_one_when_the_run_count_is_zero() {
    assert_eq!(effective_jobs(4, 0, 8), 1);
}

#[test]
fn effective_jobs_returns_one_when_available_parallelism_is_zero() {
    assert_eq!(effective_jobs(4, 10, 0), 1);
}
