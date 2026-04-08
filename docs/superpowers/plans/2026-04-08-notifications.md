# In-App Notifications Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an in-app notification system: bell icon + unread badge in the dashboard header, dropdown with recent trade open/close notifications, `/notifications` full-history page, and DB-backed read/unread state.

**Architecture:** New `notifications` table in Postgres. Both paper-trade open paths (`execute_with_quantity` and `execute`) insert a `trade_opened` notification inside the same DB transaction as the trade row; `close_position` does the same for `trade_closed`. A new `/api/notifications` REST surface exposes list / unread-count / mark-all-read. The React dashboard adds a `NotificationBell` component that polls the unread count every 15 s and renders a dropdown which fires a single `mark-all-read` on open.

**Tech Stack:** Rust (axum, sqlx, chrono, rust_decimal, anyhow), Postgres, React 19 + TanStack Query v5, Tailwind v4. No frontend test framework is installed — verification is `cargo check --workspace` + `cargo clippy -- -D warnings` + `cargo test` + `npm run lint` + `npm run build` + manual smoke.

**Spec:** `docs/superpowers/specs/2026-04-08-notifications-design.md`

---

## File Structure

**New files:**
- `migrations/20260408000001_notifications.sql` — DDL for the `notifications` table
- `crates/db/src/notifications.rs` — DB module (types + queries)
- `crates/app/src/api/notifications.rs` — HTTP handlers
- `dashboard-ui/src/components/NotificationBell.tsx` — Header bell + badge + dropdown toggle
- `dashboard-ui/src/components/NotificationDropdown.tsx` — Dropdown panel with latest 20 items
- `dashboard-ui/src/pages/Notifications.tsx` — Full-history page with filters + paging

**Modified files:**
- `crates/db/src/lib.rs` — declare `pub mod notifications`
- `crates/executor/src/paper.rs` — call `notifications::insert_trade_opened` in both `execute` and `execute_with_quantity`; call `notifications::insert_trade_closed` in `close_position`; wrap `execute` in a transaction (currently uses a bare `insert_trade` on a fresh connection)
- `crates/app/src/api/mod.rs` — declare `mod notifications`, register routes
- `crates/app/src/main.rs` — daily batch calls `notifications::purge_old_read`
- `dashboard-ui/src/App.tsx` — render `<NotificationBell />` in the header, add `/notifications` route
- `dashboard-ui/src/api/types.ts` — `Notification`, `NotificationsResponse`, `NotificationUnreadCountResponse` types
- `dashboard-ui/src/api/client.ts` — `api.notifications.{ list, unreadCount, markAllRead }`

---

## Task 1: Database migration

**Files:**
- Create: `migrations/20260408000001_notifications.sql`

- [ ] **Step 1: Write the migration SQL**

```sql
-- In-app notification log for trade open / close events.
-- Unread notifications are kept forever; read notifications are
-- purged after 30 days by the daily batch. Display fields are
-- denormalized (copied from trades + paper_accounts at write time)
-- so the dashboard dropdown can render without a JOIN.
CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind TEXT NOT NULL CHECK (kind IN ('trade_opened', 'trade_closed')),
    trade_id UUID NOT NULL REFERENCES trades(id) ON DELETE CASCADE,
    paper_account_id UUID NOT NULL,
    strategy_name TEXT NOT NULL,
    pair TEXT NOT NULL,
    direction TEXT NOT NULL,
    price NUMERIC NOT NULL,
    pnl_amount NUMERIC,
    exit_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    read_at TIMESTAMPTZ,
    -- A `trade_closed` notification must carry pnl_amount + exit_reason;
    -- a `trade_opened` notification must leave them NULL. Enforce at
    -- schema level so callers can't accidentally mix the two shapes.
    CONSTRAINT notifications_kind_fields CHECK (
        (kind = 'trade_opened' AND pnl_amount IS NULL AND exit_reason IS NULL)
        OR
        (kind = 'trade_closed' AND pnl_amount IS NOT NULL AND exit_reason IS NOT NULL)
    )
);

CREATE INDEX idx_notifications_created_at ON notifications (created_at DESC);
-- Partial index so `SELECT COUNT(*) WHERE read_at IS NULL` (the bell
-- badge query) stays O(unread) instead of O(total_history).
CREATE INDEX idx_notifications_unread ON notifications (read_at) WHERE read_at IS NULL;
```

- [ ] **Step 2: Commit the migration**

```bash
git add migrations/20260408000001_notifications.sql
git commit -m "feat(db): add notifications table for trade open/close alerts"
```

---

## Task 2: `notifications` DB module

**Files:**
- Create: `crates/db/src/notifications.rs`
- Modify: `crates/db/src/lib.rs`

- [ ] **Step 1: Add the module declaration to `crates/db/src/lib.rs`**

Replace the file contents with:

```rust
pub mod candles;
pub mod dashboard;
pub mod macro_events;
pub mod notifications;
pub mod paper_accounts;
pub mod pool;
pub mod strategies;
pub mod summary;
pub mod trades;
```

- [ ] **Step 2: Create `crates/db/src/notifications.rs`**

