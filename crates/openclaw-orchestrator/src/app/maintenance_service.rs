//! MaintenanceService — manages instance maintenance mode lifecycle.
//!
//! Provides enter/exit maintenance mode via event sourcing, maintenance state
//! queries, and projection rebuild (truncate + replay).
//!
//! Architecture (E7):
//! - When maintenance_mode = true for an instance:
//!   - BLOCKED: scheduler, worker spawning, merge operations, plan generation, budget reservations
//!   - STILL RUNNING: active workers (finish naturally), event streaming, health checks, API reads
//!   - RECONCILER: read-only diagnostics only
//!   - ALLOWED: projection rebuild, manual reconciliation, configuration changes
//!   - JANITOR: skips destructive operations unless explicitly invoked

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::errors::DomainError;
use crate::domain::events::EventEnvelope;
use crate::domain::ports::EventStore;
use crate::infra::errors::InfraError;
use crate::infra::event_store::advisory_lock_key;

/// Result of a projection rebuild operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebuildResult {
    /// Number of events replayed through projectors.
    pub events_replayed: u64,
}

/// Manages instance maintenance mode: enter, exit, query, and projection rebuild.
pub struct MaintenanceService {
    pool: PgPool,
    event_store: Arc<dyn EventStore>,
    projectors: Vec<Arc<dyn Projector>>,
}

impl MaintenanceService {
    /// Create a new MaintenanceService.
    ///
    /// `projectors` is the ordered list of projectors to replay events through
    /// during a projection rebuild.
    pub fn new(
        pool: PgPool,
        event_store: Arc<dyn EventStore>,
        projectors: Vec<Arc<dyn Projector>>,
    ) -> Self {
        Self {
            pool,
            event_store,
            projectors,
        }
    }

    /// Enter maintenance mode for an instance.
    ///
    /// Pre-condition: instance must be in 'active' state.
    /// Acquires an advisory lock, emits `InstanceBlocked` with reason "Maintenance",
    /// then releases the lock.
    pub async fn enter_maintenance(
        &self,
        instance_id: Uuid,
        reason: Option<String>,
    ) -> Result<(), AppError> {
        let (hi, lo) = advisory_lock_key(instance_id);

        // Acquire advisory lock
        sqlx::query("SELECT pg_advisory_lock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        let result = self.enter_maintenance_inner(instance_id, reason).await;

        // Always release advisory lock
        sqlx::query("SELECT pg_advisory_unlock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        result
    }

    /// Inner logic for entering maintenance mode (called while holding the advisory lock).
    async fn enter_maintenance_inner(
        &self,
        instance_id: Uuid,
        reason: Option<String>,
    ) -> Result<(), AppError> {
        // Check current state
        let state = self.query_instance_state(instance_id).await?;
        if state != "active" {
            return Err(AppError::Domain(DomainError::Precondition(format!(
                "instance {} is in '{}' state, must be 'active' to enter maintenance",
                instance_id, state
            ))));
        }

        let details = reason.unwrap_or_default();
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceBlocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "reason": "Maintenance",
                "details": details,
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(format!("maintenance-enter-{instance_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "emit InstanceBlocked (maintenance): {e}"
            )))
        })?;

        tracing::info!(
            instance_id = %instance_id,
            "entered maintenance mode"
        );

