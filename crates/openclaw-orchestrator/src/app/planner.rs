//! PlannerService — orchestrates plan generation and approval.
//!
//! Responsibilities:
//! - Emit plan lifecycle events (PlanRequested, PlanGenerated, PlanGenerationFailed, PlanApproved)
//! - In-flight guard: prevents duplicate plan generation for the same cycle
//! - Idempotency keys: `plan-requested-{cycle_id}`, `plan-generated-{cycle_id}`, etc.
//! - Delegates to the `Planner` domain port for actual plan generation

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::domain::events::EventEnvelope;
use crate::domain::planner::{PlanError, PlanProposal, Planner, PlanningContext};
use crate::domain::ports::EventStore;
use crate::infra::errors::InfraError;

/// Orchestrates the plan lifecycle: request, generate, approve.
pub struct PlannerService {
    planner: Arc<dyn Planner>,
    event_store: Arc<dyn EventStore>,
    pool: PgPool,
}

impl PlannerService {
    pub fn new(
        planner: Arc<dyn Planner>,
        event_store: Arc<dyn EventStore>,
        pool: PgPool,
    ) -> Self {
        Self {
            planner,
            event_store,
            pool,
        }
    }

    /// Request plan generation for a cycle.
    ///
    /// Emits `PlanRequested` event. The caller should then invoke `generate_plan`
    /// (possibly in a background task) to actually produce the plan.
    pub async fn request_plan(
        &self,
        instance_id: Uuid,
        cycle_id: Uuid,
        context: &PlanningContext,
    ) -> Result<(), AppError> {
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "PlanRequested".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "cycle_id": cycle_id,
                "objective": context.objective,
                "context_hash": context.context_hash,
                "actor": { "kind": "System" },
            }),
            idempotency_key: Some(format!("plan-requested-{cycle_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!("emit PlanRequested: {e}")))
        })?;

        tracing::info!(
            cycle_id = %cycle_id,
            instance_id = %instance_id,
            "plan requested"
        );

        Ok(())
    }

    /// Generate a plan for a cycle by calling the Planner domain port.
    ///
    /// Checks the in-flight guard first, then delegates to the planner.
    /// On success, emits `PlanGenerated`; on failure, emits `PlanGenerationFailed`.
    pub async fn generate_plan(
        &self,
        instance_id: Uuid,
        cycle_id: Uuid,
        context: &PlanningContext,
        attempt: u32,
    ) -> Result<PlanProposal, AppError> {
        // In-flight guard: check no plan generation is already in progress
        self.check_in_flight_generation(cycle_id).await?;

        self.generate_plan_core(instance_id, cycle_id, context, attempt)
            .await
    }

    /// Core plan generation logic, separated from the in-flight guard for testability.
    ///
    /// Calls the Planner trait and emits the appropriate event.
    pub(crate) async fn generate_plan_core(
        &self,
        instance_id: Uuid,
        cycle_id: Uuid,
        context: &PlanningContext,
        attempt: u32,
    ) -> Result<PlanProposal, AppError> {
        match self.planner.generate_plan(context).await {
            Ok(proposal) => {
                let plan_json = serde_json::to_value(&proposal)?;
                let task_count = proposal.tasks.len() as u32;
                let estimated_cost = proposal.estimated_cost;

                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id,
                    seq: 0,
                    event_type: "PlanGenerated".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "cycle_id": cycle_id,
                        "plan": plan_json,
                        "task_count": task_count,
                        "estimated_cost": estimated_cost,
                        "summary": proposal.summary,
                        "actor": { "kind": "Planner" },
                    }),
                    idempotency_key: Some(format!("plan-generated-{cycle_id}-{attempt}")),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!("emit PlanGenerated: {e}")))
                })?;

                tracing::info!(
                    cycle_id = %cycle_id,
                    task_count = task_count,
                    attempt = attempt,
                    "plan generated"
                );

                Ok(proposal)
            }
            Err(plan_err) => {
                let category = match &plan_err {
                    PlanError::NotFound { .. } => "NotFound",
                    PlanError::GenerationFailed(_) => "GenerationFailed",
                    PlanError::ContextError(_) => "ContextError",
                    PlanError::Timeout { .. } => "Timeout",
                };

                let now = Utc::now();
                let envelope = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    instance_id,
                    seq: 0,
                    event_type: "PlanGenerationFailed".to_string(),
                    event_version: 1,
                    payload: serde_json::json!({
                        "cycle_id": cycle_id,
                        "category": category,
                        "reason": plan_err.to_string(),
                        "actor": { "kind": "Planner" },
                    }),
                    idempotency_key: Some(format!("plan-generation-failed-{cycle_id}-{attempt}")),
                    correlation_id: None,
                    causation_id: None,
                    occurred_at: now,
                    recorded_at: now,
                };

                self.event_store.emit(envelope).await.map_err(|e| {
                    AppError::Infra(InfraError::Database(format!(
                        "emit PlanGenerationFailed: {e}"
                    )))
                })?;

                tracing::error!(
                    cycle_id = %cycle_id,
                    category = category,
                    reason = %plan_err,
                    attempt = attempt,
                    "plan generation failed"
                );

                Err(AppError::Domain(
                    crate::domain::errors::DomainError::Precondition(plan_err.to_string()),
                ))
            }
        }
    }

    /// Approve a plan, transitioning the cycle from PlanReady to Approved.
    ///
    /// Emits `PlanApproved` event. The co-emission of `PlanBudgetReserved`
    /// and `TaskScheduled` events per task is handled by the caller or a
    /// downstream handler.
    pub async fn approve_plan(
        &self,
        instance_id: Uuid,
        cycle_id: Uuid,
        approved_by: &str,
    ) -> Result<(), AppError> {
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "PlanApproved".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "cycle_id": cycle_id,
                "approved_by": approved_by,
                "actor": { "kind": "Human", "actor_id": approved_by },
            }),
            idempotency_key: Some(format!("plan-approved-{cycle_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!("emit PlanApproved: {e}")))
        })?;

        tracing::info!(
            cycle_id = %cycle_id,
            approved_by = approved_by,
            "plan approved"
        );

        Ok(())
    }

    /// In-flight guard: check there is no plan generation already in progress.
    ///
    /// A plan generation is "in-flight" if there is a `PlanRequested` event for
    /// this cycle with no corresponding `PlanGenerated` or `PlanGenerationFailed` event.
    async fn check_in_flight_generation(&self, cycle_id: Uuid) -> Result<(), AppError> {
        let in_flight = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT COUNT(*)
            FROM orch_events requested
            WHERE requested.event_type = 'PlanRequested'
              AND requested.payload->>'cycle_id' = $1::text
              AND NOT EXISTS (
                  SELECT 1 FROM orch_events terminal
                  WHERE terminal.event_type IN ('PlanGenerated', 'PlanGenerationFailed')
                    AND terminal.payload->>'cycle_id' = $1::text
              )
            "#,
        )
        .bind(cycle_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "check in-flight plan generation: {e}"
            )))
        })?;

        if in_flight > 0 {
            return Err(AppError::ConcurrencyConflict(format!(
                "plan generation already in-flight for cycle {cycle_id}"
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::errors::DomainError;
    use crate::domain::planner::{
        ArtifactRef, CommitSummary, CycleSummary, FileContext, PlanConstraints, PlanMetadata,
        PlanProposal, PlanningContext, RepoContext, TaskProposal, TaskScope,
    };

    // ── Mock Planner implementations ──

    /// Returns a fixed plan proposal.
    struct SuccessPlanner {
        proposal: PlanProposal,
    }

    impl SuccessPlanner {
        fn new() -> Self {
            Self {
                proposal: make_plan_proposal(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Planner for SuccessPlanner {
        async fn generate_plan(
            &self,
            _context: &PlanningContext,
        ) -> Result<PlanProposal, PlanError> {
            Ok(self.proposal.clone())
        }
    }

    /// Always fails with a GenerationFailed error.
    struct FailingPlanner {
        reason: String,
    }

    impl FailingPlanner {
        fn new(reason: &str) -> Self {
            Self {
                reason: reason.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Planner for FailingPlanner {
        async fn generate_plan(
            &self,
            _context: &PlanningContext,
        ) -> Result<PlanProposal, PlanError> {
            Err(PlanError::GenerationFailed(self.reason.clone()))
        }
    }

    /// Always times out.
    struct TimeoutPlanner;

    #[async_trait::async_trait]
    impl Planner for TimeoutPlanner {
        async fn generate_plan(
            &self,
            _context: &PlanningContext,
        ) -> Result<PlanProposal, PlanError> {
            Err(PlanError::Timeout { elapsed_secs: 300 })
        }
    }

    // ── RecordingEventStore ──

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

    // ── Test helpers ──

    fn dummy_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://dummy:dummy@localhost:5432/dummy")
            .expect("connect_lazy should not fail")
    }

    fn make_planning_context() -> PlanningContext {
        PlanningContext {
            cycle_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            objective: "Implement user authentication".to_string(),
            repo_context: RepoContext {
                file_tree_summary: "src/\n  main.rs\n  lib.rs".to_string(),
                recent_commits: vec![CommitSummary {
                    sha: "abc123".to_string(),
                    message: "initial commit".to_string(),
                    author: "alice".to_string(),
                }],
                relevant_files: vec![FileContext {
                    path: "src/main.rs".to_string(),
                    content_preview: "fn main() { ... }".to_string(),
                }],
            },
            constraints: PlanConstraints {
                max_tasks: 5,
                max_concurrent: 2,
                budget_remaining: 100_00,
                forbidden_paths: vec![],
            },
            previous_cycle_summary: Some(CycleSummary {
                cycle_id: Uuid::new_v4(),
                outcome: "completed".to_string(),
                summary: "Set up project scaffolding".to_string(),
            }),
            context_hash: "sha256-deadbeef".to_string(),
        }
    }

    fn make_task_proposal(key: &str) -> TaskProposal {
        TaskProposal {
            task_key: key.to_string(),
            title: format!("Task {key}"),
            description: format!("Implement {key}"),
            acceptance_criteria: vec!["tests pass".to_string(), "no warnings".to_string()],
            dependencies: vec![],
            estimated_tokens: Some(25_000),
            scope: Some(TaskScope {
                target_paths: vec!["src/".to_string()],
                read_only_paths: vec![],
            }),
        }
    }

    fn make_plan_proposal() -> PlanProposal {
        PlanProposal {
            tasks: vec![
                make_task_proposal("task-1"),
                {
                    let mut t = make_task_proposal("task-2");
                    t.dependencies = vec!["task-1".to_string()];
                    t
                },
            ],
            summary: "Two-phase implementation plan".to_string(),
            reasoning_ref: Some(ArtifactRef {
                kind: "reasoning_trace".to_string(),
                hash: "sha256-trace123".to_string(),
            }),
            estimated_cost: 5000,
            metadata: PlanMetadata {
                model_id: "claude-opus-4-20250514".to_string(),
                prompt_hash: "sha256-prompt456".to_string(),
                context_hash: "sha256-ctx789".to_string(),
                temperature: 0.7,
                generated_at: chrono::Utc::now(),
            },
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Tests
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn planner_service_is_constructible() {
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _svc = PlannerService::new(planner, es, dummy_pool());
    }

    // ── request_plan ──

    #[tokio::test]
    async fn request_plan_emits_plan_requested() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        let result = svc.request_plan(instance_id, cycle_id, &context).await;
        assert!(result.is_ok(), "request_plan should succeed: {result:?}");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PlanRequested");
        assert_eq!(events[0].instance_id, instance_id);
    }

    #[tokio::test]
    async fn request_plan_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        svc.request_plan(Uuid::new_v4(), cycle_id, &context)
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(*key, format!("plan-requested-{cycle_id}"));
    }

    #[tokio::test]
    async fn request_plan_payload_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        svc.request_plan(Uuid::new_v4(), cycle_id, &context)
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let payload = &events[0].payload;
        assert_eq!(payload["cycle_id"], cycle_id.to_string());
        assert_eq!(payload["objective"], "Implement user authentication");
        assert_eq!(payload["context_hash"], "sha256-deadbeef");
        assert_eq!(payload["actor"]["kind"], "System");
    }

    // ── generate_plan_core (success — tests event emission without SQL guard) ──

    #[tokio::test]
    async fn generate_plan_core_emits_plan_generated() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        let result = svc.generate_plan_core(instance_id, cycle_id, &context, 1).await;
        assert!(result.is_ok(), "generate_plan_core should succeed: {result:?}");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PlanGenerated");
        assert_eq!(events[0].instance_id, instance_id);
    }

    #[tokio::test]
    async fn generate_plan_core_returns_proposal_with_correct_task_count() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let context = make_planning_context();
        let proposal = svc
            .generate_plan_core(Uuid::new_v4(), Uuid::new_v4(), &context, 1)
            .await
            .unwrap();
        assert_eq!(proposal.tasks.len(), 2);
        assert_eq!(proposal.tasks[0].task_key, "task-1");
        assert_eq!(proposal.tasks[1].task_key, "task-2");
    }

    #[tokio::test]
    async fn generate_plan_core_proposal_has_dependencies() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let context = make_planning_context();
        let proposal = svc
            .generate_plan_core(Uuid::new_v4(), Uuid::new_v4(), &context, 1)
            .await
            .unwrap();
        assert!(proposal.tasks[0].dependencies.is_empty());
        assert_eq!(proposal.tasks[1].dependencies, vec!["task-1".to_string()]);
    }

    #[tokio::test]
    async fn generate_plan_core_event_payload_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        svc.generate_plan_core(Uuid::new_v4(), cycle_id, &context, 1)
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let payload = &events[0].payload;
        assert_eq!(payload["cycle_id"], cycle_id.to_string());
        assert_eq!(payload["task_count"], 2);
        assert_eq!(payload["summary"], "Two-phase implementation plan");
        assert_eq!(payload["actor"]["kind"], "Planner");
        assert!(payload.get("plan").is_some(), "should include plan JSON");
    }

    #[tokio::test]
    async fn generate_plan_core_idempotency_key_includes_attempt() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        svc.generate_plan_core(Uuid::new_v4(), cycle_id, &context, 3)
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let key = events[0].idempotency_key.as_ref().expect("should have key");
        assert_eq!(*key, format!("plan-generated-{cycle_id}-3"));
    }

    // ── generate_plan_core (failure — tests error event emission) ──

    #[tokio::test]
    async fn generate_plan_core_failure_emits_plan_generation_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> =
            Arc::new(FailingPlanner::new("model rate limited"));
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        let result = svc.generate_plan_core(instance_id, cycle_id, &context, 1).await;
        assert!(result.is_err());

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PlanGenerationFailed");
        assert_eq!(events[0].instance_id, instance_id);
        assert_eq!(events[0].payload["category"], "GenerationFailed");
        assert!(events[0].payload["reason"].as_str().unwrap().contains("model rate limited"));
    }

    #[tokio::test]
    async fn generate_plan_core_timeout_emits_plan_generation_failed() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(TimeoutPlanner);
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        let result = svc
            .generate_plan_core(Uuid::new_v4(), cycle_id, &context, 2)
            .await;
        assert!(result.is_err());

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].event_type, "PlanGenerationFailed");
        assert_eq!(events[0].payload["category"], "Timeout");

        let key = events[0].idempotency_key.as_ref().expect("should have key");
        assert_eq!(*key, format!("plan-generation-failed-{cycle_id}-2"));
    }

    // ── approve_plan ──

    #[tokio::test]
    async fn approve_plan_emits_plan_approved() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();

        let result = svc
            .approve_plan(instance_id, cycle_id, "alice")
            .await;
        assert!(result.is_ok(), "approve_plan should succeed: {result:?}");

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "PlanApproved");
        assert_eq!(events[0].instance_id, instance_id);
    }

    #[tokio::test]
    async fn approve_plan_idempotency_key() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        svc.approve_plan(Uuid::new_v4(), cycle_id, "bob")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let key = events[0]
            .idempotency_key
            .as_ref()
            .expect("should have key");
        assert_eq!(*key, format!("plan-approved-{cycle_id}"));
    }

    #[tokio::test]
    async fn approve_plan_payload_fields() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let cycle_id = Uuid::new_v4();
        svc.approve_plan(Uuid::new_v4(), cycle_id, "charlie")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        let payload = &events[0].payload;
        assert_eq!(payload["cycle_id"], cycle_id.to_string());
        assert_eq!(payload["approved_by"], "charlie");
        assert_eq!(payload["actor"]["kind"], "Human");
        assert_eq!(payload["actor"]["actor_id"], "charlie");
    }

    // ── event_version ──

    #[tokio::test]
    async fn request_plan_event_version_is_one() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        svc.request_plan(Uuid::new_v4(), Uuid::new_v4(), &make_planning_context())
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].event_version, 1);
    }

    #[tokio::test]
    async fn approve_plan_event_version_is_one() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        svc.approve_plan(Uuid::new_v4(), Uuid::new_v4(), "alice")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].event_version, 1);
    }

    // ── seq is zero (assigned by store) ──

    #[tokio::test]
    async fn request_plan_seq_is_zero() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        svc.request_plan(Uuid::new_v4(), Uuid::new_v4(), &make_planning_context())
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].seq, 0);
    }

    #[tokio::test]
    async fn approve_plan_seq_is_zero() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        svc.approve_plan(Uuid::new_v4(), Uuid::new_v4(), "alice")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events[0].seq, 0);
    }

    // ── Full lifecycle (request + generate + approve) ──

    #[tokio::test]
    async fn full_lifecycle_request_generate_approve() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        // Step 1: request plan
        svc.request_plan(instance_id, cycle_id, &context)
            .await
            .unwrap();

        // Step 2: generate plan (using core to bypass SQL guard)
        svc.generate_plan_core(instance_id, cycle_id, &context, 1)
            .await
            .unwrap();

        // Step 3: approve plan
        svc.approve_plan(instance_id, cycle_id, "alice")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "PlanRequested");
        assert_eq!(events[1].event_type, "PlanGenerated");
        assert_eq!(events[2].event_type, "PlanApproved");

        // All events have same instance_id
        for event in &events {
            assert_eq!(event.instance_id, instance_id);
            assert_eq!(event.payload["cycle_id"], cycle_id.to_string());
        }
    }

    // ── Plan proposal serialization within events ──

    #[tokio::test]
    async fn plan_proposal_serializes_to_json_value() {
        let proposal = make_plan_proposal();
        let value = serde_json::to_value(&proposal).unwrap();
        assert!(value.get("tasks").unwrap().is_array());
        let tasks = value["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["task_key"], "task-1");
        assert_eq!(tasks[1]["task_key"], "task-2");
        assert_eq!(value["summary"], "Two-phase implementation plan");
    }

    // ── Error paths via service ──

    #[tokio::test]
    async fn generate_plan_core_failure_returns_domain_error() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(FailingPlanner::new("LLM overloaded"));
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let result = svc
            .generate_plan_core(Uuid::new_v4(), Uuid::new_v4(), &make_planning_context(), 1)
            .await;
        assert!(matches!(result, Err(AppError::Domain(_))));
    }

    #[tokio::test]
    async fn generate_plan_core_timeout_returns_domain_error() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(TimeoutPlanner);
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let result = svc
            .generate_plan_core(Uuid::new_v4(), Uuid::new_v4(), &make_planning_context(), 1)
            .await;
        assert!(matches!(result, Err(AppError::Domain(_))));
    }

    // ── Multiple approvals ──

    #[tokio::test]
    async fn approve_plan_can_be_called_twice_different_cycles() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle1 = Uuid::new_v4();
        let cycle2 = Uuid::new_v4();

        svc.approve_plan(instance_id, cycle1, "alice").await.unwrap();
        svc.approve_plan(instance_id, cycle2, "bob").await.unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].payload["cycle_id"], cycle1.to_string());
        assert_eq!(events[1].payload["cycle_id"], cycle2.to_string());
        assert_eq!(events[0].payload["approved_by"], "alice");
        assert_eq!(events[1].payload["approved_by"], "bob");
    }

    // ── Correlation / causation ids ──

    #[tokio::test]
    async fn events_have_no_correlation_or_causation() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        svc.request_plan(Uuid::new_v4(), Uuid::new_v4(), &make_planning_context())
            .await
            .unwrap();
        svc.approve_plan(Uuid::new_v4(), Uuid::new_v4(), "alice")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        for event in &events {
            assert!(event.correlation_id.is_none());
            assert!(event.causation_id.is_none());
        }
    }

    // ── Unique event IDs ──

    #[tokio::test]
    async fn each_event_has_unique_event_id() {
        let event_store = Arc::new(RecordingEventStore::new());
        let planner: Arc<dyn Planner> = Arc::new(SuccessPlanner::new());
        let svc = PlannerService::new(planner, event_store.clone(), dummy_pool());

        let instance_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let context = make_planning_context();

        svc.request_plan(instance_id, cycle_id, &context)
            .await
            .unwrap();
        svc.approve_plan(instance_id, cycle_id, "alice")
            .await
            .unwrap();

        let events = event_store.emitted_events().await;
        assert_ne!(events[0].event_id, events[1].event_id);
    }
}