```rust
use auto_trader_core::types::{Direction, ExitReason, Trade};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub trade_id: Uuid,
    pub paper_account_id: Uuid,
    pub strategy_name: String,
    pub pair: String,
    pub direction: String,
    pub price: Decimal,
    pub pnl_amount: Option<Decimal>,
    pub exit_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
}

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Long => "long",
        Direction::Short => "short",
    }
}

fn exit_reason_str(r: ExitReason) -> String {
    serde_json::to_string(&r)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Insert a `trade_opened` notification. Must be called with the same
/// executor (usually a `&mut tx`) that wrote the `trades` row so that
/// the two live or die together.
pub async fn insert_trade_opened<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let account_id = trade
        .paper_account_id
        .ok_or_else(|| anyhow::anyhow!("trade has no paper_account_id"))?;
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price)
           VALUES ('trade_opened', $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(trade.id)
    .bind(account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(direction_str(trade.direction))
    .bind(trade.entry_price)
    .execute(executor)
    .await?;
    Ok(())
}

/// Insert a `trade_closed` notification. `trade` must have `exit_price`,
/// `pnl_amount`, and `exit_reason` populated.
pub async fn insert_trade_closed<'e, E>(executor: E, trade: &Trade) -> anyhow::Result<()>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let account_id = trade
        .paper_account_id
        .ok_or_else(|| anyhow::anyhow!("trade has no paper_account_id"))?;
    let price = trade
        .exit_price
        .ok_or_else(|| anyhow::anyhow!("closed trade has no exit_price"))?;
    let pnl = trade
        .pnl_amount
        .ok_or_else(|| anyhow::anyhow!("closed trade has no pnl_amount"))?;
    let reason = trade
        .exit_reason
        .ok_or_else(|| anyhow::anyhow!("closed trade has no exit_reason"))?;
    sqlx::query(
        r#"INSERT INTO notifications
               (kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason)
           VALUES ('trade_closed', $1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(trade.id)
    .bind(account_id)
    .bind(&trade.strategy_name)
    .bind(&trade.pair.0)
    .bind(direction_str(trade.direction))
    .bind(price)
    .bind(pnl)
    .bind(exit_reason_str(reason))
    .execute(executor)
    .await?;
    Ok(())
}

/// Paginated list with optional filters. Dates are interpreted as JST
/// (UTC+9) day boundaries to match the rest of the dashboard — a
/// `from = 2026-04-08` means "trades created from 2026-04-08 00:00 JST".
pub async fn list(
    pool: &PgPool,
    limit: i64,
    offset: i64,
    unread_only: bool,
    kind_filter: Option<&str>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<(Vec<Notification>, i64)> {
    // JST day boundaries -> UTC timestamps for indexed comparisons.
    let jst_offset = chrono::FixedOffset::east_opt(9 * 3600)
        .expect("fixed offset");
    let from_ts = from.map(|d| {
        d.and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(jst_offset)
            .unwrap()
            .with_timezone(&Utc)
    });
    let to_ts = to.map(|d| {
        (d + chrono::Duration::days(1))
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_local_timezone(jst_offset)
            .unwrap()
            .with_timezone(&Utc)
    });

    let mut sql = String::from(
        "SELECT id, kind, trade_id, paper_account_id, strategy_name, pair,
                direction, price, pnl_amount, exit_reason, created_at, read_at
         FROM notifications WHERE 1=1",
    );
    if unread_only {
        sql.push_str(" AND read_at IS NULL");
    }
    if kind_filter.is_some() {
        sql.push_str(" AND kind = $1");
    }
    // Placeholders need to be numbered from 1 and renumbered based on
    // whether kind_filter was included — do it explicitly rather than
    // guessing.
    let mut placeholder = if kind_filter.is_some() { 2 } else { 1 };
    if from_ts.is_some() {
        sql.push_str(&format!(" AND created_at >= ${placeholder}"));
        placeholder += 1;
    }
    if to_ts.is_some() {
        sql.push_str(&format!(" AND created_at < ${placeholder}"));
        placeholder += 1;
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC LIMIT ${} OFFSET ${}",
        placeholder,
        placeholder + 1
    ));

    let mut q = sqlx::query_as::<_, Notification>(&sql);
    if let Some(k) = kind_filter {
        q = q.bind(k);
    }
    if let Some(f) = from_ts {
        q = q.bind(f);
    }
    if let Some(t) = to_ts {
        q = q.bind(t);
    }
    q = q.bind(limit).bind(offset);
    let items = q.fetch_all(pool).await?;

    // Total count with the same filters (ignoring limit/offset).
    let mut count_sql = String::from("SELECT COUNT(*) FROM notifications WHERE 1=1");
    if unread_only {
        count_sql.push_str(" AND read_at IS NULL");
    }
    if kind_filter.is_some() {
        count_sql.push_str(" AND kind = $1");
    }
    let mut count_ph = if kind_filter.is_some() { 2 } else { 1 };
    if from_ts.is_some() {
        count_sql.push_str(&format!(" AND created_at >= ${count_ph}"));
        count_ph += 1;
    }
    if to_ts.is_some() {
        count_sql.push_str(&format!(" AND created_at < ${count_ph}"));
    }
    let mut cq = sqlx::query_scalar::<_, i64>(&count_sql);
    if let Some(k) = kind_filter {
        cq = cq.bind(k);
    }
    if let Some(f) = from_ts {
        cq = cq.bind(f);
    }
    if let Some(t) = to_ts {
        cq = cq.bind(t);
    }
    let total: i64 = cq.fetch_one(pool).await?;

    Ok((items, total))
}

pub async fn unread_count(pool: &PgPool) -> anyhow::Result<i64> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE read_at IS NULL")
        .fetch_one(pool)
        .await?;
    Ok(n)
}

pub async fn mark_all_read(pool: &PgPool) -> anyhow::Result<i64> {
    let result = sqlx::query("UPDATE notifications SET read_at = NOW() WHERE read_at IS NULL")
        .execute(pool)
        .await?;
    Ok(result.rows_affected() as i64)
}

/// Delete read notifications older than 30 days. Returns rows deleted.
pub async fn purge_old_read(pool: &PgPool) -> anyhow::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM notifications
         WHERE read_at IS NOT NULL
           AND read_at < NOW() - INTERVAL '30 days'",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use auto_trader_core::types::Direction;

    #[test]
    fn direction_str_maps_long_short() {
        assert_eq!(direction_str(Direction::Long), "long");
        assert_eq!(direction_str(Direction::Short), "short");
    }

    #[test]
    fn exit_reason_str_strips_quotes() {
        let s = exit_reason_str(ExitReason::StopLoss);
        // Whatever the serde encoding is, the outer quotes must be gone.
        assert!(!s.starts_with('"'));
        assert!(!s.ends_with('"'));
        assert!(!s.is_empty());
    }
}
```

