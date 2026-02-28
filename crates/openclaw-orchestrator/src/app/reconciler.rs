use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::{EventStore, WorkerManager};
use crate::infra::errors::InfraError;

/// Row returned from the orch_runs query for active runs.
#[derive(sqlx::FromRow)]
pub(crate) struct ActiveRun {
    pub id: Uuid,
    pub task_id: Uuid,
    pub instance_id: Uuid,
    pub worker_session_id: Uuid,
    pub state: String,
}

/// The Reconciler detects and repairs anomalies in the orchestrator state.
///
/// On startup:
/// - Scans for runs in 'running' or 'claimed' state
/// - Attempts to reattach to their worker processes
/// - Emits RunAbandoned for unreachable workers in 'running' state
/// - Emits RunFailed (SpawnFailure) for unreachable workers in 'claimed' state
///
/// Actor: System (all events emitted by reconciler use Actor::System)
pub struct Reconciler {
    pool: PgPool,
    worker_manager: Arc<dyn WorkerManager>,
    event_store: Arc<dyn EventStore>,
}

impl Reconciler {
    pub fn new(
        pool: PgPool,
        worker_manager: Arc<dyn WorkerManager>,
        event_store: Arc<dyn EventStore>,
    ) -> Self {
        Self {
            pool,
            worker_manager,
            event_store,
        }
    }

