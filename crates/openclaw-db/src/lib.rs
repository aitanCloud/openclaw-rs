pub mod context;
pub mod cron;
pub mod llm_log;
pub mod messages;
pub mod metrics;
pub mod sessions;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
pub use sqlx::PgPool;
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

/// Spawn a background task that retries connecting to Postgres with exponential
/// backoff (2s → 600s cap). Resolves `pool()` from `None` to `Some` once connected.
/// No-op if DATABASE_URL is unset or the pool is already initialized.
pub fn spawn_reconnect_loop() {
    // Nothing to reconnect to if URL isn't configured
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    // Already connected
    if GLOBAL_POOL.get().is_some() {
        return;
    }

    tokio::spawn(async move {
        let mut backoff = std::time::Duration::from_secs(2);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(600);
        let mut attempts: u32 = 0;
        let mut last_error = String::new();

        loop {
            // Check if another path initialized the pool while we slept
            if GLOBAL_POOL.get().is_some() {
                info!("database pool appeared (initialized elsewhere), reconnect loop exiting");
                return;
            }

            match PgPoolOptions::new()
                .max_connections(5)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&url)
                .await
            {
                Ok(pool) => {
                    match GLOBAL_POOL.set(pool) {
                        Ok(()) => info!(attempts, "database connected after background retries"),
                        Err(_) => info!("database pool was set by another path, discarding duplicate"),
                    }
                    return;
                }
                Err(e) => {
                    attempts += 1;
                    let err_msg = e.to_string();
                    if err_msg != last_error {
                        if !last_error.is_empty() {
                            info!(new_error = %err_msg, "database error changed, resetting backoff");
                        }
                        backoff = std::time::Duration::from_secs(2);
                        last_error = err_msg;
                    }
                    warn!(
                        error = %e,
                        attempt = attempts,
                        retry_in_secs = backoff.as_secs(),
                        "database reconnect: waiting to retry"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    });
}

/// Get the global pool (None if not initialized)
pub fn pool() -> Option<&'static PgPool> {
    GLOBAL_POOL.get()
}
