use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use rust_decimal::Decimal;

const JST_OFFSET_HOURS: i64 = 9;

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
    /// The loss of the current day has reached the daily limit.
    DailyLossLimit {
        /// Net profit or loss of the current day.
        day_net: Decimal,
        /// Loss amount at which trading stops.
        limit_amount: Decimal,
    },
    /// Trading is halted until the given time.
    Halted {
        /// Time at which the halt ends.
        until: DateTime<Utc>,
    },
}

impl RejectReason {
    /// Returns the snake_case name of the reason.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PriceTickStale { .. } => "price_tick_stale",
            Self::DailyLossLimit { .. } => "daily_loss_limit",
            Self::Halted { .. } => "halted",
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

/// Returns the UTC time of the most recent midnight in JST at or before `now`.
pub fn jst_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let jst = now + Duration::hours(JST_OFFSET_HOURS);
    let day_start_jst = jst.date_naive().and_time(NaiveTime::MIN);
    Utc.from_utc_datetime(&(day_start_jst - Duration::hours(JST_OFFSET_HOURS)))
}

/// Rejects when `day_net` is at or below the negative of `limit_pct` times `day_start_balance`; passes when `day_start_balance` is not positive.
pub fn eval_daily_loss(
    day_net: Decimal,
    day_start_balance: Decimal,
    limit_pct: Decimal,
) -> GateDecision {
    if day_start_balance <= Decimal::ZERO {
        return GateDecision::Pass;
    }
    let limit_amount = day_start_balance * limit_pct;
    if day_net <= -limit_amount {
        GateDecision::Reject(RejectReason::DailyLossLimit {
            day_net,
            limit_amount,
        })
    } else {
        GateDecision::Pass
    }
}

#[cfg(test)]
mod tests;
