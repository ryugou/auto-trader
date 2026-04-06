# Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** auto-trader の成績確認・口座管理ダッシュボードを構築する。バックエンド API + React フロントエンド。

**Architecture:** 既存の `crates/app/src/api.rs` にダッシュボード用 SQL クエリハンドラを追加。`dashboard-ui/` に React + Recharts + Vite のフロントエンドを構築。本番は axum が静的ファイルを配信。CI は GitHub Actions で npm build + cargo test。

**Tech Stack:** Rust (axum, sqlx, tower-http), React 19, TypeScript, Vite, Recharts, TanStack Table, TanStack Query, Tailwind CSS

**Spec:** `docs/superpowers/specs/2026-04-06-dashboard-design.md`

---

## File Structure

### 新規作成

| ファイル | 責務 |
|---------|------|
| `crates/app/src/api/mod.rs` | API モジュール定義 |
| `crates/app/src/api/accounts.rs` | paper_accounts CRUD ハンドラ（既存 api.rs から移動） |
| `crates/app/src/api/dashboard.rs` | ダッシュボード KPI・チャートデータ |
| `crates/app/src/api/trades.rs` | トレード履歴（ページネーション・フィルタ） |
| `crates/app/src/api/positions.rs` | 保有中ポジション（メモリから取得） |
| `crates/app/src/api/filters.rs` | 共通フィルタクエリパラメータ |
| `crates/db/src/dashboard.rs` | ダッシュボード用 SQL クエリ |
| `dashboard-ui/` | React フロントエンド一式 |
| `.github/workflows/ci.yml` | CI ワークフロー |

### 変更

| ファイル | 変更内容 |
|---------|---------|
| `crates/app/src/api.rs` | → `crates/app/src/api/mod.rs` にリファクタ |
| `crates/app/src/main.rs` | API router に PaperTrader 参照を State として渡す、静的ファイル配信追加 |
| `crates/app/Cargo.toml` | tower-http 依存追加 |
| `crates/db/src/lib.rs` | dashboard モジュール公開 |
| `Cargo.toml` | tower-http を workspace deps に追加 |
| `Dockerfile` | dashboard-ui ビルドステップ追加 |
| `docker-compose.yml` | dashboard-ui ボリュームマウント（開発用） |

---

## Task 1: api.rs をモジュールディレクトリにリファクタ

**Files:**
- Move: `crates/app/src/api.rs` → `crates/app/src/api/mod.rs`
- Create: `crates/app/src/api/accounts.rs`

- [ ] **Step 1: api ディレクトリを作成し、既存コードを分割**

`crates/app/src/api.rs` を `crates/app/src/api/mod.rs` に移動。paper_accounts のハンドラを `accounts.rs` に分離。`mod.rs` は router 組み立てとミドルウェアのみ残す。

`crates/app/src/api/mod.rs`:
```rust
mod accounts;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use sqlx::PgPool;

pub fn router(pool: PgPool) -> Router {
    let api_token = std::env::var("API_TOKEN").ok();
    Router::new()
        .route("/api/paper-accounts", get(accounts::list).post(accounts::create))
        .route(
            "/api/paper-accounts/{id}",
            get(accounts::get_one).put(accounts::update).delete(accounts::remove),
        )
        .layer(middleware::from_fn(move |req, next| {
            let token = api_token.clone();
            auth_middleware(token, req, next)
        }))
        .with_state(pool)
}

async fn auth_middleware(
    api_token: Option<String>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if let Some(expected) = &api_token {
        let auth = req.headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            Some(token) if token == expected => next.run(req).await,
            _ => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" }))).into_response(),
        }
    } else {
        next.run(req).await
    }
}

pub(crate) struct ApiError(pub StatusCode, pub String);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        for cause in e.chain() {
            if let Some(sqlx::Error::Database(pg_err)) = cause.downcast_ref::<sqlx::Error>() {
                return match pg_err.code().as_deref() {
                    Some("23505") => ApiError(StatusCode::CONFLICT, "duplicate name".to_string()),
                    Some("23503") => ApiError(StatusCode::CONFLICT, "account has related trades, cannot delete".to_string()),
                    _ => ApiError(StatusCode::INTERNAL_SERVER_ERROR, "database error".to_string()),
                };
            }
        }
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
    }
}
```

