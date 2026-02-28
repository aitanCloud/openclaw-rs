mod auth;
mod errors;
mod pagination;
mod router;
mod routes;
mod state;

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tracing::info;

use openclaw_orchestrator::infra::event_store::PgEventStore;

use crate::router::build_router;
use crate::state::{ApiConfig, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = ApiConfig::from_env()?;
    info!(listen_addr = %config.listen_addr, "openclaw-orch starting");

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await?;

    info!("database pool connected");

    let event_store = Arc::new(PgEventStore::new(pool.clone()));
    let event_tx = event_store.sender();

    let state = AppState {
        pool,
        event_store,
        event_tx,
        config,
    };

    let listen_addr = state.config.listen_addr.clone();
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    info!("listening on {listen_addr}");

    axum::serve(listener, app).await?;

    Ok(())
}