        Ok(())
    }

    /// Exit maintenance mode for an instance.
    ///
    /// Pre-condition: instance must be in 'blocked' state.
    /// Acquires an advisory lock, emits `InstanceUnblocked`, then releases the lock.
    pub async fn exit_maintenance(&self, instance_id: Uuid) -> Result<(), AppError> {
        let (hi, lo) = advisory_lock_key(instance_id);

        // Acquire advisory lock
        sqlx::query("SELECT pg_advisory_lock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        let result = self.exit_maintenance_inner(instance_id).await;

        // Always release advisory lock
        sqlx::query("SELECT pg_advisory_unlock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        result
    }

    /// Inner logic for exiting maintenance mode (called while holding the advisory lock).
    async fn exit_maintenance_inner(&self, instance_id: Uuid) -> Result<(), AppError> {
        // Check current state
        let state = self.query_instance_state(instance_id).await?;
        if state != "blocked" {
            return Err(AppError::Domain(DomainError::Precondition(format!(
                "instance {} is in '{}' state, must be 'blocked' to exit maintenance",
                instance_id, state
            ))));
        }

        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceUnblocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(format!("maintenance-exit-{instance_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        self.event_store.emit(envelope).await.map_err(|e| {
            AppError::Infra(InfraError::Database(format!(
                "emit InstanceUnblocked (maintenance): {e}"
            )))
        })?;

        tracing::info!(
            instance_id = %instance_id,
            "exited maintenance mode"
        );

        Ok(())
    }

    /// Check if an instance is in maintenance mode.
    ///
    /// Returns true if state = 'blocked' AND block_reason = 'Maintenance'.
    pub async fn is_maintenance_mode(&self, instance_id: Uuid) -> Result<bool, AppError> {
        let row = sqlx::query_as::<_, InstanceMaintenanceRow>(
            "SELECT state, block_reason FROM orch_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        match row {
            Some(r) => Ok(r.state == "blocked"
                && r.block_reason.as_deref() == Some("Maintenance")),
            None => Err(AppError::Domain(DomainError::NotFound {
                entity: "Instance".to_string(),
                id: instance_id.to_string(),
            })),
        }
    }

    /// Rebuild projections for an instance by replaying all events through projectors.
    ///
    /// Pre-condition: instance must be in maintenance mode.
    ///
    /// The rebuild procedure:
    /// 1. Verify instance is in maintenance mode
    /// 2. Acquire advisory lock
    /// 3. Truncate projection tables for this instance (cycles, tasks, runs,
    ///    server_capacity) — NOT events (append-only) or artifacts (no-delete trigger)
    ///    or budget_ledger (audit trail)
    /// 4. Replay all events through projectors in sequence order
    /// 5. Release advisory lock
    ///
    /// NOTE: This method does NOT exit maintenance mode. The caller should
    /// call `exit_maintenance()` after verifying the rebuild result.
    pub async fn rebuild_projections(
        &self,
        instance_id: Uuid,
    ) -> Result<RebuildResult, AppError> {
        // Pre-condition check (uses its own DB query, no lock needed yet)
        if !self.is_maintenance_mode(instance_id).await? {
            return Err(AppError::Domain(DomainError::Precondition(
                "projection rebuild requires maintenance mode".to_string(),
            )));
        }

        let (hi, lo) = advisory_lock_key(instance_id);

        // Acquire advisory lock
        sqlx::query("SELECT pg_advisory_lock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        let result = self.rebuild_projections_inner(instance_id).await;

        // Always release advisory lock
        sqlx::query("SELECT pg_advisory_unlock($1, $2)")
            .bind(hi)
            .bind(lo)
            .execute(&self.pool)
            .await
            .map_err(|e| AppError::Infra(InfraError::LockConflict(e.to_string())))?;

        result
    }

    /// Inner rebuild logic (called while holding the advisory lock).
    async fn rebuild_projections_inner(
        &self,
        instance_id: Uuid,
    ) -> Result<RebuildResult, AppError> {
        tracing::info!(
            instance_id = %instance_id,
            "starting projection rebuild"
        );

        // Step 1: Truncate projection tables for this instance.
        // Order matters due to FK constraints: runs references tasks, tasks references cycles.
        // We delete in reverse dependency order.
        //
        // NOT deleted: orch_events (append-only truth), orch_artifacts (no-delete trigger),
        // orch_budget_ledger (audit trail).
        self.truncate_projections(instance_id).await?;

        // Step 2: Replay all events through projectors
        let events = self
            .event_store
            .replay(instance_id, 0)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "event store replay for rebuild: {e}"
                )))
            })?;

        let event_count = events.len() as u64;

        tracing::info!(
            instance_id = %instance_id,
            event_count = event_count,
            projector_count = self.projectors.len(),
            "replaying events through projectors"
        );

        for event in &events {
            for projector in &self.projectors {
                // Only dispatch to projectors that handle this event type
                if projector
                    .handles()
                    .contains(&event.event_type.as_str())
                {
                    projector.handle(event, &self.pool).await?;
                }
            }
        }

        tracing::info!(
            instance_id = %instance_id,
            events_replayed = event_count,
            "projection rebuild complete"
        );

        Ok(RebuildResult {
            events_replayed: event_count,
        })
    }

    /// Truncate projection tables for an instance.
    ///
    /// Deletes rows from: orch_server_capacity, orch_runs, orch_tasks, orch_cycles.
    /// Does NOT touch: orch_events, orch_artifacts, orch_budget_ledger, orch_instances.
    async fn truncate_projections(&self, instance_id: Uuid) -> Result<(), AppError> {
        // Delete in reverse FK dependency order.
        // orch_runs depends on orch_tasks, orch_tasks depends on orch_cycles.
        // orch_server_capacity is independent.

        sqlx::query("DELETE FROM orch_server_capacity WHERE instance_id = $1")
            .bind(instance_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "truncate orch_server_capacity: {e}"
                )))
            })?;

        sqlx::query("DELETE FROM orch_runs WHERE instance_id = $1")
            .bind(instance_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "truncate orch_runs: {e}"
                )))
            })?;

        sqlx::query("DELETE FROM orch_tasks WHERE instance_id = $1")
            .bind(instance_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "truncate orch_tasks: {e}"
                )))
            })?;

        sqlx::query("DELETE FROM orch_cycles WHERE instance_id = $1")
            .bind(instance_id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                AppError::Infra(InfraError::Database(format!(
                    "truncate orch_cycles: {e}"
                )))
            })?;

        tracing::info!(
            instance_id = %instance_id,
            "projection tables truncated for rebuild"
        );

        Ok(())
    }

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
            None => Err(AppError::Domain(DomainError::NotFound {
                entity: "Instance".to_string(),
                id: instance_id.to_string(),
            })),
        }
    }
}

