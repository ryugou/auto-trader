//! Kill Switch 発動レコード。RiskGate から insert/query される。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RiskHalt {
    pub id: Uuid,
    pub account_id: Uuid,
    pub reason: String,
    pub daily_loss: Decimal,
    pub loss_limit: Decimal,
    pub triggered_at: DateTime<Utc>,
    pub halted_until: DateTime<Utc>,
    pub released_at: Option<DateTime<Utc>>,
}

/// Kill Switch 発動レコードを作成し、id を返す。
pub async fn insert_halt(
    pool: &PgPool,
    account_id: Uuid,
    reason: &str,
    daily_loss: Decimal,
    loss_limit: Decimal,
    halted_until: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO risk_halts
             (account_id, reason, daily_loss, loss_limit, halted_until)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(account_id)
    .bind(reason)
    .bind(daily_loss)
    .bind(loss_limit)
    .bind(halted_until)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// アクティブ (未解除 かつ halted_until > NOW) な halt を1件返す。
pub async fn active_halt_for_account(
    pool: &PgPool,
    account_id: Uuid,
) -> anyhow::Result<Option<RiskHalt>> {
    let halt = sqlx::query_as::<_, RiskHalt>(
        "SELECT id, account_id, reason, daily_loss, loss_limit,
                triggered_at, halted_until, released_at
         FROM risk_halts
         WHERE account_id = $1
           AND released_at IS NULL
           AND halted_until > NOW()
         ORDER BY triggered_at DESC
         LIMIT 1",
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    Ok(halt)
}

/// halt を手動解除。
pub async fn release_halt(pool: &PgPool, halt_id: Uuid) -> anyhow::Result<()> {
    sqlx::query("UPDATE risk_halts SET released_at = NOW() WHERE id = $1 AND released_at IS NULL")
        .bind(halt_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// JST 本日のクローズ済み trade の pnl_amount 合計。
pub async fn daily_realized_pnl_jst(pool: &PgPool, account_id: Uuid) -> anyhow::Result<Decimal> {
    let pnl: Option<Decimal> = sqlx::query_scalar(
        "SELECT SUM(pnl_amount) FROM trades
         WHERE account_id = $1
           AND status = 'closed'
           AND exit_at >= date_trunc('day', NOW() AT TIME ZONE 'Asia/Tokyo')
                          AT TIME ZONE 'Asia/Tokyo'",
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(pnl.unwrap_or(Decimal::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    async fn seed_account(pool: &PgPool) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO trading_accounts (id, name, account_type, exchange, strategy,
                                             initial_balance, current_balance, leverage, currency)
             VALUES ($1, 'test', 'paper', 'bitflyer_cfd', 'donchian_trend_v1',
                     30000, 30000, 2, 'JPY')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn insert_and_fetch_active_halt(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        let halted_until = chrono::Utc::now() + chrono::Duration::hours(24);
        insert_halt(
            &pool,
            account_id,
            "daily_loss_limit_exceeded",
            dec!(-1600),
            dec!(-1500),
            halted_until,
        )
        .await
        .unwrap();
        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_some());
        let halt = active.unwrap();
        assert_eq!(halt.reason, "daily_loss_limit_exceeded");
        assert_eq!(halt.daily_loss, dec!(-1600));
        assert_eq!(halt.loss_limit, dec!(-1500));
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn released_halt_not_returned_as_active(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        let halted_until = chrono::Utc::now() + chrono::Duration::hours(24);
        let halt_id = insert_halt(
            &pool,
            account_id,
            "daily_loss_limit_exceeded",
            dec!(-1600),
            dec!(-1500),
            halted_until,
        )
        .await
        .unwrap();
        release_halt(&pool, halt_id).await.unwrap();
        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_none());
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn expired_halt_not_returned_as_active(pool: PgPool) {
        let account_id = seed_account(&pool).await;
        let past = chrono::Utc::now() - chrono::Duration::minutes(1);
        sqlx::query(
            "INSERT INTO risk_halts
                 (account_id, reason, daily_loss, loss_limit, triggered_at, halted_until)
             VALUES ($1, 'test', 0, -1500, $2, $3)",
        )
        .bind(account_id)
        .bind(past - chrono::Duration::hours(1))
        .bind(past)
        .execute(&pool)
        .await
        .unwrap();
        let active = active_halt_for_account(&pool, account_id).await.unwrap();
        assert!(active.is_none(), "expired halt must not count as active");
    }
}
