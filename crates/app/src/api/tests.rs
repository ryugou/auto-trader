use std::borrow::Cow;
use std::fmt;

use sqlx::error::{DatabaseError, ErrorKind};

use super::*;

#[derive(Debug)]
struct FakeDatabaseError {
    code: &'static str,
    constraint: &'static str,
}

impl fmt::Display for FakeDatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "database error (code {}) on constraint \"{}\"",
            self.code, self.constraint
        )
    }
}

impl std::error::Error for FakeDatabaseError {}

impl DatabaseError for FakeDatabaseError {
    fn message(&self) -> &str {
        "database error"
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(self.code))
    }

    fn constraint(&self) -> Option<&str> {
        Some(self.constraint)
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
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

#[test]
fn unique_violation_on_one_live_per_exchange_surfaces_the_db_layer_message() {
    let db_err = sqlx::Error::database(FakeDatabaseError {
        code: "23505",
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

#[test]
fn unique_violation_on_an_unrecognized_constraint_falls_back_to_duplicate_name() {
    let db_err = sqlx::Error::database(FakeDatabaseError {
        code: "23505",
        constraint: "some_future_unique_constraint",
    });

    let api_err: ApiError = anyhow::Error::new(db_err).into();

    assert_eq!(api_err.0, StatusCode::CONFLICT);
    assert_eq!(api_err.1, "invalid name: duplicate");
}

#[test]
fn foreign_key_violation_on_strategy_reference_becomes_bad_request() {
    let db_err = sqlx::Error::database(FakeDatabaseError {
        code: "23503",
        constraint: "trading_accounts_strategy_fkey",
    });

    let api_err: ApiError = anyhow::Error::new(db_err).into();

    assert_eq!(api_err.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        api_err.1,
        "invalid strategy: not found in catalog (see GET /api/strategies)"
    );
}

#[test]
fn foreign_key_violation_on_an_unrecognized_constraint_becomes_conflict() {
    let db_err = sqlx::Error::database(FakeDatabaseError {
        code: "23503",
        constraint: "trades_account_id_fkey",
    });

    let api_err: ApiError = anyhow::Error::new(db_err).into();

    assert_eq!(api_err.0, StatusCode::CONFLICT);
    assert_eq!(api_err.1, "failed to delete account: related trades exist");
}
