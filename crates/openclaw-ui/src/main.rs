mod auth;
mod dispatch;
mod errors;
mod frontend;
mod pagination;
mod router;
mod routes;
mod state;

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use sqlx::postgres::PgPoolOptions;
use tracing::{info, warn};

use openclaw_orchestrator::app::planner::PlannerService;
use openclaw_orchestrator::app::reconciler::Reconciler;
use openclaw_orchestrator::app::scheduler::Scheduler;
use openclaw_orchestrator::app::tick_coordinator::TickCoordinator;
use openclaw_orchestrator::app::worker_service::WorkerService;
use openclaw_orchestrator::domain::planner::Planner;
use openclaw_orchestrator::domain::ports::{EventStore, WorkerManager};
use openclaw_orchestrator::infra::event_store::PgEventStore;
use openclaw_orchestrator::infra::event_stream::EventStreamListener;
use openclaw_orchestrator::infra::planner::ClaudeCodePlanner;
use openclaw_orchestrator::infra::worker::ProcessWorkerManager;

use crate::dispatch::{spawn_completion_handler, spawn_event_dispatcher};
use crate::router::build_router;
use crate::state::{ApiConfig, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = ApiConfig::from_env()?;
    info!(listen_addr = %config.listen_addr, "openclaw-orch starting");

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("DATABASE_URL must be set"))?;

    let pool = {
        let mut backoff = std::time::Duration::from_secs(2);
        const MAX_BACKOFF: std::time::Duration = std::time::Duration::from_secs(600);
        let mut last_error = String::new();
        let mut attempts: u32 = 0;
        loop {
            match PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(5))
                .connect(&database_url)
                .await
            {
                Ok(pool) => {
                    if attempts > 0 {
                        info!(attempts, "database connected after retries");
                    }
                    break pool;
                }
                Err(e) => {
                    attempts += 1;
                    let err_msg = e.to_string();
                    // Reset backoff if the error changed (e.g. host came up but auth fails)
                    if err_msg != last_error {
                        if !last_error.is_empty() {
                            info!(
                                new_error = %err_msg,
                                "database error changed, resetting backoff"
                            );
                        }
                        backoff = std::time::Duration::from_secs(2);
                        last_error = err_msg;
                    }
                    warn!(
                        error = %e,
                        attempt = attempts,
                        retry_in_secs = backoff.as_secs(),
                        "database unreachable, waiting to retry"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        }
    };

    info!("database pool connected");

    let event_store = Arc::new(PgEventStore::new(pool.clone()));
    let event_tx = event_store.sender();

    let instance_id = config.instance_id;
    let data_dir = config.data_dir.clone();

    let state = AppState {
        pool: pool.clone(),
        event_store: event_store.clone() as Arc<dyn EventStore>,
        event_tx: event_tx.clone(),
        config,
    };

    // ── Background loops (only if instance_id is configured) ──
    if let Some(instance_id) = instance_id {
        let data_dir = data_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/shawaz/.openclaw/data"));

        info!(instance_id = %instance_id, data_dir = %data_dir.display(), "starting background loops");

        // Create worker completion channel
        let (completion_tx, completion_rx) = tokio::sync::mpsc::unbounded_channel();

        // Create ProcessWorkerManager
        let worker_manager = Arc::new(
            ProcessWorkerManager::new(data_dir.clone(), None)
                .with_completion_sender(completion_tx),
        ) as Arc<dyn WorkerManager>;

        // Create ClaudeCodePlanner
        let planner: Arc<dyn Planner> = Arc::new(
            ClaudeCodePlanner::new(data_dir.clone(), None),
        );

        // Create services
        let event_store_dyn: Arc<dyn EventStore> = event_store.clone();
        let scheduler = Scheduler::new(pool.clone());
        let reconciler = Reconciler::new(pool.clone(), worker_manager.clone(), event_store_dyn.clone());
        let worker_service = Arc::new(WorkerService::new(
            worker_manager,
            event_store_dyn.clone(),
            data_dir,
        ));
        let planner_service = Arc::new(PlannerService::new(
            planner,
            event_store_dyn.clone(),
            pool.clone(),
        ));

        // Create TickCoordinator
        let tick_coordinator = Arc::new(TickCoordinator::new(
            pool.clone(),
            scheduler,
            reconciler,
            worker_service,
            event_store_dyn.clone(),
        ));

        // Seed orch_server_capacity (INSERT ON CONFLICT DO NOTHING)
        sqlx::query(
            "INSERT INTO orch_server_capacity (instance_id, active_runs, max_concurrent, updated_at) VALUES ($1, 0, 3, $2) ON CONFLICT (instance_id) DO NOTHING",
        )
        .bind(instance_id)
        .bind(Utc::now())
        .execute(&pool)
        .await?;
        info!("server capacity seeded");

        // ── Spawn background tasks ──

        // 1. Event stream listener (Postgres LISTEN/NOTIFY → broadcast)
        let stream_listener = EventStreamListener::new(pool.clone(), event_tx.clone());
        tokio::spawn(async move {
            if let Err(e) = stream_listener.run().await {
                tracing::error!(error = %e, "event stream listener exited with error");
            }
        });
        info!("event stream listener spawned");

        // 2. Event dispatcher (routes events to handlers: PlanRequested → plan generation)
        let event_rx = event_store.subscribe();
        spawn_event_dispatcher(
            pool.clone(),
            event_store_dyn.clone(),
            planner_service,
            event_rx,
            instance_id,
        );
        info!("event dispatcher spawned");

        // 3. Worker completion handler
        spawn_completion_handler(pool.clone(), event_store_dyn.clone(), completion_rx);
        info!("worker completion handler spawned");

        // 4. Scheduler tick loop (every 10s) — ticks all active instances
        let sched_coord = tick_coordinator.clone();
        let sched_pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(sched_coord.scheduler_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let ids = match sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM orch_instances WHERE state = 'active'",
                )
                .fetch_all(&sched_pool)
                .await
                {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::error!(error = %e, "scheduler: failed to list active instances");
                        continue;
                    }
                };
                for id in ids {
                    if let Err(e) = sched_coord.scheduler_tick(id).await {
                        tracing::error!(error = %e, instance_id = %id, "scheduler tick failed");
                    }
                }
            }
        });
        info!("scheduler loop spawned (10s interval)");

        // 5. Reconciler tick loop (every 30s) — ticks all active instances
        let recon_coord = tick_coordinator.clone();
        let recon_pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(recon_coord.reconciler_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let ids = match sqlx::query_scalar::<_, uuid::Uuid>(
                    "SELECT id FROM orch_instances WHERE state = 'active'",
                )
                .fetch_all(&recon_pool)
                .await
                {
                    Ok(ids) => ids,
                    Err(e) => {
                        tracing::error!(error = %e, "reconciler: failed to list active instances");
                        continue;
                    }
                };
                for id in ids {
                    if let Err(e) = recon_coord.reconciler_tick(id).await {
                        tracing::error!(error = %e, instance_id = %id, "reconciler tick failed");
                    }
                }
            }
        });
        info!("reconciler loop spawned (30s interval)");

        // 6. Heartbeat loop (every 15s) — updates all active instances
        let hb_pool = pool.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let now = Utc::now();
                if let Err(e) = sqlx::query(
                    "UPDATE orch_instances SET last_heartbeat = $1 WHERE state = 'active'",
                )
                .bind(now)
                .execute(&hb_pool)
                .await
                {
                    tracing::warn!(error = %e, "heartbeat update failed");
                }
            }
        });
        info!("heartbeat loop spawned (15s interval)");

        info!(instance_id = %instance_id, "all background loops started");
    } else {
        info!("no OPENCLAW_INSTANCE_ID set — running in dashboard-only mode (no background loops)");
    }

    let listen_addr = state.config.listen_addr.clone();
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr).await?;
    info!("listening on {listen_addr}");

    axum::serve(listener, app).await?;

    Ok(())
}
