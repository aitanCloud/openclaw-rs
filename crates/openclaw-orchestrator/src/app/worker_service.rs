use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::app::scheduler::ClaimResult;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::{EventStore, WorkerManager};
use crate::domain::worker::WorkerSpawnConfig;
use crate::infra::errors::InfraError;

/// Default timeout for a worker run (1 hour).
const DEFAULT_TIMEOUT_SECS: u64 = 3600;

/// Default heartbeat interval (30 seconds).
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Coordinates worker lifecycle after scheduler claims.
///
/// Responsibilities:
/// - After scheduler claims a task, spawn a worker process
/// - On successful spawn, emit RunStarted (proof-of-life)
/// - On spawn failure, emit RunFailed with failure_category = "SpawnFailure"
///
/// Future responsibilities (not yet implemented):
/// - Heartbeat monitoring loop (background task)
/// - Log tailing (background task, bounded output)
/// - Exit detection -> emit RunCompleted/RunFailed/RunTimedOut
pub struct WorkerService {
    worker_manager: Arc<dyn WorkerManager>,
    event_store: Arc<dyn EventStore>,
    data_dir: PathBuf,
}

impl WorkerService {
    pub fn new(
        worker_manager: Arc<dyn WorkerManager>,
        event_store: Arc<dyn EventStore>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            worker_manager,
            event_store,
            data_dir,
        }
    }

    /// Spawn a worker for a claimed run.
    ///
    /// Called as a post-commit side effect after RunClaimed is committed.
    /// Emits RunStarted on successful spawn (proof-of-life),
    /// or RunFailed { failure_category: "SpawnFailure" } on failure.
    ///
    /// # Arguments
    /// * `claim` - The claim result from the scheduler tick
    /// * `prompt` - The task prompt/spec to send to the coding agent
    /// * `worktree_path` - Path to the git worktree for this run
    pub async fn spawn_worker(
        &self,
        claim: &ClaimResult,
        prompt: &str,
        worktree_path: &str,
    ) -> Result<(), AppError> {
        // Read worker_session_id from the RunClaimed event's payload.
        // The scheduler generates it at claim time and stores it in the run row.
        // For this method, we generate a fresh session_id since the ClaimResult
        // does not carry it. In the full architecture, the session_id would be
        // read from the orch_runs row or the RunClaimed event payload.
        //
        // TODO: Thread worker_session_id through ClaimResult once scheduler
        // is updated to expose it.
        let session_id = Uuid::new_v4();

        // Build environment variables
        let mut environment = HashMap::new();
        environment.insert(
            "OPENCLAW_SESSION_ID".to_string(),
            session_id.to_string(),
        );
        environment.insert(
            "OPENCLAW_RUN_ID".to_string(),
            claim.run_id.to_string(),
        );
        environment.insert(
            "OPENCLAW_INSTANCE_ID".to_string(),
            claim.instance_id.to_string(),
        );

        let config = WorkerSpawnConfig {
            run_id: claim.run_id,
            task_id: claim.task_id,
            instance_id: claim.instance_id,
            session_id,
            prompt: prompt.to_string(),
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            heartbeat_interval: Duration::from_secs(DEFAULT_HEARTBEAT_INTERVAL_SECS),
            worktree_path: worktree_path.to_string(),
            environment,
        };

        match self.worker_manager.spawn(config).await {
            Ok(handle) => {
                tracing::info!(
                    run_id = %claim.run_id,
                    session_id = %handle.session_id,
                    "worker spawned successfully, emitting RunStarted"
                );

                // Emit RunStarted event (proof-of-life).
                // In the full architecture, this should only be emitted after
                // the first heartbeat is detected. For Phase 2 v1, we simplify
                // by emitting it immediately on successful spawn.
                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id: claim.instance_id,
                    seq: 0, // Will be assigned by the event store
                    event_type: "RunStarted".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "run_id": claim.run_id,
                        "task_id": claim.task_id,
                        "worker_session_id": handle.session_id,
                        "log_stdout": handle.log_stdout,
                        "log_stderr": handle.log_stderr,
                    }),
                    idempotency_key: Some(format!("run-started-{}", claim.run_id)),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!(
                        "emit RunStarted: {e}"
                    )))
                })?;

                Ok(())
            }
            Err(e) => {
                tracing::error!(
                    run_id = %claim.run_id,
                    error = %e,
                    "worker spawn failed, emitting RunFailed"
                );

                // Emit RunFailed event with failure_category = "SpawnFailure"
                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id: claim.instance_id,
                    seq: 0, // Will be assigned by the event store
                    event_type: "RunFailed".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "run_id": claim.run_id,
                        "task_id": claim.task_id,
                        "failure_category": "SpawnFailure",
                        "error_message": e.to_string(),
                    }),
                    idempotency_key: Some(format!("run-failed-{}", claim.run_id)),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                // Best-effort emit -- log if this also fails
                if let Err(emit_err) = self.event_store.emit(envelope).await {
                    tracing::error!(
                        run_id = %claim.run_id,
                        error = %emit_err,
                        "failed to emit RunFailed event"
                    );
                }

                Err(AppError::Infra(InfraError::Io(format!(
                    "worker spawn failed for run {}: {e}",
                    claim.run_id
                ))))
            }
        }
    }

    /// Get the data directory for this worker service.
    pub fn data_dir(&self) -> &PathBuf {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;
    use crate::domain::worker::{WorkerError, WorkerHandle};

    /// Mock WorkerManager that always succeeds.
    struct SuccessWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for SuccessWorkerManager {
        async fn spawn(
            &self,
            config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            Ok(WorkerHandle {
                session_id: config.session_id,
                run_id: config.run_id,
                log_stdout: format!("/tmp/{}/{}/stdout.log", config.instance_id, config.run_id),
                log_stderr: format!("/tmp/{}/{}/stderr.log", config.instance_id, config.run_id),
            })
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

    /// Mock WorkerManager that always fails spawn.
    struct FailingWorkerManager;

    #[async_trait::async_trait]
    impl WorkerManager for FailingWorkerManager {
        async fn spawn(
            &self,
            _config: WorkerSpawnConfig,
        ) -> Result<WorkerHandle, WorkerError> {
            Err(WorkerError::SpawnFailed("binary not found".to_string()))
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

    /// Mock EventStore that records emitted events.
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

    fn make_claim() -> ClaimResult {
        ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 1,
        }
    }

    #[test]
    fn worker_service_is_constructible() {
        let wm: Arc<dyn WorkerManager> = Arc::new(SuccessWorkerManager);
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _svc = WorkerService::new(wm, es, PathBuf::from("/tmp"));
    }

    #[test]
    fn worker_service_data_dir() {
        let wm: Arc<dyn WorkerManager> = Arc::new(SuccessWorkerManager);
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let svc = WorkerService::new(wm, es, PathBuf::from("/data/openclaw"));
        assert_eq!(svc.data_dir(), &PathBuf::from("/data/openclaw"));
    }

    #[test]
    fn default_timeout_is_one_hour() {
        assert_eq!(DEFAULT_TIMEOUT_SECS, 3600);
    }

    #[test]
    fn default_heartbeat_interval_is_thirty_seconds() {
        assert_eq!(DEFAULT_HEARTBEAT_INTERVAL_SECS, 30);
    }

    #[tokio::test]
    async fn spawn_worker_success_emits_run_started() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(SuccessWorkerManager);
        let svc = WorkerService::new(wm, event_store.clone(), PathBuf::from("/tmp"));

        let claim = make_claim();
        let result = svc.spawn_worker(&claim, "implement feature", "/tmp/worktree").await;
        assert!(result.is_ok(), "spawn should succeed: {result:?}");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "RunStarted");
        assert_eq!(events[0].instance_id, claim.instance_id);

        let payload = &events[0].payload;
        assert_eq!(payload["run_id"], claim.run_id.to_string());
        assert_eq!(payload["task_id"], claim.task_id.to_string());
        assert!(payload["worker_session_id"].is_string());
        assert!(payload["log_stdout"].is_string());
        assert!(payload["log_stderr"].is_string());
    }

    #[tokio::test]
    async fn spawn_worker_failure_emits_run_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(FailingWorkerManager);
        let svc = WorkerService::new(wm, event_store.clone(), PathBuf::from("/tmp"));

        let claim = make_claim();
        let result = svc.spawn_worker(&claim, "implement feature", "/tmp/worktree").await;
        assert!(result.is_err(), "spawn should fail");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "RunFailed");
        assert_eq!(events[0].instance_id, claim.instance_id);

        let payload = &events[0].payload;
        assert_eq!(payload["run_id"], claim.run_id.to_string());
        assert_eq!(payload["task_id"], claim.task_id.to_string());
        assert_eq!(payload["failure_category"], "SpawnFailure");
        assert!(payload["error_message"].is_string());
        assert!(
            payload["error_message"]
                .as_str()
                .unwrap()
                .contains("binary not found")
        );
    }

    #[tokio::test]
    async fn spawn_worker_sets_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(SuccessWorkerManager);
        let svc = WorkerService::new(wm, event_store.clone(), PathBuf::from("/tmp"));

        let claim = make_claim();
        svc.spawn_worker(&claim, "test", "/tmp")
            .await
            .expect("spawn ok");

        let events = event_store.emitted_events().await;
        let key = events[0].idempotency_key.as_ref().expect("has key");
        assert!(key.starts_with("run-started-"));
        assert!(key.contains(&claim.run_id.to_string()));
    }

    #[tokio::test]
    async fn spawn_worker_failure_has_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(FailingWorkerManager);
        let svc = WorkerService::new(wm, event_store.clone(), PathBuf::from("/tmp"));

        let claim = make_claim();
        let _ = svc.spawn_worker(&claim, "test", "/tmp").await;

        let events = event_store.emitted_events().await;
        let key = events[0].idempotency_key.as_ref().expect("has key");
        assert!(key.starts_with("run-failed-"));
        assert!(key.contains(&claim.run_id.to_string()));
    }

    #[tokio::test]
    async fn spawn_worker_event_version_is_one() {
        let event_store = Arc::new(RecordingEventStore::new());
        let wm: Arc<dyn WorkerManager> = Arc::new(SuccessWorkerManager);
        let svc = WorkerService::new(wm, event_store.clone(), PathBuf::from("/tmp"));

        let claim = make_claim();
        svc.spawn_worker(&claim, "test", "/tmp")
            .await
            .expect("ok");

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].event_version, 1);
    }
}