    /// Reconcile all active runs for a given instance on startup.
    ///
    /// Scans orch_runs for state IN ('running', 'claimed') for this instance,
    /// attempts reattach, and emits appropriate events for unreachable workers:
    /// - `RunAbandoned` for runs in 'running' state
    /// - `RunFailed` (SpawnFailure) for runs in 'claimed' state
    ///
    /// Returns the number of abandoned/failed runs.
    pub async fn reconcile_active_runs(&self, instance_id: Uuid) -> Result<u32, AppError> {
        let active_runs = sqlx::query_as::<_, ActiveRun>(
            r#"
            SELECT id, task_id, instance_id, worker_session_id, state
            FROM orch_runs
            WHERE instance_id = $1 AND state IN ('running', 'claimed')
            "#,
        )
        .bind(instance_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        self.reconcile_runs(&active_runs, instance_id).await
    }

    /// Core reconciliation logic, separated for testability.
    ///
    /// Processes a list of active runs: attempts reattach for each,
    /// emits events for unreachable workers. Errors on individual runs
    /// are logged but do not abort reconciliation of remaining runs.
    pub(crate) async fn reconcile_runs(
        &self,
        active_runs: &[ActiveRun],
        instance_id: Uuid,
    ) -> Result<u32, AppError> {
        if active_runs.is_empty() {
            tracing::info!(instance_id = %instance_id, "no active runs to reconcile");
            return Ok(0);
        }

        tracing::info!(
            instance_id = %instance_id,
            count = active_runs.len(),
            "reconciling active runs"
        );

        let mut abandoned_count = 0u32;

        for run in active_runs {
            match self
                .worker_manager
                .reattach(run.worker_session_id, run.id)
                .await
            {
                Ok(Some(_handle)) => {
                    // Successfully reattached -- worker is still alive.
                    tracing::info!(
                        run_id = %run.id,
                        session_id = %run.worker_session_id,
                        "reattached to worker"
                    );
                }
                Ok(None) => {
                    // Worker is dead -- emit appropriate event based on state.
                    tracing::warn!(
                        run_id = %run.id,
                        session_id = %run.worker_session_id,
                        state = %run.state,
                        "worker not reachable, emitting abandonment event"
                    );

                    let envelope = self.build_dead_worker_event(run);

                    self.event_store.emit(envelope).await.map_err(|e| {
                        AppError::Infra(InfraError::Database(format!(
                            "emit event for dead worker run {}: {e}",
                            run.id
                        )))
                    })?;

                    abandoned_count += 1;
                }
                Err(e) => {
                    // Reattach call itself failed -- log and continue.
                    // Do NOT abort reconciliation of other runs.
                    tracing::error!(
                        run_id = %run.id,
                        session_id = %run.worker_session_id,
                        error = %e,
                        "reattach failed, skipping"
                    );
                }
            }
        }

        tracing::info!(
            instance_id = %instance_id,
            total = active_runs.len(),
            abandoned = abandoned_count,
            "reconciliation complete"
        );

        Ok(abandoned_count)
    }

    /// Build the correct event for a dead worker based on the run's state.
    ///
    /// - 'running' state -> RunAbandoned (worker was alive, now unreachable)
    /// - 'claimed' state -> RunFailed with SpawnFailure (worker never started)
    fn build_dead_worker_event(&self, run: &ActiveRun) -> EventEnvelope {
        let now = Utc::now();

        if run.state == "claimed" {
            // Claimed but worker is dead -> spawn never succeeded.
            EventEnvelope {
                event_id: Uuid::new_v4(),
                instance_id: run.instance_id,
                seq: 0,
                event_type: "RunFailed".to_string(),
                event_version: 1,
                payload: serde_json::json!({
                    "run_id": run.id,
                    "task_id": run.task_id,
                    "failure_category": "SpawnFailure",
                    "error_message": "worker not reachable after restart (claimed but never started)",
                }),
                idempotency_key: Some(format!("reconcile-abandon-{}", run.id)),
                correlation_id: None,
                causation_id: None,
                occurred_at: now,
                recorded_at: now,
            }
        } else {
            // Running but worker is dead -> abandoned.
            EventEnvelope {
                event_id: Uuid::new_v4(),
                instance_id: run.instance_id,
                seq: 0,
                event_type: "RunAbandoned".to_string(),
                event_version: 1,
                payload: serde_json::json!({
                    "run_id": run.id,
                    "reason": "worker not reachable after restart",
                }),
                idempotency_key: Some(format!("reconcile-abandon-{}", run.id)),
                correlation_id: None,
                causation_id: None,
                occurred_at: now,
                recorded_at: now,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;
    use crate::domain::worker::{WorkerError, WorkerHandle, WorkerSpawnConfig};

    // ── Mock WorkerManager: always reattaches successfully ──

    struct AlwaysAliveWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for AlwaysAliveWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in reconciler tests")
        }

        async fn is_alive(&self, _session_id: Uuid) -> Result<bool, WorkerError> {
            Ok(true)
        }

        async fn cancel(&self, _session_id: Uuid) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn kill(&self, _session_id: Uuid) -> Result<(), WorkerError> {
            Ok(())
        }

        async fn reattach(
            &self,
            session_id: Uuid,
            run_id: Uuid,
        ) -> Result<Option<WorkerHandle>, WorkerError> {
            Ok(Some(WorkerHandle {
                session_id,
                run_id,
                log_stdout: "/tmp/stdout.log".to_string(),
                log_stderr: "/tmp/stderr.log".to_string(),
            }))
        }
    }

    // ── Mock WorkerManager: always returns None (worker dead) ──

    struct AlwaysDeadWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for AlwaysDeadWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in reconciler tests")
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

    // ── Mock WorkerManager: errors on reattach ──

    struct ErrorWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for ErrorWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in reconciler tests")
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
            Err(WorkerError::IoError("connection refused".to_string()))
        }
    }

    // ── Mock WorkerManager: mixed results (first call alive, rest dead) ──

    struct MixedWorkerManager {
        alive_session_ids: std::collections::HashSet<Uuid>,
    }

