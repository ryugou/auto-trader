#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("{0}")]
    Config(String),
    #[error("{0}")]
    Args(String),
    #[error(
        "invalid required tables: {0:?} missing (run the new auto-trader image once to apply migrations, or run `auto-trader-sim migrate` on a non-production database)"
    )]
    MissingTables(Vec<String>),
    #[error("{0}")]
    InvalidScript(String),
    #[error("failed to fetch all requested dates: {0:?}")]
    FetchIncomplete(Vec<String>),
    #[error("{0}")]
    BatchFailed(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
