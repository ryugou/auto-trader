//! Entry-path risk gates.
//!
//! - Price-tick freshness: rejects signals when the most recent price tick for
//!   the signal's pair is older than `price_freshness_secs`.
//! - Daily loss kill switch: rejects entries when the account's realized loss
//!   for the JST trading day has breached `day_start_balance × limit_pct`.
//!
//! All eval functions here are pure (no I/O, no state) so they are trivially
//! unit-testable; the caller wires DB reads / halt persistence around them.

use chrono::{DateTime, Duration, TimeZone, Utc};
use rust_decimal::Decimal;

/// Outcome of a risk-gate check.
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

/// JST (UTC+9) の当日 00:00 を UTC で返す。Kill Switch の日次区切り。
pub fn jst_day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let jst = now + Duration::hours(9);
    let day_start_jst = jst
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("00:00:00 is valid");
    Utc.from_utc_datetime(&(day_start_jst - Duration::hours(9)))
}

/// 日次損失上限の判定。pure 関数。
/// `day_net <= -(day_start_balance × limit_pct)` で Reject。
/// day_start_balance <= 0 は判定不能なので Pass。
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
mod daily_loss_tests {
    use super::*;
    use chrono::TimeZone;
    use rust_decimal_macros::dec;

    #[test]
    fn jst_day_start_is_15h_utc_of_previous_day() {
        // 2026-07-07 02:00 UTC == 2026-07-07 11:00 JST → JST day start is
        // 2026-07-07 00:00 JST == 2026-07-06 15:00 UTC.
        let now = Utc.with_ymd_and_hms(2026, 7, 7, 2, 0, 0).unwrap();
        let start = jst_day_start(now);
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 7, 6, 15, 0, 0).unwrap());
    }

    #[test]
    fn rejects_at_exactly_limit_and_beyond() {
        // start 100_000, limit 5% => limit_amount 5_000.
        let balance = dec!(100000);
        let pct = dec!(0.05);
        // exactly at limit (day_net == -5000) rejects.
        assert!(matches!(
            eval_daily_loss(dec!(-5000), balance, pct),
            GateDecision::Reject(RejectReason::DailyLossLimit { .. })
        ));
        // beyond limit rejects.
        assert!(matches!(
            eval_daily_loss(dec!(-5001), balance, pct),
            GateDecision::Reject(RejectReason::DailyLossLimit { .. })
        ));
    }

    #[test]
    fn passes_below_limit_and_on_profit() {
        let balance = dec!(100000);
        let pct = dec!(0.05);
        // -4999 is above the -5000 threshold → pass.
        assert!(matches!(
            eval_daily_loss(dec!(-4999), balance, pct),
            GateDecision::Pass
        ));
        // profit → pass.
        assert!(matches!(
            eval_daily_loss(dec!(10000), balance, pct),
            GateDecision::Pass
        ));
    }

    #[test]
    fn passes_when_day_start_balance_non_positive() {
        let pct = dec!(0.05);
        assert!(matches!(
            eval_daily_loss(dec!(-99999), dec!(0), pct),
            GateDecision::Pass
        ));
        assert!(matches!(
            eval_daily_loss(dec!(-99999), dec!(-100), pct),
            GateDecision::Pass
        ));
    }
}
