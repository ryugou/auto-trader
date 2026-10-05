mod accounts;
mod dashboard;
pub(crate) mod filters;
mod health;
mod market;
mod notifications;
mod positions;
mod strategies;
mod trades;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub price_store: std::sync::Arc<crate::price_store::PriceStore>,
    /// Resolved liquidation thresholds per exchange — keys must match what
    /// `startup::resolve_exchange_liquidation_levels` validated. The accounts
    /// API rejects creation requests for exchanges missing from this map so
    /// runtime account creation cannot panic worker tasks that look up
    /// liquidation levels per signal/exit/close.
    pub exchange_liquidation_levels: std::sync::Arc<
        std::collections::HashMap<auto_trader_core::types::Exchange, rust_decimal::Decimal>,
    >,
}

pub fn router(state: AppState) -> Router {
    let api_token = std::env::var("API_TOKEN").ok();

    let api_routes = Router::new()
        .route(
            "/trading-accounts",
            get(accounts::list).post(accounts::create),
        )
        .route(
            "/trading-accounts/:id",
            get(accounts::get_one)
                .put(accounts::update)
                .delete(accounts::remove),
        )
        .route("/dashboard/summary", get(dashboard::summary))
        .route("/dashboard/pnl-history", get(dashboard::pnl_history))
        .route(
            "/dashboard/balance-history",
            get(dashboard::balance_history),
        )
        .route("/dashboard/strategies", get(dashboard::strategies))
        .route("/dashboard/pairs", get(dashboard::pairs))
        .route("/dashboard/hourly-winrate", get(dashboard::hourly_winrate))
        .route("/trades", get(trades::list))
        .route("/trades/:id/events", get(trades::events))
        .route("/positions", get(positions::list))
        .route("/strategies", get(strategies::list))
        .route("/strategies/:name", get(strategies::get_one))
        .route("/notifications", get(notifications::list))
        .route(
            "/notifications/unread-count",
            get(notifications::unread_count),
        )
        .route(
            "/notifications/mark-all-read",
            axum::routing::post(notifications::mark_all_read),
        )
        .route("/market/prices", get(market::prices))
        .route("/health/market-feed", get(health::market_feed))
        .layer(middleware::from_fn(move |req, next| {
            let token = api_token.clone();
            auth_middleware(token, req, next)
        }))
        .with_state(state);

    Router::new()
        .nest("/api", api_routes)
        .fallback_service(
            ServeDir::new("dashboard-ui/dist")
                .fallback(ServeFile::new("dashboard-ui/dist/index.html")),
        )
        .layer(
            // Permissive CORS for same-host dashboard (network_mode: host).
            CorsLayer::permissive(),
        )
}

async fn auth_middleware(
    api_token: Option<String>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if let Some(expected) = &api_token {
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match auth {
            Some(token) if token == expected => next.run(req).await,
            _ => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "unauthorized" })),
            )
                .into_response(),
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
                    // Disambiguate by constraint name: the one-live-per-exchange
                    // partial unique index gets its specific, actionable
                    // message (built by `translate_unique_violation` and
                    // carried as `e`'s top-level context); any other unique
                    // violation (e.g. a future `name` uniqueness constraint)
                    // falls back to the generic message.
                    Some("23505") => match pg_err.constraint() {
                        Some("trading_accounts_one_live_per_exchange") => {
                            ApiError(StatusCode::CONFLICT, e.to_string())
                        }
                        _ => ApiError(StatusCode::CONFLICT, "duplicate name".to_string()),
                    },
                    // FK violation. Disambiguate by constraint name so the
                    // message reflects which relationship was violated:
                    //  - trading_accounts_strategy_fkey: catalog reference (400,
                    //    user fixable by picking a valid strategy)
                    //  - everything else (e.g. trades→trading_accounts): we
                    //    treat it as a delete-blocked-by-children case
                    Some("23503") => match pg_err.constraint() {
                        Some("trading_accounts_strategy_fkey") => ApiError(
                            StatusCode::BAD_REQUEST,
                            "strategy not found in catalog (see GET /api/strategies)".to_string(),
                        ),
                        _ => ApiError(
                            StatusCode::CONFLICT,
                            "account has related trades, cannot delete".to_string(),
                        ),
                    },
                    _ => ApiError(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "database error".to_string(),
                    ),
                };
            }
        }
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error".to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::error::{DatabaseError, ErrorKind};
    use std::borrow::Cow;
    use std::fmt;

    /// Minimal stand-in for a Postgres driver error, carrying only what
    /// `From<anyhow::Error>` inspects (`code()` / `constraint()`). The real
    /// `PgDatabaseError` has no public constructor outside an actual
    /// round-trip to Postgres, so this hand-rolled impl exercises the
    /// `constraint()` dispatch without a live database.
    #[derive(Debug)]
    struct FakeUniqueViolation {
        constraint: &'static str,
    }

    impl fmt::Display for FakeUniqueViolation {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "duplicate key value violates unique constraint \"{}\"",
                self.constraint
            )
        }
    }

    impl std::error::Error for FakeUniqueViolation {}

    impl DatabaseError for FakeUniqueViolation {
        fn message(&self) -> &str {
            "duplicate key value violates unique constraint"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed("23505"))
        }

        fn constraint(&self) -> Option<&str> {
            Some(self.constraint)
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::UniqueViolation
        }

        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
    }

    /// Mirrors what `translate_unique_violation` (crates/db) produces: the
    /// original `sqlx::Error::Database` as the source, with a human-readable
    /// context message naming the exchange on top.
    #[test]
    fn unique_violation_on_one_live_per_exchange_surfaces_the_db_layer_message() {
        let db_err = sqlx::Error::database(FakeUniqueViolation {
            constraint: "trading_accounts_one_live_per_exchange",
        });
        let wrapped = anyhow::Error::new(db_err).context(
            "live account for exchange 'bitflyer_cfd' already exists; \
             only 1 active live account per exchange is supported",
        );

        let api_err: ApiError = wrapped.into();

        assert_eq!(api_err.0, StatusCode::CONFLICT);
        assert!(
            api_err.1.contains("bitflyer_cfd") && api_err.1.contains("already exists"),
            "expected the DB-layer message to pass through, got: {}",
            api_err.1
        );
    }

    /// A unique violation on any constraint other than
    /// `trading_accounts_one_live_per_exchange` (e.g. a hypothetical future
    /// `name` uniqueness constraint) must keep falling back to the generic
    /// message — this dispatch must not swallow unrecognized constraints.
    #[test]
    fn unique_violation_on_an_unrecognized_constraint_falls_back_to_duplicate_name() {
        let db_err = sqlx::Error::database(FakeUniqueViolation {
            constraint: "some_future_unique_constraint",
        });

        let api_err: ApiError = anyhow::Error::new(db_err).into();

        assert_eq!(api_err.0, StatusCode::CONFLICT);
        assert_eq!(api_err.1, "duplicate name");
    }
}
