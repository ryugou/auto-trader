//! Signal → Executor の間の前段ガード。paper/live 共通。

use auto_trader_core::types::Signal;
use auto_trader_db::risk_halts;
use auto_trader_db::trading_accounts::TradingAccount;
use auto_trader_notify::{
    KillSwitchReleasedEvent, KillSwitchTriggeredEvent, Notifier, NotifyEvent,
};
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Utc};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RiskGateConfig {
    pub daily_loss_limit_pct: Decimal,
    pub price_freshness_secs: u64,
}

#[derive(Debug)]
pub enum GateDecision {
    Pass,
    Reject(RejectReason),
}

#[derive(Debug, Clone)]
pub enum RejectReason {
    DailyLossLimitExceeded { loss: Decimal, limit: Decimal },
    PriceTickStale { age_secs: u64 },
    DuplicatePosition { existing_trade_id: Uuid },
    KillSwitchActive { until: DateTime<Utc> },
}

impl RejectReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DailyLossLimitExceeded { .. } => "daily_loss_limit_exceeded",
            Self::PriceTickStale { .. } => "price_tick_stale",
            Self::DuplicatePosition { .. } => "duplicate_position",
            Self::KillSwitchActive { .. } => "kill_switch_active",
        }
    }
}

pub struct RiskGate {
    pool: PgPool,
    notifier: Arc<Notifier>,
    config: RiskGateConfig,
    release_jst_hour: u32,
}

impl RiskGate {
    pub fn new(
        pool: PgPool,
        notifier: Arc<Notifier>,
        config: RiskGateConfig,
        release_jst_hour: u32,
    ) -> Self {
        Self {
            pool,
            notifier,
            config,
            release_jst_hour,
        }
    }

    pub async fn check(
        &self,
        signal: &Signal,
        account: &TradingAccount,
        last_tick_age_secs: u64,
        current_unrealized: Decimal,
    ) -> anyhow::Result<GateDecision> {
        // 1) freshness
        if let GateDecision::Reject(r) =
            Self::eval_price_freshness(&self.config, last_tick_age_secs)
        {
            return Ok(GateDecision::Reject(r));
        }
        // 2) kill switch: fetch latest unreleased halt regardless of expiry
        if let Some(halt) = risk_halts::latest_unreleased_halt(&self.pool, account.id).await? {
            let now = Utc::now();
            if halt.halted_until > now {
                return Ok(GateDecision::Reject(RejectReason::KillSwitchActive {
                    until: halt.halted_until,
                }));
            }
            // Halt expired — release it so the partial index stays bounded
            // and we have a clean audit trail. Fire KillSwitchReleased notify
            // only when this caller actually transitioned the row (not a
            // concurrent duplicate release).
            let released = risk_halts::release_halt(&self.pool, halt.id).await?;
            if released {
                let ev = NotifyEvent::KillSwitchReleased(KillSwitchReleasedEvent {
                    account_name: account.name.clone(),
                });
                if let Err(e) = self.notifier.send(ev).await {
                    tracing::error!(
                        "risk_gate: KillSwitchReleased notify failed for {}: {e}",
                        account.name
                    );
                }
            }
            // Fall through to other gates.
        }
        // 3) duplicate position
        let existing = auto_trader_db::trades::find_open_for_strategy_pair(
            &self.pool,
            account.id,
            &signal.strategy_name,
            &signal.pair.0,
        )
        .await?;
        if let Some(trade_id) = existing {
            return Ok(GateDecision::Reject(RejectReason::DuplicatePosition {
                existing_trade_id: trade_id,
            }));
        }
        // 4) kill switch evaluation + insert + notify
        let realized = risk_halts::daily_realized_pnl_jst(&self.pool, account.id).await?;
        if let GateDecision::Reject(RejectReason::DailyLossLimitExceeded { loss, limit }) =
            Self::eval_kill_switch(
                &self.config,
                account.initial_balance,
                realized,
                current_unrealized,
            )
        {
            let halted_until = self.compute_halted_until(Utc::now());
            let inserted = risk_halts::insert_halt(
                &self.pool,
                account.id,
                "daily_loss_limit_exceeded",
                loss,
                limit,
                halted_until,
            )
            .await?;
            // Only notify when we actually created a fresh halt.
            // Concurrent second path gets None (ON CONFLICT DO NOTHING) — silent.
            if inserted.is_some() {
                let ev = NotifyEvent::KillSwitchTriggered(KillSwitchTriggeredEvent {
                    account_name: account.name.clone(),
                    daily_loss: loss,
                    limit,
                    halted_until,
                });
                if let Err(e) = self.notifier.send(ev).await {
                    tracing::error!("risk_gate: KillSwitchTriggered notify failed: {e}");
                }
            }
            return Ok(GateDecision::Reject(RejectReason::DailyLossLimitExceeded {
                loss,
                limit,
            }));
        }
        Ok(GateDecision::Pass)
    }

    pub fn eval_price_freshness(cfg: &RiskGateConfig, age_secs: u64) -> GateDecision {
        if age_secs > cfg.price_freshness_secs {
            GateDecision::Reject(RejectReason::PriceTickStale { age_secs })
        } else {
            GateDecision::Pass
        }
    }

    pub fn eval_kill_switch(
        cfg: &RiskGateConfig,
        initial_balance: Decimal,
        realized: Decimal,
        unrealized: Decimal,
    ) -> GateDecision {
        let limit_abs = initial_balance * cfg.daily_loss_limit_pct;
        let loss_limit = -limit_abs;
        let total = realized + unrealized;
        if total <= loss_limit {
            GateDecision::Reject(RejectReason::DailyLossLimitExceeded {
                loss: total,
                limit: loss_limit,
            })
        } else {
            GateDecision::Pass
        }
    }

    fn compute_halted_until(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let jst: FixedOffset = FixedOffset::east_opt(9 * 3600).unwrap();
        let jst_now = now.with_timezone(&jst);
        let today_date = jst_now.date_naive();
        let today_release = jst
            .with_ymd_and_hms(
                today_date.year(),
                today_date.month(),
                today_date.day(),
                self.release_jst_hour,
                0,
                0,
            )
            .single()
            .expect("release hour 0..=23 always valid");
        let target = if today_release > jst_now {
            today_release
        } else {
            today_release + Duration::days(1)
        };
        target.with_timezone(&Utc)
    }
}
