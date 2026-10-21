/// Result of a risk gate check.
#[derive(Debug)]
pub enum GateDecision {
    /// The order may be placed.
    Pass,
    /// The order must not be placed, for the given reason.
    Reject(RejectReason),
}

/// Reason a risk gate check rejected an order.
#[derive(Debug, Clone)]
pub enum RejectReason {
    /// The latest price tick is older than the allowed freshness.
    PriceTickStale {
        /// Age of the latest price tick in seconds.
        age_secs: u64,
    },
}

impl RejectReason {
    /// Returns the snake_case name of the reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PriceTickStale { .. } => "price_tick_stale",
        }
    }
}

/// Rejects when the age of the latest price tick exceeds `price_freshness_secs`.
pub fn eval_price_freshness(price_freshness_secs: u64, age_secs: u64) -> GateDecision {
    if age_secs > price_freshness_secs {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs })
    } else {
        GateDecision::Pass
    }
}