`crates/app/src/api/accounts.rs`:
```rust
use super::ApiError;
use auto_trader_db::paper_accounts::{
    self, CreatePaperAccount, PaperAccount, UpdatePaperAccount,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn list(State(pool): State<PgPool>) -> Result<Json<Vec<PaperAccount>>, ApiError> {
    paper_accounts::list_paper_accounts(&pool)
        .await
        .map(Json)
        .map_err(Into::into)
}

pub async fn get_one(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::get_paper_account(&pool, id)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
}

pub async fn create(
    State(pool): State<PgPool>,
    Json(req): Json<CreatePaperAccount>,
) -> Result<impl IntoResponse, ApiError> {
    paper_accounts::create_paper_account(&pool, &req)
        .await
        .map(|a| (StatusCode::CREATED, Json(a)))
        .map_err(Into::into)
}

pub async fn update(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePaperAccount>,
) -> Result<Json<PaperAccount>, ApiError> {
    paper_accounts::update_paper_account(&pool, id, &req)
        .await
        .map_err(ApiError::from)?
        .map(Json)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
}

pub async fn remove(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let deleted = paper_accounts::delete_paper_account(&pool, id)
        .await
        .map_err(ApiError::from)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError(StatusCode::NOT_FOUND, "account not found".to_string()))
    }
}
```

- [ ] **Step 2: main.rs の `mod api` を確認（パスは自動で api/mod.rs を参照）**

`crates/app/src/main.rs` の `mod api;` は変更不要（Rust は `api.rs` → `api/mod.rs` を自動解決）。

- [ ] **Step 3: cargo check && cargo test で全テスト通過を確認**

Run: `cargo check && cargo test`
Expected: 全テスト PASS

- [ ] **Step 4: コミット**

```bash
git add -A
git commit -m "refactor(api): split api.rs into api/ module directory"
```

---

## Task 2: 共通フィルタとダッシュボード DB クエリ

**Files:**
- Create: `crates/app/src/api/filters.rs`
- Create: `crates/db/src/dashboard.rs`
- Modify: `crates/db/src/lib.rs`

- [ ] **Step 1: 共通フィルタ型を定義**

`crates/app/src/api/filters.rs`:
```rust
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize, Default)]
pub struct DashboardFilter {
    pub exchange: Option<String>,
    pub paper_account_id: Option<Uuid>,
    pub strategy: Option<String>,
    pub pair: Option<String>,
    pub from: Option<String>,  // RFC3339 date
    pub to: Option<String>,    // RFC3339 date
}

#[derive(Debug, Deserialize, Default)]
pub struct TradeFilter {
    pub exchange: Option<String>,
    pub paper_account_id: Option<Uuid>,
    pub strategy: Option<String>,
    pub pair: Option<String>,
    pub status: Option<String>,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}
```

- [ ] **Step 2: ダッシュボード用 SQL クエリを DB クレートに追加**