- [ ] **Step 3: Run `cargo check -p auto-trader-db` to confirm the module compiles**

Run: `cargo check -p auto-trader-db`
Expected: PASS (0 errors).

If errors reference unknown `Trade` fields, re-read `crates/core/src/types.rs` and adjust.

- [ ] **Step 4: Run the unit tests in the module**

Run: `cargo test -p auto-trader-db notifications::tests --lib`
Expected: 2 tests passing.

- [ ] **Step 5: Commit**

```bash
git add crates/db/src/notifications.rs crates/db/src/lib.rs
git commit -m "feat(db): add notifications module with insert/list/mark-read/purge"
```

---

## Task 3: Emit notifications from the paper executor

**Files:**
- Modify: `crates/executor/src/paper.rs`

- [ ] **Step 1: Add notification insert to `execute_with_quantity`**

Find the block around `crates/executor/src/paper.rs:122` that reads:

```rust
        let mut tx = self.pool.begin().await?;
        auto_trader_db::trades::insert_trade_with_executor(&mut *tx, &trade).await?;
```

Replace with:

```rust
        let mut tx = self.pool.begin().await?;
        auto_trader_db::trades::insert_trade_with_executor(&mut *tx, &trade).await?;
        auto_trader_db::notifications::insert_trade_opened(&mut *tx, &trade).await?;
```

- [ ] **Step 2: Convert `execute` (FX path) to use a transaction and emit a notification**

Find the `execute` method at roughly `crates/executor/src/paper.rs:270` and replace the current body from `let trade = Trade { ... };` through `auto_trader_db::trades::insert_trade(&self.pool, &trade).await?;` with:

```rust
        let trade = Trade {
            id: Uuid::new_v4(),
            strategy_name: signal.strategy_name.clone(),
            pair: signal.pair.clone(),
            exchange: self.exchange,
            direction: signal.direction,
            entry_price: signal.entry_price,
            exit_price: None,
            stop_loss: signal.stop_loss,
            take_profit: signal.take_profit,
            quantity: None,
            leverage,
            fees: Decimal::ZERO,
            paper_account_id: Some(self.paper_account_id),
            entry_at: Utc::now(),
            exit_at: None,
            pnl_pips: None,
            pnl_amount: None,
            exit_reason: None,
            mode: TradeMode::Paper,
            status: TradeStatus::Open,
            max_hold_until: signal.max_hold_until,
        };

        // Wrap the trade insert + notification insert in a single tx so
        // the two states never disagree. FX has no margin lock so this
        // is a two-statement transaction; crypto does the same dance
        // with more work inside (see `execute_with_quantity`).
        let mut tx = self.pool.begin().await?;
        auto_trader_db::trades::insert_trade_with_executor(&mut *tx, &trade).await?;
        auto_trader_db::notifications::insert_trade_opened(&mut *tx, &trade).await?;
        tx.commit().await?;
```

- [ ] **Step 3: Emit a `trade_closed` notification from `close_position`**

Find the block in `close_position` (around line 471) that ends with `tx.commit().await?;` after the `trade_close` event insert. **Before** `tx.commit().await?;`, and **after** constructing the final `Trade { ... }` struct at the bottom of the function (around line 475-500)…

Actually the struct is built *after* the commit. We need the struct *before* commit. Rewrite the close path so the `Trade { ... }` that the function returns is built **before** the commit, then insert the notification using that in-transaction struct, then commit.

Specifically, find the sequence:

```rust
        sqlx::query(
            r#"INSERT INTO paper_account_events
                   (paper_account_id, event_type, amount, occurred_at, reference_id)
               VALUES ($1, 'trade_close', $2, $3, $4)"#,
        )
        .bind(self.paper_account_id)
        .bind(pnl_amount)
        .bind(exit_at)
        .bind(locked.id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let trade = Trade {
```

…and replace with:

```rust
        sqlx::query(
            r#"INSERT INTO paper_account_events
                   (paper_account_id, event_type, amount, occurred_at, reference_id)
               VALUES ($1, 'trade_close', $2, $3, $4)"#,
        )
        .bind(self.paper_account_id)
        .bind(pnl_amount)
        .bind(exit_at)
        .bind(locked.id)
        .execute(&mut *tx)
        .await?;

        // Build the closed-trade view *before* commit so we can emit
        // the notification inside the same tx. The struct is also the
        // return value for the caller below.
        let trade = Trade {
```

Then scroll down to the matching closing brace of the `let trade = Trade { ... };` block and find the `Ok(trade)` at the end of the function. Insert a notification emit + commit just before `Ok(trade)`. Specifically, change:

```rust
        };

        Ok(trade)
    }
```

to:

```rust
        };

        auto_trader_db::notifications::insert_trade_closed(&mut *tx, &trade).await?;
        tx.commit().await?;

        Ok(trade)
    }
```

**Important:** The original code already had `tx.commit().await?;` *before* constructing the `Trade { ... }` — you removed that commit in the previous replacement, so now the only commit in the function is the new one at the end. Double-check by running `grep -c 'tx.commit' crates/executor/src/paper.rs` after editing; the count for `close_position` should match what it was minus 1 from the removal and plus 1 from the re-add (net same file-level total).

- [ ] **Step 4: Verify the executor compiles**