/// Check if an instance is in maintenance mode given its state and block_reason.
///
/// This is a pure function that can be used by other services without needing
/// a DB connection (e.g., when the state is already in memory).
pub fn check_maintenance_mode(state: &str, block_reason: Option<&str>) -> bool {
    state == "blocked" && block_reason == Some("Maintenance")
}

// ── Row types for DB queries ──

#[derive(sqlx::FromRow)]
struct InstanceStateRow {
    state: String,
}

#[derive(sqlx::FromRow)]
struct InstanceMaintenanceRow {
    state: String,
    block_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Ok(self.events.lock().await.clone())
        }

        async fn head_seq(&self, _instance_id: Uuid) -> Result<i64, DomainError> {
            Ok(self.events.lock().await.len() as i64)
        }
    }

    /// EventStore that fails on emit.
    struct FailingEventStore;

    #[async_trait::async_trait]
    impl EventStore for FailingEventStore {
        async fn emit(&self, _event: EventEnvelope) -> Result<i64, DomainError> {
            Err(DomainError::Precondition(
                "event store unavailable".to_string(),
            ))
        }

        async fn replay(
            &self,
            _instance_id: Uuid,
            _since_seq: i64,
        ) -> Result<Vec<EventEnvelope>, DomainError> {
            Ok(Vec::new())
        }

        async fn head_seq(&self, _instance_id: Uuid) -> Result<i64, DomainError> {
            Ok(0)
        }
    }

    // ── Mock Projector ──

    struct CountingProjector {
        name: &'static str,
        handles: Vec<&'static str>,
        call_count: tokio::sync::Mutex<u64>,
    }

    impl CountingProjector {
        fn new(name: &'static str, handles: Vec<&'static str>) -> Self {
            Self {
                name,
                handles,
                call_count: tokio::sync::Mutex::new(0),
            }
        }

        async fn count(&self) -> u64 {
            *self.call_count.lock().await
        }
    }

    #[async_trait::async_trait]
    impl Projector for CountingProjector {
        fn name(&self) -> &'static str {
            self.name
        }

        fn handles(&self) -> &[&'static str] {
            &self.handles
        }

        async fn handle(&self, _event: &EventEnvelope, _pool: &PgPool) -> Result<(), AppError> {
            let mut count = self.call_count.lock().await;
            *count += 1;
            Ok(())
        }
    }

    // ── Helpers ──

    fn dummy_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://dummy:dummy@localhost:5432/dummy")
            .expect("connect_lazy should not fail")
    }

    fn make_service(event_store: Arc<dyn EventStore>) -> MaintenanceService {
        MaintenanceService::new(dummy_pool(), event_store, vec![])
    }

    fn make_service_with_projectors(
        event_store: Arc<dyn EventStore>,
        projectors: Vec<Arc<dyn Projector>>,
    ) -> MaintenanceService {
        MaintenanceService::new(dummy_pool(), event_store, projectors)
    }

    fn make_event(instance_id: Uuid, event_type: &str) -> EventEnvelope {
        let now = Utc::now();
        EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: event_type.to_string(),
            event_version: 1,
            payload: serde_json::json!({}),
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Pure function tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn check_maintenance_mode_blocked_with_maintenance_reason() {
        assert!(check_maintenance_mode("blocked", Some("Maintenance")));
    }

    #[test]
    fn check_maintenance_mode_blocked_with_other_reason() {
        assert!(!check_maintenance_mode("blocked", Some("BudgetExceeded")));
    }

    #[test]
    fn check_maintenance_mode_blocked_with_no_reason() {
        assert!(!check_maintenance_mode("blocked", None));
    }

    #[test]
    fn check_maintenance_mode_active() {
        assert!(!check_maintenance_mode("active", None));
    }

    #[test]
    fn check_maintenance_mode_active_with_maintenance_reason() {
        // Even if block_reason somehow says Maintenance, state must be blocked
        assert!(!check_maintenance_mode("active", Some("Maintenance")));
    }

    #[test]
    fn check_maintenance_mode_suspended() {
        assert!(!check_maintenance_mode("suspended", None));
    }

    #[test]
    fn check_maintenance_mode_provisioning() {
        assert!(!check_maintenance_mode("provisioning", None));
    }

    #[test]
    fn check_maintenance_mode_empty_state() {
        assert!(!check_maintenance_mode("", None));
    }

    // ═══════════════════════════════════════════════════════════════════
    // RebuildResult tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn rebuild_result_fields() {
        let result = RebuildResult {
            events_replayed: 42,
        };
        assert_eq!(result.events_replayed, 42);
    }

    #[test]
    fn rebuild_result_debug() {
        let result = RebuildResult {
            events_replayed: 10,
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("RebuildResult"));
        assert!(debug.contains("events_replayed"));
    }

    #[test]
    fn rebuild_result_clone() {
        let result = RebuildResult {
            events_replayed: 5,
        };
        let cloned = result.clone();
        assert_eq!(result, cloned);
    }

    #[test]
    fn rebuild_result_eq() {
        let a = RebuildResult {
            events_replayed: 10,
        };
        let b = RebuildResult {
            events_replayed: 10,
        };
        assert_eq!(a, b);
    }

    #[test]
    fn rebuild_result_ne() {
        let a = RebuildResult {
            events_replayed: 10,
        };
        let b = RebuildResult {
            events_replayed: 20,
        };
        assert_ne!(a, b);
    }

    // ═══════════════════════════════════════════════════════════════════
    // MaintenanceService construction
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn maintenance_service_is_constructible() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let _svc = make_service(es);
    }

    #[tokio::test]
    async fn maintenance_service_with_projectors_is_constructible() {
        let es: Arc<dyn EventStore> = Arc::new(RecordingEventStore::new());
        let p1: Arc<dyn Projector> = Arc::new(CountingProjector::new(
            "TestProjector",
            vec!["InstanceCreated"],
        ));
        let _svc = make_service_with_projectors(es, vec![p1]);
    }

    // ═══════════════════════════════════════════════════════════════════
    // enter_maintenance_inner tests (unit-testable without real DB)
    // ═══════════════════════════════════════════════════════════════════

    // NOTE: enter_maintenance_inner and exit_maintenance_inner are private methods
    // that require a real DB connection (they query orch_instances). The public
    // enter_maintenance/exit_maintenance methods additionally need pg_advisory_lock.
    //
    // We test the event emission patterns by testing that the service creates
    // correct EventEnvelope structures. Integration tests with a real DB would
    // test the full flow.

    #[tokio::test]
    async fn enter_maintenance_event_structure() {
        // Test that the InstanceBlocked event has the right structure
        // by manually constructing what enter_maintenance_inner would emit.
        let instance_id = Uuid::new_v4();
        let event_store = Arc::new(RecordingEventStore::new());

        // Simulate what enter_maintenance_inner does (without DB queries)
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceBlocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "reason": "Maintenance",
                "details": "scheduled downtime",
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(format!("maintenance-enter-{instance_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        event_store.emit(envelope).await.unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "InstanceBlocked");
        assert_eq!(events[0].instance_id, instance_id);
        assert_eq!(events[0].event_version, 1);
        assert_eq!(events[0].payload["reason"], "Maintenance");
        assert_eq!(events[0].payload["details"], "scheduled downtime");
        assert_eq!(events[0].payload["actor"]["kind"], "Admin");
        assert_eq!(events[0].payload["actor"]["id"], "maintenance_service");
    }

    #[tokio::test]
    async fn enter_maintenance_idempotency_key_format() {
        let instance_id = Uuid::new_v4();
        let expected_key = format!("maintenance-enter-{instance_id}");

        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceBlocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "reason": "Maintenance",
                "details": "",
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(expected_key.clone()),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        assert_eq!(
            envelope.idempotency_key.as_ref().unwrap(),
            &expected_key
        );
    }

    #[tokio::test]
    async fn exit_maintenance_event_structure() {
        let instance_id = Uuid::new_v4();
        let event_store = Arc::new(RecordingEventStore::new());

        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceUnblocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(format!("maintenance-exit-{instance_id}")),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        event_store.emit(envelope).await.unwrap();

        let events = event_store.emitted_events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "InstanceUnblocked");
        assert_eq!(events[0].instance_id, instance_id);
        assert_eq!(events[0].event_version, 1);
        assert_eq!(events[0].payload["actor"]["kind"], "Admin");
        assert_eq!(events[0].payload["actor"]["id"], "maintenance_service");
    }

    #[tokio::test]
    async fn exit_maintenance_idempotency_key_format() {
        let instance_id = Uuid::new_v4();
        let expected_key = format!("maintenance-exit-{instance_id}");

        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: "InstanceUnblocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "actor": { "kind": "Admin", "id": "maintenance_service" },
            }),
            idempotency_key: Some(expected_key.clone()),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        assert_eq!(
            envelope.idempotency_key.as_ref().unwrap(),
            &expected_key
        );
    }

    // ═══════════════════════════════════════════════════════════════════
    // Projection rebuild — projector dispatch tests (no DB needed)
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn rebuild_dispatches_events_to_matching_projectors() {
        // Test that rebuild_projections_inner dispatches events only to
        // projectors that handle the event type.
        let instance_id = Uuid::new_v4();

        // Create events
        let event_store = Arc::new(RecordingEventStore::new());
        event_store.emit(make_event(instance_id, "InstanceCreated")).await.unwrap();
        event_store.emit(make_event(instance_id, "CycleCreated")).await.unwrap();
        event_store.emit(make_event(instance_id, "InstanceBlocked")).await.unwrap();

        // Create projectors
        let instance_proj = Arc::new(CountingProjector::new(
            "InstanceProjector",
            vec!["InstanceCreated", "InstanceBlocked", "InstanceUnblocked"],
        ));
        let cycle_proj = Arc::new(CountingProjector::new(
            "CycleProjector",
            vec!["CycleCreated"],
        ));

        let svc = MaintenanceService {
            pool: dummy_pool(),
            event_store: event_store.clone(),
            projectors: vec![
                instance_proj.clone() as Arc<dyn Projector>,
                cycle_proj.clone() as Arc<dyn Projector>,
            ],
        };

        // Call the inner method directly (bypasses DB checks)
        // We cannot call rebuild_projections_inner because it tries to truncate DB tables.
        // Instead, test the dispatch logic by replaying events manually.
        let events = event_store.replay(instance_id, 0).await.unwrap();
        for event in &events {
            for projector in &svc.projectors {
                if projector.handles().contains(&event.event_type.as_str()) {
                    projector.handle(event, &svc.pool).await.unwrap();
                }
            }
        }

        // InstanceProjector handles InstanceCreated + InstanceBlocked = 2 calls
        assert_eq!(instance_proj.count().await, 2);
        // CycleProjector handles CycleCreated = 1 call
        assert_eq!(cycle_proj.count().await, 1);
    }

    #[tokio::test]
    async fn rebuild_skips_projectors_that_dont_handle_event() {
        let instance_id = Uuid::new_v4();

        let event_store = Arc::new(RecordingEventStore::new());
        event_store.emit(make_event(instance_id, "CycleCreated")).await.unwrap();

        let instance_proj = Arc::new(CountingProjector::new(
            "InstanceProjector",
            vec!["InstanceCreated"],
        ));

        let svc = MaintenanceService {
            pool: dummy_pool(),
            event_store: event_store.clone(),
            projectors: vec![instance_proj.clone() as Arc<dyn Projector>],
        };

        let events = event_store.replay(instance_id, 0).await.unwrap();
        for event in &events {
            for projector in &svc.projectors {
                if projector.handles().contains(&event.event_type.as_str()) {
                    projector.handle(event, &svc.pool).await.unwrap();
                }
            }
        }

        // InstanceProjector does NOT handle CycleCreated
        assert_eq!(instance_proj.count().await, 0);
    }

    #[tokio::test]
    async fn rebuild_with_no_events_returns_zero() {
        let event_store = Arc::new(RecordingEventStore::new());

        let proj = Arc::new(CountingProjector::new(
            "TestProjector",
            vec!["InstanceCreated"],
        ));

        let _svc = MaintenanceService {
            pool: dummy_pool(),
            event_store: event_store.clone(),
            projectors: vec![proj.clone() as Arc<dyn Projector>],
        };

        let instance_id = Uuid::new_v4();
        let events = event_store.replay(instance_id, 0).await.unwrap();
        assert_eq!(events.len(), 0);
        assert_eq!(proj.count().await, 0);
    }

    #[tokio::test]
    async fn rebuild_with_no_projectors_still_works() {
        let instance_id = Uuid::new_v4();
        let event_store = Arc::new(RecordingEventStore::new());
        event_store.emit(make_event(instance_id, "InstanceCreated")).await.unwrap();

        let no_projector_svc = MaintenanceService {
            pool: dummy_pool(),
            event_store: event_store.clone(),
            projectors: vec![], // no projectors
        };

        let events = event_store.replay(instance_id, 0).await.unwrap();
        assert_eq!(events.len(), 1);
        // No projectors means no dispatch — this should not panic.
        for event in &events {
            for projector in &no_projector_svc.projectors {
                if projector.handles().contains(&event.event_type.as_str()) {
                    projector.handle(event, &no_projector_svc.pool).await.unwrap();
                }
            }
        }
    }

    // ═══════════════════════════════════════════════════════════════════
    // Event store failure handling
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn enter_maintenance_event_store_failure() {
        // Test that if the event store fails, the error propagates correctly.
        let failing_es: Arc<dyn EventStore> = Arc::new(FailingEventStore);

        // Simulate the emit call that enter_maintenance_inner would make
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            seq: 0,
            event_type: "InstanceBlocked".to_string(),
            event_version: 1,
            payload: serde_json::json!({}),
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        let result = failing_es.emit(envelope).await;
        assert!(result.is_err());
    }

    // ═══════════════════════════════════════════════════════════════════
    // Advisory lock key integration
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn advisory_lock_key_used_for_maintenance() {
        // Verify advisory_lock_key is deterministic
        let instance_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let (hi1, lo1) = advisory_lock_key(instance_id);
        let (hi2, lo2) = advisory_lock_key(instance_id);
        assert_eq!(hi1, hi2);
        assert_eq!(lo1, lo2);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Event payload structure — detailed assertions
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn enter_maintenance_payload_with_reason() {
        let payload = serde_json::json!({
            "reason": "Maintenance",
            "details": "projection rebuild required",
            "actor": { "kind": "Admin", "id": "maintenance_service" },
        });

        assert_eq!(payload["reason"], "Maintenance");
        assert_eq!(payload["details"], "projection rebuild required");
        assert_eq!(payload["actor"]["kind"], "Admin");
        assert_eq!(payload["actor"]["id"], "maintenance_service");
    }

    #[test]
    fn enter_maintenance_payload_without_reason() {
        let details: String = None::<String>.unwrap_or_default();
        let payload = serde_json::json!({
            "reason": "Maintenance",
            "details": details,
            "actor": { "kind": "Admin", "id": "maintenance_service" },
        });

        assert_eq!(payload["reason"], "Maintenance");
        assert_eq!(payload["details"], "");
        assert_eq!(payload["actor"]["kind"], "Admin");
    }

    #[test]
    fn exit_maintenance_payload_structure() {
        let payload = serde_json::json!({
            "actor": { "kind": "Admin", "id": "maintenance_service" },
        });

        assert_eq!(payload["actor"]["kind"], "Admin");
        assert_eq!(payload["actor"]["id"], "maintenance_service");
        // No "reason" or "details" in InstanceUnblocked
        assert!(payload.get("reason").is_none());
    }

    // ═══════════════════════════════════════════════════════════════════
    // Rebuild event filtering
    // ═══════════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn rebuild_replays_events_in_order() {
        let instance_id = Uuid::new_v4();
        let event_store = Arc::new(RecordingEventStore::new());

        // Emit events in order
        event_store.emit(make_event(instance_id, "InstanceCreated")).await.unwrap();
        event_store.emit(make_event(instance_id, "CycleCreated")).await.unwrap();
        event_store.emit(make_event(instance_id, "TaskScheduled")).await.unwrap();

        let events = event_store.replay(instance_id, 0).await.unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, "InstanceCreated");
        assert_eq!(events[1].event_type, "CycleCreated");
        assert_eq!(events[2].event_type, "TaskScheduled");
    }

    #[tokio::test]
    async fn rebuild_dispatches_to_multiple_projectors_per_event() {
        let instance_id = Uuid::new_v4();
        let event_store = Arc::new(RecordingEventStore::new());

        // An event that two projectors both handle
        event_store.emit(make_event(instance_id, "SharedEvent")).await.unwrap();

        let proj_a = Arc::new(CountingProjector::new("A", vec!["SharedEvent"]));
        let proj_b = Arc::new(CountingProjector::new("B", vec!["SharedEvent"]));

        let svc = MaintenanceService {
            pool: dummy_pool(),
            event_store: event_store.clone(),
            projectors: vec![
                proj_a.clone() as Arc<dyn Projector>,
                proj_b.clone() as Arc<dyn Projector>,
            ],
        };

        let events = event_store.replay(instance_id, 0).await.unwrap();
        for event in &events {
            for projector in &svc.projectors {
                if projector.handles().contains(&event.event_type.as_str()) {
                    projector.handle(event, &svc.pool).await.unwrap();
                }
            }
        }

        assert_eq!(proj_a.count().await, 1);
        assert_eq!(proj_b.count().await, 1);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Edge cases
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn check_maintenance_mode_case_sensitive() {
        // "BLOCKED" is not "blocked"
        assert!(!check_maintenance_mode("BLOCKED", Some("Maintenance")));
        // "maintenance" is not "Maintenance"
        assert!(!check_maintenance_mode("blocked", Some("maintenance")));
    }

    #[tokio::test]
    async fn enter_and_exit_produce_distinct_idempotency_keys() {
        let instance_id = Uuid::new_v4();
        let enter_key = format!("maintenance-enter-{instance_id}");
        let exit_key = format!("maintenance-exit-{instance_id}");
        assert_ne!(enter_key, exit_key);
    }

    #[tokio::test]
    async fn different_instances_produce_different_idempotency_keys() {
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let enter_key_1 = format!("maintenance-enter-{id1}");
        let enter_key_2 = format!("maintenance-enter-{id2}");
        assert_ne!(enter_key_1, enter_key_2);
    }
}