`crates/db/src/dashboard.rs`:
```rust
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct SummaryStats {
    pub total_pnl: Decimal,
    pub total_fees: Decimal,
    pub trade_count: i64,
    pub win_count: i64,
    pub max_drawdown: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PnlHistoryRow {
    pub date: NaiveDate,
    pub total_pnl: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct StrategyStats {
    pub strategy_name: String,
    pub trade_count: i64,
    pub win_count: i64,
    pub total_pnl: Decimal,
    pub max_drawdown: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PairStats {
    pub pair: String,
    pub trade_count: i64,
    pub win_count: i64,
    pub total_pnl: Decimal,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HourlyWinrate {
    pub hour: i32,
    pub trade_count: i64,
    pub win_count: i64,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TradeRow {
    pub id: Uuid,
    pub strategy_name: String,
    pub pair: String,
    pub exchange: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub stop_loss: Decimal,
    pub take_profit: Decimal,
    pub quantity: Option<Decimal>,
    pub leverage: Decimal,
    pub fees: Decimal,
    pub pnl_amount: Option<Decimal>,
    pub pnl_pips: Option<Decimal>,
    pub entry_at: chrono::DateTime<chrono::Utc>,
    pub exit_at: Option<chrono::DateTime<chrono::Utc>>,
    pub exit_reason: Option<String>,
    pub paper_account_id: Option<Uuid>,
    pub status: String,
}

pub async fn get_summary(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<SummaryStats> {
    let row = sqlx::query_as::<_, SummaryStats>(
        r#"SELECT
            COALESCE(SUM(total_pnl), 0) as total_pnl,
            COALESCE(SUM(max_drawdown), 0) as max_drawdown,
            COALESCE(SUM(trade_count), 0)::bigint as trade_count,
            COALESCE(SUM(win_count), 0)::bigint as win_count,
            0::decimal as total_fees
        FROM daily_summary
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
          AND ($3::date IS NULL OR date >= $3)
          AND ($4::date IS NULL OR date <= $4)"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn get_pnl_history(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> anyhow::Result<Vec<PnlHistoryRow>> {
    let rows = sqlx::query_as::<_, PnlHistoryRow>(
        r#"SELECT date, SUM(total_pnl) as total_pnl
        FROM daily_summary
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
          AND ($3::date IS NULL OR date >= $3)
          AND ($4::date IS NULL OR date <= $4)
        GROUP BY date ORDER BY date ASC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_strategy_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<StrategyStats>> {
    let rows = sqlx::query_as::<_, StrategyStats>(
        r#"SELECT strategy_name,
            SUM(trade_count)::bigint as trade_count,
            SUM(win_count)::bigint as win_count,
            SUM(total_pnl) as total_pnl,
            MAX(max_drawdown) as max_drawdown
        FROM daily_summary
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
        GROUP BY strategy_name ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_pair_stats(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<PairStats>> {
    let rows = sqlx::query_as::<_, PairStats>(
        r#"SELECT pair,
            SUM(trade_count)::bigint as trade_count,
            SUM(win_count)::bigint as win_count,
            SUM(total_pnl) as total_pnl
        FROM daily_summary
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
        GROUP BY pair ORDER BY total_pnl DESC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_hourly_winrate(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
) -> anyhow::Result<Vec<HourlyWinrate>> {
    let rows = sqlx::query_as::<_, HourlyWinrate>(
        r#"SELECT EXTRACT(HOUR FROM entry_at)::int as hour,
            COUNT(*)::bigint as trade_count,
            COUNT(*) FILTER (WHERE pnl_amount > 0)::bigint as win_count
        FROM trades
        WHERE status = 'closed'
          AND ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
        GROUP BY hour ORDER BY hour ASC"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn get_trades(
    pool: &PgPool,
    exchange: Option<&str>,
    paper_account_id: Option<Uuid>,
    strategy: Option<&str>,
    pair: Option<&str>,
    status: Option<&str>,
    page: i64,
    per_page: i64,
) -> anyhow::Result<(Vec<TradeRow>, i64)> {
    let offset = (page - 1) * per_page;

    let total: (i64,) = sqlx::query_as(
        r#"SELECT COUNT(*)::bigint FROM trades
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
          AND ($3::text IS NULL OR strategy_name = $3)
          AND ($4::text IS NULL OR pair = $4)
          AND ($5::text IS NULL OR status = $5)"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query_as::<_, TradeRow>(
        r#"SELECT id, strategy_name, pair, exchange, direction, entry_price, exit_price,
            stop_loss, take_profit, quantity, leverage, fees, pnl_amount, pnl_pips,
            entry_at, exit_at, exit_reason, paper_account_id, status
        FROM trades
        WHERE ($1::text IS NULL OR exchange = $1)
          AND ($2::uuid IS NULL OR paper_account_id = $2)
          AND ($3::text IS NULL OR strategy_name = $3)
          AND ($4::text IS NULL OR pair = $4)
          AND ($5::text IS NULL OR status = $5)
        ORDER BY entry_at DESC
        LIMIT $6 OFFSET $7"#,
    )
    .bind(exchange)
    .bind(paper_account_id)
    .bind(strategy)
    .bind(pair)
    .bind(status)
    .bind(per_page)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    Ok((rows, total.0))
}
```

`crates/db/src/lib.rs` に追加:
```rust
pub mod dashboard;
```

- [ ] **Step 3: cargo check で確認**

Run: `cargo check`
Expected: 成功

- [ ] **Step 4: コミット**