Run: `cargo check -p auto-trader-executor`
Expected: PASS.

If you get an "unused variable" or "borrow after commit" error in `close_position`, re-read Step 3 carefully — the order is: INSERT events → build trade struct → insert_trade_closed → commit → return.

- [ ] **Step 5: Run the full workspace check + clippy + test**

Run: `cargo check --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/executor/src/paper.rs
git commit -m "feat(executor): emit trade_opened/trade_closed notifications atomically"
```

---

## Task 4: Notifications API

**Files:**
- Create: `crates/app/src/api/notifications.rs`
- Modify: `crates/app/src/api/mod.rs`

- [ ] **Step 1: Create `crates/app/src/api/notifications.rs`**

```rust
use super::{ApiError, AppState};
use auto_trader_db::notifications::{self as notifs_db, Notification};
use axum::extract::{Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Default)]
pub struct NotificationsFilter {
    pub unread_only: Option<bool>,
    pub limit: Option<i64>,
    pub page: Option<i64>,
    pub kind: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NotificationsResponse {
    pub items: Vec<Notification>,
    pub total: i64,
    pub unread_count: i64,
    pub page: i64,
    pub limit: i64,
}

#[derive(Debug, Serialize)]
pub struct UnreadCountResponse {
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct MarkReadResponse {
    pub marked: i64,
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

pub async fn list(
    State(state): State<AppState>,
    Query(filter): Query<NotificationsFilter>,
) -> Result<Json<NotificationsResponse>, ApiError> {
    let page = filter.page.unwrap_or(1).max(1);
    let limit = filter.limit.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * limit;
    let unread_only = filter.unread_only.unwrap_or(false);
    let from = filter.from.as_deref().and_then(parse_date);
    let to = filter.to.as_deref().and_then(parse_date);

    // kind must be one of the two known values or None — reject
    // anything else so a typo can't silently collapse to "no match".
    let kind = match filter.kind.as_deref() {
        None | Some("") => None,
        Some(k @ "trade_opened") | Some(k @ "trade_closed") => Some(k),
        Some(_) => {
            return Err(ApiError(
                axum::http::StatusCode::BAD_REQUEST,
                "invalid kind (expected trade_opened | trade_closed)".to_string(),
            ));
        }
    };

    let (items, total) =
        notifs_db::list(&state.pool, limit, offset, unread_only, kind, from, to)
            .await
            .map_err(ApiError::from)?;
    let unread_count = notifs_db::unread_count(&state.pool)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(NotificationsResponse {
        items,
        total,
        unread_count,
        page,
        limit,
    }))
}

pub async fn unread_count(
    State(state): State<AppState>,
) -> Result<Json<UnreadCountResponse>, ApiError> {
    let count = notifs_db::unread_count(&state.pool)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(UnreadCountResponse { count }))
}

pub async fn mark_all_read(
    State(state): State<AppState>,
) -> Result<Json<MarkReadResponse>, ApiError> {
    let marked = notifs_db::mark_all_read(&state.pool)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(MarkReadResponse { marked }))
}
```

- [ ] **Step 2: Register the routes in `crates/app/src/api/mod.rs`**

Add `mod notifications;` next to the existing module declarations at the top (around line 1-6) so the file starts with:

```rust
mod accounts;
mod dashboard;
pub(crate) mod filters;
mod notifications;
mod positions;
mod strategies;
mod trades;
```

Add these routes to the `api_routes` builder inside `router()`, immediately before the `.layer(middleware::from_fn(...))` call:

```rust
        .route("/notifications", get(notifications::list))
        .route("/notifications/unread-count", get(notifications::unread_count))
        .route("/notifications/mark-all-read", axum::routing::post(notifications::mark_all_read))
```

- [ ] **Step 3: Verify the app compiles**

Run: `cargo check -p auto-trader && cargo clippy -p auto-trader -- -D warnings`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/api/notifications.rs crates/app/src/api/mod.rs
git commit -m "feat(api): add /api/notifications endpoints (list / unread-count / mark-all-read)"
```

---

## Task 5: Daily purge of old read notifications

**Files:**
- Modify: `crates/app/src/main.rs`

- [ ] **Step 1: Add the purge call to the daily batch**

Find the daily batch startup section around `crates/app/src/main.rs:1044-1055`. The current code is:

```rust
        let backfill_days: i64 = config.monitor.backfill_days.unwrap_or(7) as i64;
        for i in (1..=backfill_days).rev() {
            let d = today - chrono::Duration::days(i);
            tracing::info!("daily batch startup backfill: {d}");
            if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                &daily_pool, d,
            ).await {
                tracing::error!("daily batch backfill failed for {d}: {e}");
            }
        }
```

After the `for` loop closes, insert:

```rust
        // Purge notifications that have been read for more than 30
        // days. Unread notifications are kept forever.
        match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
            Ok(n) if n > 0 => tracing::info!("purged {n} old read notifications"),
            Ok(_) => {}
            Err(e) => tracing::warn!("failed to purge old read notifications: {e}"),
        }
