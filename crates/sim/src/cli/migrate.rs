use std::path::Path;

use super::prepare::load_config_and_connect;

pub(super) async fn run_migrate(config_path: &Path) -> i32 {
    let (pool, _) = match load_config_and_connect(config_path).await {
        Ok(connected) => connected,
        Err(code) => return code,
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