```bash
git add -A
git commit -m "feat(db): add dashboard SQL queries and common filter types"
```

---

## Task 3: ダッシュボード API ハンドラ

**Files:**
- Create: `crates/app/src/api/dashboard.rs`
- Create: `crates/app/src/api/trades.rs`
- Create: `crates/app/src/api/positions.rs`
- Modify: `crates/app/src/api/mod.rs`
- Modify: `crates/app/src/main.rs`
- Modify: `crates/app/Cargo.toml`
- Modify: `Cargo.toml`

- [ ] **Step 1: AppState を定義して router に渡す**

API は PgPool だけでなく PaperTrader 参照も必要（ポジション取得）。AppState を導入する。

`crates/app/src/api/mod.rs` に AppState を追加:
```rust
use auto_trader_executor::paper::PaperTrader;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub paper_traders: Vec<(String, Arc<PaperTrader>)>,  // (account_name, trader)
}
```

router の State を `PgPool` → `AppState` に変更。accounts ハンドラも `State(state): State<AppState>` → `state.pool` で DB アクセス。

- [ ] **Step 2: dashboard.rs ハンドラを作成**

`crates/app/src/api/dashboard.rs`:
```rust
use super::{filters::DashboardFilter, ApiError, AppState};
use auto_trader_db::dashboard;
use axum::extract::{Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::Serialize;

#[derive(Serialize)]
pub struct SummaryResponse {
    pub total_pnl: rust_decimal::Decimal,
    pub net_pnl: rust_decimal::Decimal,
    pub total_fees: rust_decimal::Decimal,
    pub trade_count: i64,
    pub win_count: i64,
    pub loss_count: i64,
    pub win_rate: f64,
    pub expected_value: f64,
    pub max_drawdown: rust_decimal::Decimal,
}

pub async fn summary(
    State(state): State<AppState>,
    Query(f): Query<DashboardFilter>,
) -> Result<Json<SummaryResponse>, ApiError> {
    let from = f.from.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let to = f.to.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let stats = dashboard::get_summary(
        &state.pool, f.exchange.as_deref(), f.paper_account_id, from, to,
    ).await.map_err(ApiError::from)?;

    let loss_count = stats.trade_count - stats.win_count;
    let win_rate = if stats.trade_count > 0 {
        stats.win_count as f64 / stats.trade_count as f64
    } else { 0.0 };
    let expected_value = if stats.trade_count > 0 {
        stats.total_pnl.to_string().parse::<f64>().unwrap_or(0.0) / stats.trade_count as f64
    } else { 0.0 };

    Ok(Json(SummaryResponse {
        total_pnl: stats.total_pnl,
        net_pnl: stats.total_pnl - stats.total_fees,
        total_fees: stats.total_fees,
        trade_count: stats.trade_count,
        win_count: stats.win_count,
        loss_count,
        win_rate,
        expected_value,
        max_drawdown: stats.max_drawdown,
    }))
}

pub async fn pnl_history(
    State(state): State<AppState>,
    Query(f): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::PnlHistoryRow>>, ApiError> {
    let from = f.from.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    let to = f.to.as_deref().and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    dashboard::get_pnl_history(&state.pool, f.exchange.as_deref(), f.paper_account_id, from, to)
        .await.map(Json).map_err(Into::into)
}

pub async fn strategies(
    State(state): State<AppState>,
    Query(f): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::StrategyStats>>, ApiError> {
    dashboard::get_strategy_stats(&state.pool, f.exchange.as_deref(), f.paper_account_id)
        .await.map(Json).map_err(Into::into)
}

pub async fn pairs(
    State(state): State<AppState>,
    Query(f): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::PairStats>>, ApiError> {
    dashboard::get_pair_stats(&state.pool, f.exchange.as_deref(), f.paper_account_id)
        .await.map(Json).map_err(Into::into)
}

pub async fn hourly_winrate(
    State(state): State<AppState>,
    Query(f): Query<DashboardFilter>,
) -> Result<Json<Vec<dashboard::HourlyWinrate>>, ApiError> {
    dashboard::get_hourly_winrate(&state.pool, f.exchange.as_deref(), f.paper_account_id)
        .await.map(Json).map_err(Into::into)
}
```

