use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::app::reconciler::Reconciler;
use crate::app::scheduler::{ClaimResult, Scheduler};
use crate::app::worker_service::WorkerService;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::EventStore;
use crate::infra::errors::InfraError;

// ── Constants ──

/// Time after which a run stuck in 'claimed' state is considered stale (5 minutes).
pub(crate) const CLAIM_TIMEOUT_SECS: u64 = 300;

/// Default scheduler tick interval (10 seconds).
pub const DEFAULT_SCHEDULER_INTERVAL_SECS: u64 = 10;

/// Default reconciler tick interval (30 seconds).
pub const DEFAULT_RECONCILER_INTERVAL_SECS: u64 = 30;

// ── Helper types for DB queries ──

/// Row type for reading instance state from `orch_instances`.
#[derive(sqlx::FromRow)]
struct InstanceStateRow {
    state: String,
}

/// Row type for reading the task prompt from `orch_tasks`.
#[derive(sqlx::FromRow)]
struct TaskPromptRow {
    prompt: String,
}

/// A run that has been stuck in 'claimed' state past the timeout.
#[derive(Debug, Clone)]
pub(crate) struct StaleClaim {
    pub run_id: Uuid,
    pub task_id: Uuid,
    pub instance_id: Uuid,
}

/// Row type for stale claim detection query.
#[derive(sqlx::FromRow)]
struct StaleClaimRow {
    id: Uuid,
    task_id: Uuid,
    instance_id: Uuid,
}

// ── Pure helper functions (testable without DB) ──

/// Check whether the given instance state string represents maintenance mode.
///
/// Maintenance mode is defined as state = 'blocked' (the reason check is
/// deferred to the caller who has access to additional metadata).
pub(crate) fn is_maintenance_mode(state: &str) -> bool {
    state == "blocked"
}

/// Check whether the given instance state represents an active instance
/// that should participate in scheduler ticks.
pub(crate) fn is_instance_active(state: &str) -> bool {
    state == "active"
}

/// Build a RunFailed(SpawnFailure) event for a stale claim.
pub(crate) fn build_stale_claim_event(stale: &StaleClaim) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id: stale.instance_id,
        seq: 0,
        event_type: "RunFailed".to_string(),
        event_version: 1,
        payload: serde_json::json!({
            "run_id": stale.run_id,
            "task_id": stale.task_id,
            "failure_category": "SpawnFailure",
            "error_message": format!(
                "run {} stuck in claimed state for over {} seconds",
                stale.run_id, CLAIM_TIMEOUT_SECS
            ),
        }),
        idempotency_key: Some(format!("stale-claim-failed-{}", stale.run_id)),
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

// ── TickCoordinator ──

/// Coordinates the periodic operational loops of the orchestrator.
///
/// Two independent ticks run on configurable intervals:
/// - **Scheduler tick**: claims tasks and spawns workers
/// - **Reconciler tick**: detects anomalies and repairs state
///
/// Individual tick failures are logged but do not crash the coordinator.
pub struct TickCoordinator {
    pool: PgPool,
    scheduler: Scheduler,
    reconciler: Reconciler,
    worker_service: Arc<WorkerService>,
    event_store: Arc<dyn EventStore>,
    pub scheduler_interval: Duration,
    pub reconciler_interval: Duration,
}

impl TickCoordinator {
    pub fn new(
        pool: PgPool,
        scheduler: Scheduler,
        reconciler: Reconciler,
        worker_service: Arc<WorkerService>,
        event_store: Arc<dyn EventStore>,
    ) -> Self {
        Self {
            pool,
            scheduler,
            reconciler,
            worker_service,
            event_store,
            scheduler_interval: Duration::from_secs(DEFAULT_SCHEDULER_INTERVAL_SECS),
            reconciler_interval: Duration::from_secs(DEFAULT_RECONCILER_INTERVAL_SECS),
        }
    }

    /// Override the default scheduler tick interval.
    pub fn with_scheduler_interval(mut self, interval: Duration) -> Self {
        self.scheduler_interval = interval;
        self
    }

