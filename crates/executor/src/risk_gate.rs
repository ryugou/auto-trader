//! Entry-path price-tick freshness gate.
//!
//! Rejects signals when the most recent price tick for the signal's pair is
//! older than `price_freshness_secs`. Ensures entry decisions are based on
//! sufficiently recent market data.

/// Outcome of the freshness check.
#[derive(Debug)]
pub enum GateDecision {
    Pass,
    Reject(RejectReason),
}

#[derive(Debug, Clone)]
pub enum RejectReason {
    PriceTickStale { age_secs: u64 },
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PriceTickStale { .. } => "price_tick_stale",
        }
    }
}

/// Pure function — no I/O, no state.
///
/// Returns `GateDecision::Reject` when `age_secs > price_freshness_secs`.
pub fn eval_price_freshness(price_freshness_secs: u64, age_secs: u64) -> GateDecision {
    if age_secs > price_freshness_secs {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs })
    } else {
        GateDecision::Pass
    }
}