- [ ] **Step 3: trades.rs ハンドラを作成**

`crates/app/src/api/trades.rs`:
```rust
use super::{filters::TradeFilter, ApiError, AppState};
use auto_trader_db::dashboard;
use axum::extract::{Query, State};
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct TradesResponse {
    pub trades: Vec<dashboard::TradeRow>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

pub async fn list(
    State(state): State<AppState>,
    Query(f): Query<TradeFilter>,
) -> Result<Json<TradesResponse>, ApiError> {
    let page = f.page.unwrap_or(1).max(1);
    let per_page = f.per_page.unwrap_or(20).clamp(1, 100);
    let (trades, total) = dashboard::get_trades(
        &state.pool,
        f.exchange.as_deref(),
        f.paper_account_id,
        f.strategy.as_deref(),
        f.pair.as_deref(),
        f.status.as_deref(),
        page, per_page,
    ).await.map_err(ApiError::from)?;
    Ok(Json(TradesResponse { trades, total, page, per_page }))
}
```

- [ ] **Step 4: positions.rs ハンドラを作成**

`crates/app/src/api/positions.rs`:
```rust
use super::{ApiError, AppState};
use axum::extract::State;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct PositionResponse {
    pub trade_id: String,
    pub strategy_name: String,
    pub pair: String,
    pub exchange: String,
    pub direction: String,
    pub entry_price: String,
    pub quantity: Option<String>,
    pub stop_loss: String,
    pub take_profit: String,
    pub entry_at: String,
    pub paper_account_id: Option<String>,
    pub paper_account_name: String,
}

pub async fn list(
    State(state): State<AppState>,
) -> Result<Json<Vec<PositionResponse>>, ApiError> {
    let mut all_positions = Vec::new();
    for (name, trader) in &state.paper_traders {
        let positions = trader.open_positions().await.map_err(|e| ApiError::from(e))?;
        for pos in positions {
            let t = &pos.trade;
            all_positions.push(PositionResponse {
                trade_id: t.id.to_string(),
                strategy_name: t.strategy_name.clone(),
                pair: t.pair.to_string(),
                exchange: t.exchange.as_str().to_string(),
                direction: serde_json::to_string(&t.direction).unwrap_or_default().trim_matches('"').to_string(),
                entry_price: t.entry_price.to_string(),
                quantity: t.quantity.map(|q| q.to_string()),
                stop_loss: t.stop_loss.to_string(),
                take_profit: t.take_profit.to_string(),
                entry_at: t.entry_at.to_rfc3339(),
                paper_account_id: t.paper_account_id.map(|id| id.to_string()),
                paper_account_name: name.clone(),
            });
        }
    }
    // FX paper_trader positions are not included (no paper_account_id)
    Ok(Json(all_positions))
}
```

- [ ] **Step 5: mod.rs にルートを追加**

`crates/app/src/api/mod.rs` にモジュールと新ルートを追加:
```rust
mod accounts;
mod dashboard;
mod filters;
mod positions;
mod trades;
```

router に追加:
```rust
.route("/api/dashboard/summary", get(dashboard::summary))
.route("/api/dashboard/pnl-history", get(dashboard::pnl_history))
.route("/api/dashboard/strategies", get(dashboard::strategies))
.route("/api/dashboard/pairs", get(dashboard::pairs))
.route("/api/dashboard/hourly-winrate", get(dashboard::hourly_winrate))
.route("/api/trades", get(trades::list))
.route("/api/positions", get(positions::list))
```

- [ ] **Step 6: main.rs で AppState を構築して router に渡す**

main.rs の API router 構築部分を変更:
```rust
let api_state = api::AppState {
    pool: pool.clone(),
    paper_traders: paper_accounts.iter()
        .map(|(name, _strategy, trader)| (name.clone(), trader.clone()))
        .collect(),
};
let app = api::router(api_state);
```

- [ ] **Step 7: tower-http を依存に追加（静的ファイル配信用）**

`Cargo.toml` (workspace):
```toml
tower-http = { version = "0.6", features = ["fs", "cors"] }
```

`crates/app/Cargo.toml`:
```toml
tower-http = { workspace = true }
```

- [ ] **Step 8: cargo check && cargo test**

Run: `cargo check && cargo test`
Expected: 全テスト PASS

