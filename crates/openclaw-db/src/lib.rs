pub mod context;
pub mod cron;
pub mod llm_log;
pub mod messages;
pub mod metrics;
pub mod sessions;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::OnceLock;
use tracing::{info, warn};

static GLOBAL_POOL: OnceLock<PgPool> = OnceLock::new();

/// Initialize the global database pool. Call once from main.
/// Returns Ok(true) if connected, Ok(false) if DATABASE_URL not set (graceful skip).
pub async fn init() -> Result<bool> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            warn!("DATABASE_URL not set — Postgres features disabled");
            return Ok(false);
        }
    };

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&url)
        .await?;

    info!("Connected to Postgres");
    let _ = GLOBAL_POOL.set(pool);
    Ok(true)
}

/// Try to initialize, but don't fail if Postgres is unreachable.
/// Returns true if connected.
pub async fn try_init() -> bool {
    match init().await {
        Ok(connected) => connected,
        Err(e) => {
            warn!("Postgres connection failed (non-fatal): {}", e);
            false
        }
    }
}

/// Get the global pool (None if not initialized)
pub fn pool() -> Option<&'static PgPool> {
    GLOBAL_POOL.get()
}