```

Also find the `if now_date != last_date { ... }` block around lines 1059-1066 (inside the `loop { interval.tick().await; ... }`) and add the same purge call *after* `update_daily_max_drawdown` inside that block so the purge runs on every daily rollover, not just at startup:

```rust
            if now_date != last_date {
                tracing::info!("running daily batch for {last_date}");
                if let Err(e) = auto_trader_db::summary::update_daily_max_drawdown(
                    &daily_pool, last_date,
                ).await {
                    tracing::error!("daily batch failed: {e}");
                }
                match auto_trader_db::notifications::purge_old_read(&daily_pool).await {
                    Ok(n) if n > 0 => tracing::info!("purged {n} old read notifications"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("failed to purge old read notifications: {e}"),
                }
                last_date = now_date;
            }
```

- [ ] **Step 2: Verify compile + clippy + tests**

Run: `cargo check --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/main.rs
git commit -m "feat(app): purge read notifications older than 30 days in daily batch"
```

---

## Task 6: Frontend API client and types

**Files:**
- Modify: `dashboard-ui/src/api/types.ts`
- Modify: `dashboard-ui/src/api/client.ts`

- [ ] **Step 1: Add types to `dashboard-ui/src/api/types.ts`**

Append to the end of the file:

```ts
export interface Notification {
  id: string
  kind: 'trade_opened' | 'trade_closed'
  trade_id: string
  paper_account_id: string
  strategy_name: string
  pair: string
  direction: 'long' | 'short'
  price: string
  pnl_amount: string | null
  exit_reason: string | null
  created_at: string
  read_at: string | null
}

export interface NotificationsResponse {
  items: Notification[]
  total: number
  unread_count: number
  page: number
  limit: number
}

export interface NotificationUnreadCountResponse {
  count: number
}

export interface NotificationsFilter {
  unread_only?: string
  limit?: string
  page?: string
  kind?: 'trade_opened' | 'trade_closed' | ''
  from?: string
  to?: string
}
```

- [ ] **Step 2: Add API functions to `dashboard-ui/src/api/client.ts`**

Append to the `api` object (just before the closing `}`, right after the `strategies` block):

```ts
  notifications: {
    list: (params: Record<string, string | undefined> = {}) =>
      get<NotificationsResponse>(`/api/notifications${qs(params)}`),
    unreadCount: () =>
      get<NotificationUnreadCountResponse>(`/api/notifications/unread-count`),
    markAllRead: () =>
      post<{ marked: number }>(`/api/notifications/mark-all-read`, {}),
  },
```

Also add the new imports to the top of `client.ts`:

```ts
import type {
  // ... existing imports remain as-is
  NotificationsResponse,
  NotificationUnreadCountResponse,
} from './types'
```

(Merge into the existing `import type { ... } from './types'` block — do not add a second import line.)

- [ ] **Step 3: Run lint + build**

Run: `cd dashboard-ui && npm run lint && npm run build`
Expected: PASS (pre-existing errors in `AccountForm.tsx` and `RiskBadge.tsx` are out of scope — verify your edits don't add new issues).

- [ ] **Step 4: Commit**

```bash
git add dashboard-ui/src/api/types.ts dashboard-ui/src/api/client.ts
git commit -m "feat(ui): add notifications types and API client bindings"
```

---

## Task 7: `NotificationBell` + `NotificationDropdown` components

**Files:**
- Create: `dashboard-ui/src/components/NotificationBell.tsx`
- Create: `dashboard-ui/src/components/NotificationDropdown.tsx`

- [ ] **Step 1: Create `dashboard-ui/src/components/NotificationDropdown.tsx`**

```tsx
import { useQuery } from '@tanstack/react-query'
import { Link } from 'react-router-dom'
import { api } from '../api/client'
import type { Notification } from '../api/types'

interface NotificationDropdownProps {
  open: boolean
}

function formatRelativeTime(iso: string): string {
  const now = Date.now()
  const then = new Date(iso).getTime()
  const diffSec = Math.max(0, Math.floor((now - then) / 1000))
  if (diffSec < 60) return `${diffSec}秒前`
  const diffMin = Math.floor(diffSec / 60)
  if (diffMin < 60) return `${diffMin}分前`
  const diffHour = Math.floor(diffMin / 60)
  if (diffHour < 24) return `${diffHour}時間前`
  const diffDay = Math.floor(diffHour / 24)
  return `${diffDay}日前`
}