- [ ] **Step 9: コミット**

```bash
git add -A
git commit -m "feat(api): add dashboard, trades, positions endpoints with AppState"
```

---

## Task 4: 静的ファイル配信と CORS

**Files:**
- Modify: `crates/app/src/api/mod.rs`
- Modify: `crates/app/src/main.rs`

- [ ] **Step 1: router に静的ファイル配信と CORS を追加**

`crates/app/src/api/mod.rs` の router に:
```rust
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

// router の末尾に追加:
.fallback_service(ServeDir::new("dashboard-ui/dist"))
.layer(CorsLayer::new()
    .allow_origin(Any)
    .allow_methods(Any)
    .allow_headers(Any))
```

- [ ] **Step 2: cargo check**

Run: `cargo check`
Expected: 成功

- [ ] **Step 3: コミット**

```bash
git add -A
git commit -m "feat(api): add static file serving and CORS for dashboard UI"
```

---

## Task 5: フロントエンド初期化

**Files:**
- Create: `dashboard-ui/` 一式

- [ ] **Step 1: Vite + React + TypeScript プロジェクトを初期化**

```bash
cd /Users/ryugo/Developer/src/personal/auto-trader
npm create vite@latest dashboard-ui -- --template react-ts
cd dashboard-ui
npm install
npm install recharts @tanstack/react-table @tanstack/react-query
npm install -D tailwindcss @tailwindcss/vite
```

- [ ] **Step 2: Tailwind CSS を設定**

`dashboard-ui/src/index.css`:
```css
@import "tailwindcss";
```

`dashboard-ui/vite.config.ts`:
```typescript
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    proxy: {
      '/api': 'http://localhost:3001',
    },
  },
})
```

- [ ] **Step 3: API クライアントと型定義を作成**

`dashboard-ui/src/api/types.ts` と `dashboard-ui/src/api/client.ts` を作成。spec のレスポンス型に合わせた TypeScript 型と fetch ラッパー。

- [ ] **Step 4: npm run dev で起動確認**

Run: `cd dashboard-ui && npm run dev`
Expected: Vite dev server が起動、http://localhost:5173 でアクセス可能

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(ui): initialize dashboard-ui with React + Vite + Tailwind"
```

---

## Task 6: フロントエンド — 概要ページ

**Files:**
- Create/Modify: `dashboard-ui/src/pages/Overview.tsx`
- Create: `dashboard-ui/src/components/KpiCards.tsx`
- Create: `dashboard-ui/src/components/PnlChart.tsx`
- Create: `dashboard-ui/src/components/GlobalFilters.tsx`

- [ ] **Step 1: GlobalFilters コンポーネント（取引所・口座・期間フィルタ）**

ヘッダーに常駐するフィルタバー。選択値は URL クエリパラメータと同期。

- [ ] **Step 2: KpiCards コンポーネント**

`/api/dashboard/summary` からデータ取得。カード4枚: 総損益、勝率、期待値、最大DD。

- [ ] **Step 3: PnlChart コンポーネント**

`/api/dashboard/pnl-history` からデータ取得。Recharts の AreaChart で損益推移を描画。累計の折れ線。

- [ ] **Step 4: Overview ページに組み立て**

KpiCards + PnlChart + 口座比較チャートを配置。

- [ ] **Step 5: npm run build で確認**

Run: `cd dashboard-ui && npm run build`
Expected: `dist/` にビルド成果物が生成

- [ ] **Step 6: コミット**

```bash
git add -A
git commit -m "feat(ui): add overview page with KPI cards and PnL chart"
```

---

## Task 7: フロントエンド — トレード履歴ページ

**Files:**
- Create: `dashboard-ui/src/pages/Trades.tsx`
- Create: `dashboard-ui/src/components/TradeTable.tsx`

- [ ] **Step 1: TradeTable コンポーネント**

`/api/trades` からデータ取得。TanStack Table でソート・フィルタ・ページネーション。列: ペア、方向、エントリー/エグジット価格、数量、PnL、手数料、net_pnl、保有時間。

- [ ] **Step 2: Trades ページ**

GlobalFilters + TradeTable を配置。

- [ ] **Step 3: コミット**

```bash
git add -A
git commit -m "feat(ui): add trade history page with sortable table"
```

---

## Task 8: フロントエンド — 分析ページ

**Files:**
- Create: `dashboard-ui/src/pages/Analysis.tsx`
- Create: `dashboard-ui/src/components/StrategyChart.tsx`
- Create: `dashboard-ui/src/components/PairChart.tsx`
- Create: `dashboard-ui/src/components/HourlyChart.tsx`

- [ ] **Step 1: StrategyChart（戦略別成績 BarChart）**

`/api/dashboard/strategies` からデータ取得。

- [ ] **Step 2: PairChart（ペア別成績 BarChart）**

`/api/dashboard/pairs` からデータ取得。

- [ ] **Step 3: HourlyChart（時間帯別勝率 BarChart、24時間）**

`/api/dashboard/hourly-winrate` からデータ取得。

- [ ] **Step 4: Analysis ページに組み立て**

3つのチャートを縦に並べる。

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(ui): add analysis page with strategy, pair, and hourly charts"
```

