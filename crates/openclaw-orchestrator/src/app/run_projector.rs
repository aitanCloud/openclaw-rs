use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use sqlx::PgPool;

/// Projects run lifecycle events into the `orch_runs` table.
///
/// Handles all 7 run state-changing events:
/// - RunClaimed    -> INSERT (claimed)
/// - RunStarted    -> claimed -> running (proof-of-life from worker)
/// - RunCompleted  -> running -> completed (store exit_code, output_json, cost_cents, finished_at)
/// - RunFailed     -> claimed|running -> failed (store failure_category, finished_at)
/// - RunTimedOut   -> running -> timed_out (finished_at)
/// - RunCancelled  -> claimed|running -> cancelled (store cancel_reason, finished_at)
/// - RunAbandoned  -> running -> abandoned (store abandon_reason, finished_at)
///
/// # Anti-pair rule (Section 31.4)
///
/// `RunClaimed` and `RunStarted` MUST NEVER appear in the same transaction.
/// The claim is the orchestrator reserving the run; the start is proof-of-life
/// from the worker process. These are causally separate and mixing them in one
/// tx would violate the single-writer-per-aggregate invariant.
///
/// Enforcement of this rule happens at the command handler level, not here in
/// the projector. The projector is a pure read-side projection that processes
/// events after they are committed.
pub struct RunProjector;

