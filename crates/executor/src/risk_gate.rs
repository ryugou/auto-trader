use chrono::{DateTime, Duration, NaiveTime, TimeZone, Utc};
use rust_decimal::Decimal;

const JST_OFFSET_HOURS: i64 = 9;

#[derive(Debug)]
pub enum GateDecision {
    Pass,
    Reject(RejectReason),
}

#[derive(Debug, Clone)]
pub enum RejectReason {
    PriceTickStale {
        age_secs: u64,
    },
    DailyLossLimit {
        day_net: Decimal,
        limit_amount: Decimal,
    },
    Halted {
        until: DateTime<Utc>,
    },
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PriceTickStale { .. } => "price_tick_stale",
            Self::DailyLossLimit { .. } => "daily_loss_limit",
            Self::Halted { .. } => "halted",
        }
    }
}

pub fn eval_price_freshness(price_freshness_secs: u64, age_secs: u64) -> GateDecision {
    if age_secs > price_freshness_secs {
        GateDecision::Reject(RejectReason::PriceTickStale { age_secs })
    } else {
        GateDecision::Pass
    }
}

pub fn jst_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let jst = now + Duration::hours(JST_OFFSET_HOURS);
    let day_start_jst = jst.date_naive().and_time(NaiveTime::MIN);
    Utc.from_utc_datetime(&(day_start_jst - Duration::hours(JST_OFFSET_HOURS)))
}

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