function formatAbsoluteJst(iso: string): string {
  return new Date(iso).toLocaleString('ja-JP', {
    timeZone: 'Asia/Tokyo',
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function formatSignedInt(value: string | null): string {
  if (value == null) return ''
  const n = Number(value)
  if (Number.isNaN(n)) return value
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

function renderBody(n: Notification): React.ReactNode {
  const dir = n.direction.toUpperCase()
  const price = Number(n.price).toLocaleString()
  if (n.kind === 'trade_opened') {
    return (
      <span>
        <span className="text-sky-400 font-mono">OPEN</span>{' '}
        {n.pair} <span className={n.direction === 'long' ? 'text-emerald-400' : 'text-red-400'}>{dir}</span>
        {' '}@ {price}
      </span>
    )
  }
  const pnlNum = Number(n.pnl_amount ?? '0')
  const pnlClass = pnlNum >= 0 ? 'text-emerald-400' : 'text-red-400'
  return (
    <span>
      <span className="text-amber-400 font-mono">CLOSE</span>{' '}
      {n.pair} <span className={n.direction === 'long' ? 'text-emerald-400' : 'text-red-400'}>{dir}</span>
      {' '}
      <span className={`font-mono ${pnlClass}`}>{formatSignedInt(n.pnl_amount)}</span>
      {n.exit_reason && <span className="text-gray-500"> ({n.exit_reason})</span>}
    </span>
  )
}

export default function NotificationDropdown({ open }: NotificationDropdownProps) {
  const { data, isLoading } = useQuery({
    queryKey: ['notifications', { limit: 20 }],
    queryFn: () => api.notifications.list({ limit: '20', page: '1' }),
    // Only fetch while the dropdown is actually open to avoid
    // thrashing when the user isn't looking at it.
    enabled: open,
  })

  if (!open) return null

  return (
    <div className="absolute right-0 top-full mt-2 w-96 bg-gray-900 border border-gray-800 rounded-lg shadow-xl overflow-hidden z-50">
      <div className="px-4 py-2 border-b border-gray-800 text-sm font-semibold text-gray-100">
        通知
      </div>
      <div className="max-h-96 overflow-y-auto">
        {isLoading ? (
          <div className="px-4 py-6 text-center text-xs text-gray-500">読み込み中...</div>
        ) : !data || data.items.length === 0 ? (
          <div className="px-4 py-6 text-center text-xs text-gray-500">通知はありません</div>
        ) : (
          data.items.map((n) => (
            <div
              key={n.id}
              className={`px-4 py-2 border-b border-gray-800/60 last:border-b-0 ${
                n.read_at == null ? 'bg-sky-950/40' : ''
              }`}
            >
              <div className="text-xs text-gray-100">{renderBody(n)}</div>
              <div
                className="text-[10px] text-gray-500 mt-0.5"
                title={formatAbsoluteJst(n.created_at)}
              >
                {n.strategy_name} · {formatRelativeTime(n.created_at)}
              </div>
            </div>
          ))
        )}
      </div>
      <div className="px-4 py-2 border-t border-gray-800 text-right">
        <Link
          to="/notifications"
          className="text-xs text-sky-400 hover:text-sky-300"
        >
          すべて見る →
        </Link>
      </div>
    </div>
  )
}
```

- [ ] **Step 2: Create `dashboard-ui/src/components/NotificationBell.tsx`**

```tsx
import { useEffect, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import NotificationDropdown from './NotificationDropdown'

export default function NotificationBell() {
  const [open, setOpen] = useState(false)
  const containerRef = useRef<HTMLDivElement>(null)
  const queryClient = useQueryClient()

  // Poll the lightweight unread-count endpoint. Uses the shared query
  // client's 15s refetch interval.
  const { data: unreadData } = useQuery({
    queryKey: ['notifications-unread-count'],
    queryFn: () => api.notifications.unreadCount(),
  })

  const markAllRead = useMutation({
    mutationFn: () => api.notifications.markAllRead(),
    onSuccess: () => {
      // Invalidate both the badge and the dropdown list so they reflect
      // the now-read state on the next render.
      queryClient.invalidateQueries({ queryKey: ['notifications-unread-count'] })
      queryClient.invalidateQueries({ queryKey: ['notifications'] })
    },
  })

  // Close the dropdown on any mousedown outside the container. Using
  // mousedown (not click) matches the convention users expect from
  // other dropdowns so touching outside-and-releasing-inside doesn't
  // keep it open.
  useEffect(() => {
    if (!open) return
    const handler = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handler)
    return () => document.removeEventListener('mousedown', handler)
  }, [open])

  const toggle = () => {
    const next = !open
    setOpen(next)
    // Fire the mark-all-read exactly once, at the moment of opening.
    // The mutation is idempotent so double-opens don't cause harm, but
    // we guard with `next` to skip the call on close.
    if (next) {
      markAllRead.mutate()
    }
  }

  const count = unreadData?.count ?? 0
  const badgeText = count > 99 ? '99+' : String(count)

  return (
    <div ref={containerRef} className="relative ml-auto">
      <button
        type="button"
        onClick={toggle}
        aria-label="通知"
        className="relative p-1.5 text-gray-400 hover:text-gray-100 rounded transition"
      >
        <svg
          width="20"
          height="20"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
          <path d="M13.73 21a2 2 0 0 1-3.46 0" />
        </svg>
        {count > 0 && (
          <span className="absolute -top-0.5 -right-0.5 min-w-[16px] h-[16px] px-1 bg-red-500 text-white text-[10px] font-semibold rounded-full flex items-center justify-center">
            {badgeText}
          </span>
        )}
      </button>
      <NotificationDropdown open={open} />
    </div>
  )
}
```

- [ ] **Step 3: Lint + build**

Run: `cd dashboard-ui && npm run lint && npm run build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add dashboard-ui/src/components/NotificationBell.tsx dashboard-ui/src/components/NotificationDropdown.tsx
git commit -m "feat(ui): add NotificationBell with unread badge and dropdown"
```

---

## Task 8: Notifications page

**Files:**
- Create: `dashboard-ui/src/pages/Notifications.tsx`

- [ ] **Step 1: Create `dashboard-ui/src/pages/Notifications.tsx`**

```tsx
import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'
import type { Notification } from '../api/types'

const JST_OFFSET_MS = 9 * 60 * 60 * 1000

function jstDateString(date: Date): string {
  return new Date(date.getTime() + JST_OFFSET_MS).toISOString().slice(0, 10)
}

function periodToRange(period: string): { from?: string; to?: string } {
  if (!period) return {}
  const now = new Date()
  const to = jstDateString(now)
  if (period === 'today') return { from: to, to }
  if (period === '1w') {
    const d = new Date(now)
    d.setUTCDate(d.getUTCDate() - 7)
    return { from: jstDateString(d), to }
  }
  if (period === '1m') {
    const d = new Date(now)
    d.setUTCMonth(d.getUTCMonth() - 1)
    return { from: jstDateString(d), to }
  }
  return {}
}

function formatDateJst(iso: string): string {
  return new Date(iso).toLocaleString('ja-JP', {
    timeZone: 'Asia/Tokyo',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  })
}

function formatNum(value: string | null): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return value
  return Math.round(n).toLocaleString()
}

function formatSignedInt(value: string | null): string {
  if (value == null) return '-'
  const n = Number(value)
  if (Number.isNaN(n)) return value
  const sign = n > 0 ? '+' : ''
  return `${sign}${Math.round(n).toLocaleString()}`
}

function kindLabel(kind: Notification['kind']): string {
  return kind === 'trade_opened' ? 'OPEN' : 'CLOSE'
}

const PER_PAGE = 50

export default function Notifications() {
  const [period, setPeriod] = useState<string>('')
  const [kind, setKind] = useState<'' | 'trade_opened' | 'trade_closed'>('')
  const [page, setPage] = useState(1)
  const range = useMemo(() => periodToRange(period), [period])

  const { data, isLoading } = useQuery({
    queryKey: ['notifications', { page, period, kind }],
    queryFn: () =>
      api.notifications.list({
        page: String(page),
        limit: String(PER_PAGE),
        from: range.from,
        to: range.to,
        kind: kind || undefined,
      }),
  })

  const total = data?.total ?? 0
  const totalPages = Math.max(1, Math.ceil(total / PER_PAGE))
  const rangeStart = total === 0 ? 0 : (page - 1) * PER_PAGE + 1
  const rangeEnd = Math.min(page * PER_PAGE, total)

  const selectClass =
    'bg-gray-800 border border-gray-700 text-gray-100 text-sm rounded px-3 py-1.5 focus:outline-none focus:border-blue-500'
  const labelClass = 'text-xs text-gray-400 mr-1'

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-bold">通知履歴</h2>

      <div className="bg-gray-900 rounded p-3 flex flex-wrap items-center gap-3">
        <div className="flex items-center gap-2">
          <span className={labelClass}>期間</span>
          <select
            value={period}
            onChange={(e) => {
              setPeriod(e.target.value)
              setPage(1)
            }}
            className={selectClass}
          >
            <option value="">全期間</option>
            <option value="today">今日</option>
            <option value="1w">1週間</option>
            <option value="1m">1ヶ月</option>
          </select>
        </div>

        <div className="flex items-center gap-2">
          <span className={labelClass}>種別</span>
          <select
            value={kind}
            onChange={(e) => {
              setKind(e.target.value as '' | 'trade_opened' | 'trade_closed')
              setPage(1)
            }}
            className={selectClass}
          >
            <option value="">すべて</option>
            <option value="trade_opened">OPEN</option>
            <option value="trade_closed">CLOSE</option>
          </select>
        </div>
      </div>

      <div className="bg-gray-900 rounded-lg shadow overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="px-3 py-2 text-left text-gray-400 font-medium">日時</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">種別</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">戦略</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">ペア</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">方向</th>
                <th className="px-3 py-2 text-right text-gray-400 font-medium">価格</th>
                <th className="px-3 py-2 text-right text-gray-400 font-medium">PnL</th>
                <th className="px-3 py-2 text-left text-gray-400 font-medium">exit_reason</th>
              </tr>
            </thead>
            <tbody>
              {isLoading ? (
                <tr>
                  <td colSpan={8} className="px-3 py-8 text-center text-gray-500">
                    読み込み中...
                  </td>
                </tr>
              ) : !data || data.items.length === 0 ? (
                <tr>
                  <td colSpan={8} className="px-3 py-8 text-center text-gray-500">
                    通知はありません
                  </td>
                </tr>
              ) : (
                data.items.map((n) => (
                  <tr
                    key={n.id}
                    className={`border-b border-gray-800/50 ${
                      n.read_at == null ? 'bg-sky-950/30' : ''
                    }`}
                  >
                    <td className="px-3 py-2 text-gray-300 whitespace-nowrap">
                      {formatDateJst(n.created_at)}
                    </td>
                    <td className="px-3 py-2 font-mono">
                      <span
                        className={
                          n.kind === 'trade_opened'
                            ? 'text-sky-400'
                            : 'text-amber-400'
                        }
                      >
                        {kindLabel(n.kind)}
                      </span>
                    </td>
                    <td className="px-3 py-2 text-gray-300">{n.strategy_name}</td>
                    <td className="px-3 py-2 text-gray-300">{n.pair}</td>
                    <td className="px-3 py-2">
                      <span
                        className={
                          n.direction === 'long'
                            ? 'text-emerald-400'
                            : 'text-red-400'
                        }
                      >
                        {n.direction.toUpperCase()}
                      </span>
                    </td>
                    <td className="px-3 py-2 text-right font-mono text-gray-300">
                      {formatNum(n.price)}
                    </td>
                    <td className="px-3 py-2 text-right font-mono">
                      {n.pnl_amount == null ? (
                        <span className="text-gray-500">-</span>
                      ) : (
                        <span
                          className={
                            Number(n.pnl_amount) >= 0
                              ? 'text-emerald-400'
                              : 'text-red-400'
                          }
                        >
                          {formatSignedInt(n.pnl_amount)}
                        </span>
                      )}
                    </td>
                    <td className="px-3 py-2 text-gray-400 text-xs">
                      {n.exit_reason ?? '-'}
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>

        {totalPages > 1 && (
          <div className="flex items-center justify-between gap-2 px-4 py-2 border-t border-gray-800 text-xs text-gray-400">
            <span>
              {total} 件中 {rangeStart}-{rangeEnd} 件
            </span>
            <div className="flex gap-2">
              <button
                type="button"
                onClick={() => setPage((p) => Math.max(1, p - 1))}
                disabled={page <= 1}
                className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                前へ
              </button>
              <button
                type="button"
                onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
                disabled={page >= totalPages}
                className="px-3 py-1 bg-gray-800 rounded hover:bg-gray-700 disabled:opacity-40 disabled:cursor-not-allowed"
              >
                次へ
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
```

- [ ] **Step 2: Lint + build**

Run: `cd dashboard-ui && npm run lint && npm run build`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add dashboard-ui/src/pages/Notifications.tsx
git commit -m "feat(ui): add /notifications page with filters and paging"
```

---

## Task 9: Wire bell into header and add `/notifications` route

**Files:**
- Modify: `dashboard-ui/src/App.tsx`

- [ ] **Step 1: Edit `App.tsx` to render the bell and register the route**

Replace the file contents with:

```tsx
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { BrowserRouter, Routes, Route, NavLink } from 'react-router-dom'
import Overview from './pages/Overview'
import Trades from './pages/Trades'
import Analysis from './pages/Analysis'
import Accounts from './pages/Accounts'
import Positions from './pages/Positions'
import Strategies from './pages/Strategies'
import Notifications from './pages/Notifications'
import NotificationBell from './components/NotificationBell'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      retry: 1,
      // Auto-refetch every 15 seconds while a query is mounted so the
      // dashboard reflects new positions / fills / balance changes
      // without the user having to hit reload. TanStack defaults to
      // pausing this when the browser tab is in the background, so we
      // don't burn CPU when nobody's watching.
      // (refetchOnWindowFocus is already the TanStack default — left
      // implicit so the config matches the actual behavior.)
      refetchInterval: 15_000,
    },
  },
})

const navItems = [
  { to: '/', label: '概要' },
  { to: '/trades', label: 'トレード' },
  { to: '/analysis', label: '分析' },
  { to: '/accounts', label: '口座' },
  { to: '/positions', label: 'ポジション' },
  { to: '/strategies', label: '戦略' },
]

function NavBar() {
  return (
    <nav className="flex items-center gap-1 overflow-x-auto">
      {navItems.map((item) => (
        <NavLink
          key={item.to}
          to={item.to}
          end={item.to === '/'}
          className={({ isActive }) =>
            `px-3 py-1.5 text-sm rounded transition whitespace-nowrap ${
              isActive
                ? 'bg-gray-800 text-gray-100 font-medium'
                : 'text-gray-400 hover:text-gray-200 hover:bg-gray-800/50'
            }`
          }
        >
          {item.label}
        </NavLink>
      ))}
    </nav>
  )
}

function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <div className="min-h-screen bg-gray-950 text-gray-100">
          <header className="border-b border-gray-800 px-4 py-3">
            <div className="max-w-7xl mx-auto flex flex-col sm:flex-row items-start sm:items-center gap-3">
              <h1 className="text-lg font-bold whitespace-nowrap">
                Auto Trader
              </h1>
              <NavBar />
              {/* Bell lives flush-right; `ml-auto` inside the component
                  pushes it to the end of the flex row. Deliberately not
                  in `navItems` so it does not render as a tab. */}
              <NotificationBell />
            </div>
          </header>
          <main className="max-w-7xl mx-auto p-4">
            <Routes>
              <Route path="/" element={<Overview />} />
              <Route path="/trades" element={<Trades />} />
              <Route path="/analysis" element={<Analysis />} />
              <Route path="/accounts" element={<Accounts />} />
              <Route path="/positions" element={<Positions />} />
              <Route path="/strategies" element={<Strategies />} />
              <Route path="/notifications" element={<Notifications />} />
            </Routes>
          </main>
        </div>
      </BrowserRouter>
    </QueryClientProvider>
  )
}

export default App
```

- [ ] **Step 2: Lint + build**

Run: `cd dashboard-ui && npm run lint && npm run build`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add dashboard-ui/src/App.tsx
git commit -m "feat(ui): wire NotificationBell into header and add /notifications route"
```

---

## Task 10: Integration verification

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo check --workspace && cargo clippy --workspace -- -D warnings && cargo test --workspace`
Expected: PASS.

- [ ] **Step 2: Frontend build**

Run: `cd dashboard-ui && npm run lint && npm run build`
Expected: PASS (pre-existing errors in `AccountForm.tsx` / `RiskBadge.tsx` allowed).

- [ ] **Step 3: Rebuild Docker image and restart**

Run: `docker compose build auto-trader && docker compose up -d auto-trader`
Expected: Rebuild succeeds, container restarts cleanly, `docker logs auto-trader-auto-trader-1 --tail 30` shows `API server listening on 0.0.0.0:3001` and no panics.

- [ ] **Step 4: Manual smoke test**

Open the dashboard and verify, ticking each item:

- [ ] ヘッダー右端にベルアイコンが表示される
- [ ] 初回ロード時にベルに赤バッジが付く（既存オープン/クローズが未読扱い）
- [ ] ベルをクリックでドロップダウンが開き、最新 20 件が `OPEN ...` / `CLOSE ...` のフォーマットで並ぶ
- [ ] ドロップダウンを開いた直後に赤バッジが消える（既読化）
- [ ] 未読アイテム（もし新しく発生したら）は `bg-sky-950/40` で背景色が強調される
- [ ] ドロップダウン外クリックで閉じる
- [ ] ドロップダウン下部の「すべて見る →」で `/notifications` ページに遷移
- [ ] `/notifications` ページで期間/種別フィルタが動作する
- [ ] 「前へ」「次へ」ボタンが 50 件超でのみ表示され、正しくページング動作する
- [ ] 空状態表示（期間を「今日」にしてトレードが 0 件のときなど）
- [ ] ブラウザ開発者ツールで console / network エラーが出ていない

- [ ] **Step 5: DB sanity check**

Run inside the db container:
```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT kind, COUNT(*) FROM notifications GROUP BY kind;"
```
Expected: `trade_opened` and/or `trade_closed` counts reflecting reality.

Run:
```bash
docker exec auto-trader-db-1 psql -U auto-trader -d auto_trader -c "SELECT COUNT(*) FROM notifications WHERE read_at IS NULL;"
```
Expected: 0 immediately after clicking the bell (unread has been cleared).

- [ ] **Step 6: Report findings back to the user**

Summarize what passed, what didn't. Do not proceed to review/PR if anything is broken.

---

## Post-Implementation

1. Run the `code-review` skill flow (local codex:review loop → address findings → push → open PR → Copilot review → address findings). Per project convention and stored feedback memory, Claude pushes and creates the PR but does NOT merge.
2. Do NOT merge — user does that.
