/// Error returned by the simulator.
#[derive(Debug, thiserror::Error)]
pub enum SimError {
    /// A configuration value is invalid.
    #[error("{0}")]
    Config(String),
    /// A command line argument or a run parameter is invalid.
    #[error("{0}")]
    Args(String),
    /// Required tables are missing from the database; holds their names.
    #[error(
        "invalid required tables: {0:?} missing (run the new auto-trader image once to apply migrations, or run `auto-trader-sim migrate` on a non-production database)"
    )]
    MissingTables(Vec<String>),
    /// A trading script is invalid.
    #[error("{0}")]
    InvalidScript(String),
    /// Some requested dates could not be fetched; holds those dates.
    #[error("failed to fetch all requested dates: {0:?}")]
    FetchIncomplete(Vec<String>),
    /// A batch of runs failed.
    #[error("{0}")]
    BatchFailed(String),
    /// A database query failed.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// Any other failure, such as I/O, an HTTP request or serialization.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
