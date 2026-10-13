use std::path::Path;

use crate::config;
use crate::data;

pub(super) async fn run_migrate(config_path: &Path) -> i32 {
    let settings = match config::load(config_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                config_path = %config_path.display(),
                error = %e,
                "failed to load or validate sim config"
            );
            return 1;
        }
    };
    let pool = match data::connect(&settings.database_url).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "failed to connect to database");
            return 1;
        }
    };
    match sqlx::migrate!("../../migrations").run(&pool).await {
        Ok(()) => {
            tracing::info!("applied migrations");
            0
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to apply migrations");
            1
        }
    }
}