    /// Override the default reconciler tick interval.
    pub fn with_reconciler_interval(mut self, interval: Duration) -> Self {
        self.reconciler_interval = interval;
        self
    }

    // ── Scheduler tick ──

    /// Execute a single scheduler tick: claim tasks and spawn workers.
    ///
    /// Safe-conditions gate: skips if the instance is not in 'active' state.
    /// Errors on individual worker spawns do not abort remaining spawns.
    pub async fn scheduler_tick(&self, instance_id: Uuid) -> Result<(), AppError> {
        // 1. Safe-conditions gate: query instance state
        let instance_state = self.query_instance_state(instance_id).await?;
        if !is_instance_active(&instance_state) {
            tracing::info!(
                instance_id = %instance_id,
                state = %instance_state,
                "scheduler_tick: skipping, instance not active"
            );
            return Ok(());
        }

        // 2. Run scheduler to claim tasks
        let claims = self.scheduler.tick(instance_id).await?;
        if claims.is_empty() {
            tracing::info!(instance_id = %instance_id, "scheduler_tick: no tasks claimed");
            return Ok(());
        }

        tracing::info!(
            instance_id = %instance_id,
            claimed = claims.len(),
            "scheduler_tick: tasks claimed, spawning workers"
        );

        // 3. For each claim, look up prompt and spawn worker (best-effort)
        let mut spawned = 0u32;
        let mut failed = 0u32;

        for claim in &claims {
            match self.spawn_for_claim(claim).await {
                Ok(()) => {
                    spawned += 1;
                }
                Err(e) => {
                    failed += 1;
                    tracing::error!(
                        run_id = %claim.run_id,
                        task_id = %claim.task_id,
                        error = %e,
                        "scheduler_tick: worker spawn failed, continuing"
                    );
                }
            }
        }

        tracing::info!(
            instance_id = %instance_id,
            claimed = claims.len(),
            spawned = spawned,
            failed = failed,
            "scheduler_tick: complete"
        );

        Ok(())
    }

    /// Look up the task prompt and spawn a worker for a single claim.
    async fn spawn_for_claim(&self, claim: &ClaimResult) -> Result<(), AppError> {
        // Look up the task prompt
        let prompt = self.query_task_prompt(claim.task_id, claim.instance_id).await?;

        // Build the worktree path: {data_dir}/{instance_id}/{run_id}
        let worktree_path = format!(
            "{}/{}/{}",
            self.worker_service.data_dir().display(),
            claim.instance_id,
            claim.run_id
        );

        self.worker_service
            .spawn_worker(claim, &prompt, &worktree_path)
            .await
    }

    // ── Reconciler tick ──

    /// Execute a single reconciler tick: reconcile active runs and detect stale claims.
    ///
    /// Maintenance mode: if instance is 'blocked', logs diagnostics but does NOT
    /// emit state-changing events (reconciliation and stale claim detection are skipped).
    pub async fn reconciler_tick(&self, instance_id: Uuid) -> Result<(), AppError> {
        // 1. Check instance state for maintenance mode
        let instance_state = self.query_instance_state(instance_id).await?;

        if is_maintenance_mode(&instance_state) {
            tracing::info!(
                instance_id = %instance_id,
                state = %instance_state,
                "reconciler_tick: maintenance mode, skipping state-changing operations"
            );
            return Ok(());
        }

        // Only reconcile if instance is active
        if !is_instance_active(&instance_state) {
            tracing::info!(
                instance_id = %instance_id,
                state = %instance_state,
                "reconciler_tick: skipping, instance not active"
            );
            return Ok(());
        }

        // 2. Reconcile active runs (reattach / abandon detection)
        let reconciled = self.reconciler.reconcile_active_runs(instance_id).await?;
        tracing::info!(
            instance_id = %instance_id,
            reconciled = reconciled,
            "reconciler_tick: active run reconciliation complete"
        );

        // 3. Stale claim detection
        let stale_claims = self.detect_stale_claims(instance_id).await?;
        if !stale_claims.is_empty() {
            tracing::info!(
                instance_id = %instance_id,
                stale_count = stale_claims.len(),
                "reconciler_tick: detected stale claims, emitting RunFailed events"
            );

            for stale in &stale_claims {
                let envelope = build_stale_claim_event(stale);
                if let Err(e) = self.event_store.emit(envelope).await {
                    tracing::error!(
                        run_id = %stale.run_id,
                        error = %e,
                        "reconciler_tick: failed to emit stale claim RunFailed event"
                    );
                }
            }
        }

        Ok(())
    }