---

## Task 9: フロントエンド — 口座管理・ポジションページ

**Files:**
- Create: `dashboard-ui/src/pages/Accounts.tsx`
- Create: `dashboard-ui/src/pages/Positions.tsx`
- Create: `dashboard-ui/src/components/AccountForm.tsx`

- [ ] **Step 1: Accounts ページ**

`/api/paper-accounts` で CRUD。一覧テーブル + 作成フォーム + 編集モーダル + 削除確認。

- [ ] **Step 2: Positions ページ**

`/api/positions` から保有ポジション一覧。リロードボタン付き。

- [ ] **Step 3: App.tsx にルーティング設定**

React Router で `/`, `/trades`, `/analysis`, `/accounts`, `/positions` を設定。ナビゲーションバー追加。

- [ ] **Step 4: npm run build で最終確認**

Run: `cd dashboard-ui && npm run build`
Expected: ビルド成功

- [ ] **Step 5: コミット**

```bash
git add -A
git commit -m "feat(ui): add accounts management and positions pages"
```

---

## Task 10: CI ワークフロー

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `Dockerfile`

- [ ] **Step 1: GitHub Actions CI を作成**

`.github/workflows/ci.yml`:
```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

jobs:
  frontend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: actions/setup-node@v4
        with:
          node-version: 22
          cache: npm
          cache-dependency-path: dashboard-ui/package-lock.json
      - run: cd dashboard-ui && npm ci
      - run: cd dashboard-ui && npm run build

  backend:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          submodules: recursive
      - uses: actions-rust-lang/setup-rust-toolchain@v1
      - run: sudo apt-get install -y protobuf-compiler
      - run: cargo check --workspace
      - run: cargo test --workspace
      - run: cargo clippy --workspace -- -D warnings
```

- [ ] **Step 2: Dockerfile にフロントエンドビルドを追加**

Dockerfile の builder ステージに:
```dockerfile
# Frontend build
FROM node:22-alpine AS frontend
WORKDIR /app/dashboard-ui
COPY dashboard-ui/package*.json ./
RUN npm ci
COPY dashboard-ui/ ./
RUN npm run build

# Rust build (既存)
FROM rust:1.85-bookworm AS builder
# ... 既存のまま ...
COPY --from=frontend /app/dashboard-ui/dist /app/dashboard-ui/dist

# Runtime
FROM debian:bookworm-slim
# ... 既存のまま ...
COPY --from=frontend /app/dashboard-ui/dist /app/dashboard-ui/dist
```

- [ ] **Step 3: コミット**

```bash
git add -A
git commit -m "ci: add GitHub Actions workflow and multi-stage Docker build"
```

---

## Task 11: 最終確認

- [ ] **Step 1: cargo test で全テスト通過**

Run: `cargo test --workspace`
Expected: 全テスト PASS

- [ ] **Step 2: cargo clippy で警告なし**

Run: `cargo clippy --workspace -- -D warnings`
Expected: 警告なし

- [ ] **Step 3: npm run build でフロントエンドビルド成功**

Run: `cd dashboard-ui && npm run build`
Expected: `dist/` 生成

- [ ] **Step 4: docker compose build で Docker イメージビルド成功**

Run: `docker compose build`
Expected: ビルド成功