    #[async_trait::async_trait]
    impl WorkerManager for MixedWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in reconciler tests")
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
            session_id: Uuid,
            run_id: Uuid,
        ) -> Result<Option<WorkerHandle>, WorkerError> {
            if self.alive_session_ids.contains(&session_id) {
                Ok(Some(WorkerHandle {
                    session_id,
                    run_id,
                    log_stdout: "/tmp/stdout.log".to_string(),
                    log_stderr: "/tmp/stderr.log".to_string(),
                }))
            } else {
                Ok(None)
            }
        }
    }

    // ── Mock WorkerManager: first errors, rest dead ──

    struct ErrorThenDeadWorkerManager {
        error_session_ids: std::collections::HashSet<Uuid>,
    }

    #[async_trait::async_trait]
    impl WorkerManager for ErrorThenDeadWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            unimplemented!("not used in reconciler tests")
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
            if self.error_session_ids.contains(&_session_id) {
                Err(WorkerError::IoError("connection refused".to_string()))
            } else {
                Ok(None)
            }
        }
    }

    // ── Mock EventStore that records emitted events ──

    struct RecordingEventStore {
        events: tokio::sync::Mutex<Vec<EventEnvelope>>,
    }

    impl RecordingEventStore {
        fn new() -> Self {
            Self {
                events: tokio::sync::Mutex::new(Vec::new()),
            }
        }

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

    // ── Helpers ──

    fn make_active_run_with_instance(state: &str, instance_id: Uuid) -> ActiveRun {
        ActiveRun {
            id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id,
            worker_session_id: Uuid::new_v4(),
            state: state.to_string(),
        }
    }

    /// Build a Reconciler for testing using the core `reconcile_runs` method
    /// (bypasses DB query). Uses a dummy PgPool that is never actually connected.
    fn make_reconciler(
        worker_manager: Arc<dyn WorkerManager>,
        event_store: Arc<dyn EventStore>,
    ) -> Reconciler {
        // Create a PgPool with a bogus connection string. It will never be used
        // because tests call reconcile_runs() directly, not reconcile_active_runs().
        let pool_opts = sqlx::postgres::PgPoolOptions::new().max_connections(1);
        let pool = pool_opts
            .connect_lazy("postgres://fake:fake@localhost/fake")
            .expect("lazy pool creation should not fail");
        Reconciler::new(pool, worker_manager, event_store)
    }

    // ── Tests ──

    #[tokio::test]
    async fn reconciler_is_constructible() {
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysAliveWorkerManager);
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _reconciler = make_reconciler(wm, es);
    }

    #[test]
    fn active_run_fields_accessible() {
        let run = ActiveRun {
            id: Uuid::new_v4(),
            task_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            worker_session_id: Uuid::new_v4(),
            state: "running".to_string(),
        };

        let _: Uuid = run.id;
        let _: Uuid = run.task_id;
        let _: Uuid = run.instance_id;
        let _: Uuid = run.worker_session_id;
        let _: &str = &run.state;
    }

    #[tokio::test]
    async fn reconcile_empty_returns_zero() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysAliveWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let result = reconciler.reconcile_runs(&[], instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);

        let events = event_store.emitted_events().await;
        assert!(events.is_empty(), "no events should be emitted for empty runs");
    }

    #[tokio::test]
    async fn reconcile_reattach_success_no_events() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysAliveWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let runs = vec![
            make_active_run_with_instance("running", instance_id),
            make_active_run_with_instance("claimed", instance_id),
        ];

        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0, "no runs should be abandoned");

        let events = event_store.emitted_events().await;
        assert!(events.is_empty(), "no events for successful reattach");
    }

    #[tokio::test]
    async fn reconcile_running_dead_emits_run_abandoned() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let runs = vec![make_active_run_with_instance("running", instance_id)];

        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "RunAbandoned");
    }

    #[tokio::test]
    async fn reconcile_claimed_dead_emits_run_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let runs = vec![make_active_run_with_instance("claimed", instance_id)];

        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 1);

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "RunFailed");

        let payload = &events[0].payload;
        assert_eq!(payload["failure_category"], "SpawnFailure");
        assert!(payload["error_message"].as_str().unwrap().contains("claimed"));
    }

    #[tokio::test]
    async fn reconcile_mixed_reattach_and_abandon() {
        let instance_id = Uuid::new_v4();
        let alive_run = make_active_run_with_instance("running", instance_id);
        let dead_running = make_active_run_with_instance("running", instance_id);
        let dead_claimed = make_active_run_with_instance("claimed", instance_id);

        let alive_session = alive_run.worker_session_id;
        let mut alive_set = std::collections::HashSet::new();
        alive_set.insert(alive_session);

        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(MixedWorkerManager {
            alive_session_ids: alive_set,
        });
        let reconciler = make_reconciler(wm, event_store.clone());

        let runs = vec![alive_run, dead_running, dead_claimed];
        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2, "two runs should be abandoned/failed");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 2);

        // First dead run was 'running' -> RunAbandoned
        assert_eq!(events[0].event_type, "RunAbandoned");
        // Second dead run was 'claimed' -> RunFailed
        assert_eq!(events[1].event_type, "RunFailed");
    }

    #[tokio::test]
    async fn run_abandoned_event_has_correct_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let run = make_active_run_with_instance("running", instance_id);
        let run_id = run.id;

        let result = reconciler.reconcile_runs(&[run], instance_id).await;
        assert!(result.is_ok());

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(event.event_type, "RunAbandoned");
        assert_eq!(event.event_version, 1);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.seq, 0, "seq should be 0 (assigned by event store)");

        let payload = &event.payload;
        assert_eq!(payload["run_id"], run_id.to_string());
        assert_eq!(payload["reason"], "worker not reachable after restart");
    }

    #[tokio::test]
    async fn run_abandoned_has_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let run = make_active_run_with_instance("running", instance_id);
        let run_id = run.id;

        reconciler
            .reconcile_runs(&[run], instance_id)
            .await
            .expect("reconcile ok");

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have idempotency key");
        assert_eq!(*key, format!("reconcile-abandon-{}", run_id));
    }

    #[tokio::test]
    async fn run_failed_has_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let run = make_active_run_with_instance("claimed", instance_id);
        let run_id = run.id;

        reconciler
            .reconcile_runs(&[run], instance_id)
            .await
            .expect("reconcile ok");

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have idempotency key");
        assert_eq!(*key, format!("reconcile-abandon-{}", run_id));
    }

    #[tokio::test]
    async fn reconcile_reattach_error_continues_to_next_run() {
        let instance_id = Uuid::new_v4();
        let error_run = make_active_run_with_instance("running", instance_id);
        let dead_run = make_active_run_with_instance("running", instance_id);

        let error_session = error_run.worker_session_id;
        let mut error_set = std::collections::HashSet::new();
        error_set.insert(error_session);

        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(ErrorThenDeadWorkerManager {
            error_session_ids: error_set,
        });
        let reconciler = make_reconciler(wm, event_store.clone());

        let runs = vec![error_run, dead_run];
        let result = reconciler.reconcile_runs(&runs, instance_id).await;

        // Should succeed despite one reattach error.
        assert!(result.is_ok());
        // Only the dead run counts as abandoned (error run is skipped).
        assert_eq!(result.unwrap(), 1);

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1, "only dead run should emit event");
        assert_eq!(events[0].event_type, "RunAbandoned");
    }

    #[tokio::test]
    async fn reconcile_all_errors_returns_zero() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(ErrorWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let runs = vec![
            make_active_run_with_instance("running", instance_id),
            make_active_run_with_instance("claimed", instance_id),
        ];

        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0, "errors are skipped, not counted");

        let events = event_store.emitted_events().await;
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn run_failed_event_has_correct_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysDeadWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let run = make_active_run_with_instance("claimed", instance_id);
        let run_id = run.id;
        let task_id = run.task_id;

        reconciler
            .reconcile_runs(&[run], instance_id)
            .await
            .expect("reconcile ok");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);

        let event = &events[0];
        assert_eq!(event.event_type, "RunFailed");
        assert_eq!(event.event_version, 1);
        assert_eq!(event.instance_id, instance_id);

        let payload = &event.payload;
        assert_eq!(payload["run_id"], run_id.to_string());
        assert_eq!(payload["task_id"], task_id.to_string());
        assert_eq!(payload["failure_category"], "SpawnFailure");
        assert!(payload["error_message"].as_str().unwrap().contains("not reachable"));
    }

    #[tokio::test]
    async fn reconcile_single_run_all_alive() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(AlwaysAliveWorkerManager);
        let reconciler = make_reconciler(wm, event_store.clone());

        let instance_id = Uuid::new_v4();
        let runs = vec![make_active_run_with_instance("running", instance_id)];

        let result = reconciler.reconcile_runs(&runs, instance_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);

        let events = event_store.emitted_events().await;
        assert!(events.is_empty());
    }
}