    // ── DB query helpers ──

    /// Query the current state of an instance from `orch_instances`.
    async fn query_instance_state(&self, instance_id: Uuid) -> Result<String, AppError> {
        let row = sqlx::query_as::<_, InstanceStateRow>(
            "SELECT state FROM orch_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        match row {
            Some(r) => Ok(r.state),
            None => {
                tracing::error!(instance_id = %instance_id, "instance not found in orch_instances");
                Err(AppError::Infra(InfraError::Database(format!(
                    "instance {instance_id} not found"
                ))))
            }
        }
    }

    /// Query the prompt for a task from `orch_tasks`.
    async fn query_task_prompt(
        &self,
        task_id: Uuid,
        instance_id: Uuid,
    ) -> Result<String, AppError> {
        let row = sqlx::query_as::<_, TaskPromptRow>(
            "SELECT prompt FROM orch_tasks WHERE id = $1 AND instance_id = $2",
        )
        .bind(task_id)
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        match row {
            Some(r) => Ok(r.prompt),
            None => {
                tracing::error!(
                    task_id = %task_id,
                    instance_id = %instance_id,
                    "task not found in orch_tasks"
                );
                Err(AppError::Infra(InfraError::Database(format!(
                    "task {task_id} not found for instance {instance_id}"
                ))))
            }
        }
    }

    /// Detect runs stuck in 'claimed' state past the claim timeout.
    ///
    /// Returns a list of stale claims that should be failed.
    pub(crate) async fn detect_stale_claims(
        &self,
        instance_id: Uuid,
    ) -> Result<Vec<StaleClaim>, AppError> {
        let timeout_secs = CLAIM_TIMEOUT_SECS as i64;

        let rows = sqlx::query_as::<_, StaleClaimRow>(
            r#"
            SELECT id, task_id, instance_id
            FROM orch_runs
            WHERE instance_id = $1
              AND state = 'claimed'
              AND started_at < NOW() - make_interval(secs => $2::double precision)
            "#,
        )
        .bind(instance_id)
        .bind(timeout_secs)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        Ok(rows
            .into_iter()
            .map(|r| StaleClaim {
                run_id: r.id,
                task_id: r.task_id,
                instance_id: r.instance_id,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;
    use crate::domain::ports::EventStore;
    use crate::domain::worker::{WorkerError, WorkerHandle, WorkerSpawnConfig};
    use crate::domain::ports::WorkerManager;

    // ── Constants tests ──

    #[test]
    fn claim_timeout_is_five_minutes() {
        assert_eq!(CLAIM_TIMEOUT_SECS, 300);
    }

    #[test]
    fn default_scheduler_interval_is_ten_seconds() {
        assert_eq!(DEFAULT_SCHEDULER_INTERVAL_SECS, 10);
    }

    #[test]
    fn default_reconciler_interval_is_thirty_seconds() {
        assert_eq!(DEFAULT_RECONCILER_INTERVAL_SECS, 30);
    }

    // ── is_instance_active tests ──

    #[test]
    fn active_state_is_active() {
        assert!(is_instance_active("active"));
    }

    #[test]
    fn blocked_state_is_not_active() {
        assert!(!is_instance_active("blocked"));
    }

    #[test]
    fn provisioning_state_is_not_active() {
        assert!(!is_instance_active("provisioning"));
    }

    #[test]
    fn suspended_state_is_not_active() {
        assert!(!is_instance_active("suspended"));
    }

    #[test]
    fn provisioning_failed_state_is_not_active() {
        assert!(!is_instance_active("provisioning_failed"));
    }

    #[test]
    fn empty_state_is_not_active() {
        assert!(!is_instance_active(""));
    }

    // ── is_maintenance_mode tests ──

    #[test]
    fn blocked_is_maintenance() {
        assert!(is_maintenance_mode("blocked"));
    }

    #[test]
    fn active_is_not_maintenance() {
        assert!(!is_maintenance_mode("active"));
    }

    #[test]
    fn provisioning_is_not_maintenance() {
        assert!(!is_maintenance_mode("provisioning"));
    }

    #[test]
    fn suspended_is_not_maintenance() {
        assert!(!is_maintenance_mode("suspended"));
    }

    // ── build_stale_claim_event tests ──

    #[test]
    fn stale_claim_event_type_is_run_failed() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.event_type, "RunFailed");
    }

    #[test]
    fn stale_claim_event_version_is_one() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.event_version, 1);
    }