impl RunProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RunProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Projector for RunProjector {
    fn name(&self) -> &'static str {
        "RunProjector"
    }

    fn handles(&self) -> &[&'static str] {
        &[
            "RunClaimed",
            "RunStarted",
            "RunCompleted",
            "RunFailed",
            "RunTimedOut",
            "RunCancelled",
            "RunAbandoned",
        ]
    }

    async fn handle(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        match event.event_type.as_str() {
            "RunClaimed" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;
                let task_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("task_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing task_id".into(),
                            ))
                        })?,
                )?;
                let run_number: i32 = serde_json::from_value(
                    event
                        .payload
                        .get("run_number")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_number".into(),
                            ))
                        })?,
                )?;
                let worker_session_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("worker_session_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing worker_session_id".into(),
                            ))
                        })?,
                )?;
                let branch = event
                    .payload
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let worktree_path = event
                    .payload
                    .get("worktree_path")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let prompt_sent = event
                    .payload
                    .get("prompt_sent")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let lease_until: Option<chrono::DateTime<chrono::Utc>> = event
                    .payload
                    .get("lease_until")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?;

                // Idempotent: INSERT ON CONFLICT DO NOTHING (same event_id processed twice = no-op)
                sqlx::query(
                    r#"
                    INSERT INTO orch_runs (id, task_id, instance_id, run_number, state, worker_session_id, branch, worktree_path, prompt_sent, lease_until, started_at)
                    VALUES ($1, $2, $3, $4, 'claimed', $5, $6, $7, $8, $9, $10)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                )
                .bind(run_id)
                .bind(task_id)
                .bind(event.instance_id)
                .bind(run_number)
                .bind(worker_session_id)
                .bind(&branch)
                .bind(&worktree_path)
                .bind(&prompt_sent)
                .bind(lease_until)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunStarted" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;

                // RunStarted: claimed -> running (proof-of-life from worker)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'running' WHERE id = $1 AND state = 'claimed'",
                )
                .bind(run_id)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunCompleted" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;
                let exit_code: Option<i32> = event
                    .payload
                    .get("exit_code")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?;
                let output_json = event.payload.get("output_json").cloned();
                let cost_cents: i64 = event
                    .payload
                    .get("cost_cents")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()?
                    .unwrap_or(0);

                // RunCompleted: running -> completed (store exit_code, output_json, cost_cents, finished_at)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'completed', exit_code = $2, output_json = $3, cost_cents = $4, finished_at = $5 WHERE id = $1 AND state = 'running'",
                )
                .bind(run_id)
                .bind(exit_code)
                .bind(&output_json)
                .bind(cost_cents)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunFailed" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;
                let failure_category = event
                    .payload
                    .get("failure_category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // RunFailed: claimed|running -> failed (store failure_category, finished_at)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'failed', failure_category = $2, finished_at = $3 WHERE id = $1 AND state IN ('claimed', 'running')",
                )
                .bind(run_id)
                .bind(failure_category)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunTimedOut" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;

                // RunTimedOut: running -> timed_out (finished_at)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'timed_out', finished_at = $2 WHERE id = $1 AND state = 'running'",
                )
                .bind(run_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunCancelled" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // RunCancelled: claimed|running -> cancelled (store cancel_reason, finished_at)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'cancelled', cancel_reason = $2, finished_at = $3 WHERE id = $1 AND state IN ('claimed', 'running')",
                )
                .bind(run_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunAbandoned" => {
                let run_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("run_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing run_id".into(),
                            ))
                        })?,
                )?;
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // RunAbandoned: running -> abandoned (store abandon_reason, finished_at)
                sqlx::query(
                    "UPDATE orch_runs SET state = 'abandoned', abandon_reason = $2, finished_at = $3 WHERE id = $1 AND state = 'running'",
                )
                .bind(run_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            _ => {
                // Unknown event type for this projector -- ignore
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::events::{EventCriticality, EventEnvelope, EVENT_KINDS};

    /// Create a test EventEnvelope with the given event type and payload.
    fn fixture(
        instance_id: uuid::Uuid,
        event_type: &str,
        payload: serde_json::Value,
    ) -> EventEnvelope {
        EventEnvelope {
            event_id: uuid::Uuid::new_v4(),
            instance_id,
            seq: 0,
            event_type: event_type.to_string(),
            event_version: 1,
            payload,
            idempotency_key: None,
            correlation_id: None,
            causation_id: None,
            occurred_at: chrono::Utc::now(),
            recorded_at: chrono::Utc::now(),
        }
    }

    /// Returns the set of run event types (starting with "Run", excluding "RunBudget" and "RunCost").
    fn run_event_types() -> Vec<&'static str> {
        EVENT_KINDS
            .iter()
            .filter(|e| {
                e.event_type.starts_with("Run")
                    && !e.event_type.starts_with("RunBudget")
                    && !e.event_type.starts_with("RunCost")
            })
            .map(|e| e.event_type)
            .collect()
    }

    // ── Golden test: projector covers all required state-changing events ──

    #[test]
    fn run_projector_handles_all_run_events() {
        let projector = RunProjector::new();
        let handled = projector.handles();

        // Every Run* event (excluding RunBudget*/RunCost*) in EVENT_KINDS that is StateChanging must be handled
        let run_events: Vec<&str> = EVENT_KINDS
            .iter()
            .filter(|e| {
                e.event_type.starts_with("Run")
                    && !e.event_type.starts_with("RunBudget")
                    && !e.event_type.starts_with("RunCost")
            })
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .map(|e| e.event_type)
            .collect();

        for event_type in &run_events {
            assert!(
                handled.contains(event_type),
                "RunProjector does not handle state-changing event: {event_type}"
            );
        }
    }

    #[test]
    fn handles_returns_correct_count() {
        let projector = RunProjector::new();
        assert_eq!(projector.handles().len(), 7, "expected 7 run events");
    }

    #[test]
    fn handles_only_run_events() {
        let projector = RunProjector::new();
        let run_types = run_event_types();
        for event_type in projector.handles() {
            assert!(
                run_types.contains(event_type),
                "RunProjector handles event not in run set: {event_type}"
            );
        }
    }

    #[test]
    fn no_duplicate_handles() {
        let projector = RunProjector::new();
        let handles = projector.handles();
        let mut seen = std::collections::HashSet::new();
        for event_type in handles {
            assert!(
                seen.insert(*event_type),
                "duplicate event type in handles(): {event_type}"
            );
        }
    }

    #[test]
    fn name_is_correct() {
        let projector = RunProjector::new();
        assert_eq!(projector.name(), "RunProjector");
    }

    #[test]
    fn default_impl_works() {
        let projector = RunProjector::default();
        assert_eq!(projector.name(), "RunProjector");
    }

    // ── Golden test: fixture helper produces valid envelopes ──

    #[test]
    fn fixture_creates_valid_envelope() {
        let instance_id = uuid::Uuid::new_v4();
        let run_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let worker_session_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "run_id": run_id,
            "task_id": task_id,
            "run_number": 1,
            "worker_session_id": worker_session_id,
            "branch": "feature/login",
            "worktree_path": "/tmp/worktrees/login",
            "prompt_sent": "implement login feature",
            "lease_until": "2026-02-28T12:00:00Z",
        });
        let event = fixture(instance_id, "RunClaimed", payload);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_type, "RunClaimed");
        assert_eq!(event.event_version, 1);
        assert!(event.idempotency_key.is_none());
        assert!(event.correlation_id.is_none());
        assert!(event.causation_id.is_none());
        assert_eq!(event.seq, 0);
    }

    #[test]
    fn fixture_generates_unique_event_ids() {
        let instance_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({});
        let e1 = fixture(instance_id, "RunClaimed", payload.clone());
        let e2 = fixture(instance_id, "RunClaimed", payload);
        assert_ne!(e1.event_id, e2.event_id, "each fixture should get a unique event_id");
    }

    // ── CI coverage gate: all handled events exist in EVENT_KINDS ──

    #[test]
    fn all_handled_events_exist_in_event_kinds() {
        let projector = RunProjector::new();
        let known_types: Vec<&str> = EVENT_KINDS.iter().map(|e| e.event_type).collect();
        for event_type in projector.handles() {
            assert!(
                known_types.contains(event_type),
                "RunProjector handles unknown event: {event_type}"
            );
        }
    }

    #[test]
    fn all_handled_events_are_state_changing() {
        let projector = RunProjector::new();
        for event_type in projector.handles() {
            let entry = EVENT_KINDS
                .iter()
                .find(|e| e.event_type == *event_type)
                .unwrap_or_else(|| panic!("event type not in EVENT_KINDS: {event_type}"));
            assert_eq!(
                entry.criticality,
                EventCriticality::StateChanging,
                "RunProjector handles non-state-changing event: {event_type}"
            );
        }
    }

    // ── Coverage gate: handles() exactly matches the canonical run event set ──

    #[test]
    fn handles_matches_canonical_run_event_set() {
        let projector = RunProjector::new();
        let handled: std::collections::HashSet<&&str> = projector.handles().iter().collect();
        let canonical: std::collections::HashSet<&str> = run_event_types().into_iter().collect();

        // Every canonical run event must be handled
        for event_type in &canonical {
            assert!(
                handled.contains(event_type),
                "RunProjector missing canonical run event: {event_type}"
            );
        }

        // Every handled event must be in the canonical set
        for event_type in &handled {
            assert!(
                canonical.contains(**event_type),
                "RunProjector handles extra event not in canonical set: {event_type}"
            );
        }
    }
}