    #[test]
    fn stale_claim_event_seq_is_zero() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.seq, 0);
    }

    #[test]
    fn stale_claim_event_has_spawn_failure_category() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.payload["failure_category"], "SpawnFailure");
    }

    #[test]
    fn stale_claim_event_has_correct_run_id() {
        let run_id = Uuid::new_v4();
        let stale = StaleClaim {
            run_id,
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.payload["run_id"], run_id.to_string());
    }

    #[test]
    fn stale_claim_event_has_correct_task_id() {
        let task_id = Uuid::new_v4();
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id,
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.payload["task_id"], task_id.to_string());
    }

    #[test]
    fn stale_claim_event_has_correct_instance_id() {
        let instance_id = Uuid::new_v4();
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id,
        };
        let event = build_stale_claim_event(&stale);
        assert_eq!(event.instance_id, instance_id);
    }

    #[test]
    fn stale_claim_event_has_idempotency_key() {
        let run_id = Uuid::new_v4();
        let stale = StaleClaim {
            run_id,
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        let key = event
            .idempotency_key
            .as_ref()
            .expect("should have idempotency key");
        assert_eq!(*key, format!("stale-claim-failed-{}", run_id));
    }

    #[test]
    fn stale_claim_event_error_message_mentions_timeout() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        let msg = event.payload["error_message"].as_str().unwrap();
        assert!(msg.contains("300"), "error message should mention timeout seconds");
        assert!(msg.contains("claimed"), "error message should mention claimed state");
    }

    #[test]
    fn stale_claim_event_has_no_correlation_or_causation() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let event = build_stale_claim_event(&stale);
        assert!(event.correlation_id.is_none());
        assert!(event.causation_id.is_none());
    }

    // ── StaleClaim struct tests ──

    #[test]
    fn stale_claim_debug() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let debug = format!("{stale:?}");
        assert!(debug.contains("StaleClaim"));
        assert!(debug.contains("run_id"));
    }

    #[test]
    fn stale_claim_clone() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let cloned = stale.clone();
        assert_eq!(stale.run_id, cloned.run_id);
        assert_eq!(stale.task_id, cloned.task_id);
        assert_eq!(stale.instance_id, cloned.instance_id);
    }

    // ── TickCoordinator construction tests ──

    /// Mock WorkerManager for coordinator construction tests.
    struct NoopWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for NoopWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in construction tests")
        }

        async fn is_alive(&self, _session_id: Uuid) -> Result<bool, WorkerError> {
            Ok(false)
        }

        async fn cancel(&self, _session_id: Uuid) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn kill(&self, _session_id: Uuid) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn reattach(
            &self,
            _session_id: Uuid,
            _run_id: Uuid,
        ) -> Result<Option<WorkerHandle>, WorkerError> {
            Ok(None)
        }
    }

    /// Mock EventStore for tests.
    struct RecordingEventStore {
        events: tokio::sync::Mutex<Vec<EventEnvelope>>,
    }

    impl RecordingEventStore {
        fn new() -> Self {
            Self {
                events: tokio::sync::Mutex::new(Vec::new()),
            }
        }

        #[allow(dead_code)]
        async fn emitted_events(&self) -> Vec<EventEnvelope> {
            self.events.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl EventStore for RecordingEventStore {
        async fn emit(&self, event: EventEnvelope) -> Result<i64, DomainError> {
            let mut events = self.events.lock().await;
            let seq = events.len() as i64 + 1;
            events.push(event);
            Ok(seq)
        }

        async fn replay(
            &self,
            _instance_id: Uuid,
            _since_seq: i64,
        ) -> Result<Vec<EventEnvelope>, DomainError> {
            Ok(Vec::new())
        }
    }

    /// Create a lazy PgPool that is never actually connected (for unit tests).
    fn lazy_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .expect("lazy pool creation should not fail")
    }

    fn make_coordinator(event_store: Arc<dyn EventStore>) -> TickCoordinator {
        let pool = lazy_pool();
        let wm: Arc<dyn WorkerManager> = Arc::new(NoopWorkerManager);
        let scheduler = Scheduler::new(pool.clone());
        let reconciler = Reconciler::new(pool.clone(), wm.clone(), event_store.clone());
        let worker_service = Arc::new(WorkerService::new(
            wm,
            event_store.clone(),
            std::path::PathBuf::from("/tmp/openclaw"),
        ));

        TickCoordinator::new(pool, scheduler, reconciler, worker_service, event_store)
    }

    #[tokio::test]
    async fn coordinator_is_constructible() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _coord = make_coordinator(es);
    }

    #[tokio::test]
    async fn coordinator_default_intervals() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let coord = make_coordinator(es);
        assert_eq!(coord.scheduler_interval, Duration::from_secs(10));
        assert_eq!(coord.reconciler_interval, Duration::from_secs(30));
    }

    #[tokio::test]
    async fn coordinator_custom_scheduler_interval() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let coord = make_coordinator(es).with_scheduler_interval(Duration::from_secs(5));
        assert_eq!(coord.scheduler_interval, Duration::from_secs(5));
    }

    #[tokio::test]
    async fn coordinator_custom_reconciler_interval() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let coord = make_coordinator(es).with_reconciler_interval(Duration::from_secs(60));
        assert_eq!(coord.reconciler_interval, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn coordinator_chained_interval_overrides() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let coord = make_coordinator(es)
            .with_scheduler_interval(Duration::from_secs(3))
            .with_reconciler_interval(Duration::from_secs(15));
        assert_eq!(coord.scheduler_interval, Duration::from_secs(3));
        assert_eq!(coord.reconciler_interval, Duration::from_secs(15));
    }

    // ── Multiple stale claims produce distinct events ──

    #[test]
    fn stale_claim_events_have_unique_idempotency_keys() {
        let stale1 = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };
        let stale2 = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };

        let event1 = build_stale_claim_event(&stale1);
        let event2 = build_stale_claim_event(&stale2);

        assert_ne!(
            event1.idempotency_key, event2.idempotency_key,
            "different stale claims should have different idempotency keys"
        );
    }

    #[test]
    fn stale_claim_events_have_unique_event_ids() {
        let stale = StaleClaim {
            run_id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
        };

        let event1 = build_stale_claim_event(&stale);
        let event2 = build_stale_claim_event(&stale);

        assert_ne!(
            event1.event_id, event2.event_id,
            "each event should get a unique event_id"
        );
    }

    // ── Edge case: all instance states ──

    #[test]
    fn all_instance_states_gate_correctly() {
        let states_and_expected: Vec<(&str, bool, bool)> = vec![
            //              (state,               active, maintenance)
            ("active",              true,   false),
            ("blocked",             false,  true),
            ("suspended",           false,  false),
            ("provisioning",        false,  false),
            ("provisioning_failed", false,  false),
        ];

        for (state, expected_active, expected_maintenance) in states_and_expected {
            assert_eq!(
                is_instance_active(state),
                expected_active,
                "is_instance_active(\"{state}\") should be {expected_active}"
            );
            assert_eq!(
                is_maintenance_mode(state),
                expected_maintenance,
                "is_maintenance_mode(\"{state}\") should be {expected_maintenance}"
            );
        }
    }
}
