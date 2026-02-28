use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use sqlx::PgPool;

/// Projects task lifecycle events into the `orch_tasks` table.
///
/// Handles all 6 task state-changing events:
/// - TaskScheduled       -> INSERT (scheduled)
/// - TaskRetryScheduled  -> active|verifying -> scheduled (reset active_run_id to NULL, keep attempt count)
/// - TaskPassed          -> verifying -> passed
/// - TaskFailed          -> active|verifying -> failed (stores failure_reason)
/// - TaskCancelled       -> scheduled|active -> cancelled (stores cancel_reason)
/// - TaskSkipped         -> scheduled -> skipped (stores skip_reason)
pub struct TaskProjector;

impl TaskProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TaskProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Projector for TaskProjector {
    fn name(&self) -> &'static str {
        "TaskProjector"
    }

    fn handles(&self) -> &[&'static str] {
        &[
            "TaskScheduled",
            "TaskRetryScheduled",
            "TaskPassed",
            "TaskFailed",
            "TaskCancelled",
            "TaskSkipped",
        ]
    }

    async fn handle(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        match event.event_type.as_str() {
            "TaskScheduled" => {
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
                let cycle_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("cycle_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing cycle_id".into(),
                            ))
                        })?,
                )?;
                let task_key: String = serde_json::from_value(
                    event
                        .payload
                        .get("task_key")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing task_key".into(),
                            ))
                        })?,
                )?;
                let phase: String = serde_json::from_value(
                    event.payload.get("phase").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing phase".into(),
                        ))
                    })?,
                )?;
                let ordinal: i32 = serde_json::from_value(
                    event.payload.get("ordinal").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing ordinal".into(),
                        ))
                    })?,
                )?;
                let title: String = serde_json::from_value(
                    event.payload.get("title").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing title".into(),
                        ))
                    })?,
                )?;
                let description: String = serde_json::from_value(
                    event
                        .payload
                        .get("description")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing description".into(),
                            ))
                        })?,
                )?;
                let acceptance = event.payload.get("acceptance").cloned().ok_or_else(|| {
                    AppError::Domain(crate::domain::errors::DomainError::Precondition(
                        "missing acceptance".into(),
                    ))
                })?;
                let max_retries: i32 = serde_json::from_value(
                    event
                        .payload
                        .get("max_retries")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing max_retries".into(),
                            ))
                        })?,
                )?;

                // Idempotent: INSERT ON CONFLICT DO NOTHING (same event_id processed twice = no-op)
                sqlx::query(
                    r#"
                    INSERT INTO orch_tasks (id, cycle_id, instance_id, task_key, phase, ordinal, state, title, description, acceptance, max_retries, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, $6, 'scheduled', $7, $8, $9, $10, $11, $11)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                )
                .bind(task_id)
                .bind(cycle_id)
                .bind(event.instance_id)
                .bind(&task_key)
                .bind(&phase)
                .bind(ordinal)
                .bind(&title)
                .bind(&description)
                .bind(&acceptance)
                .bind(max_retries)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "TaskRetryScheduled" => {
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

                // TaskRetryScheduled: active|verifying -> scheduled, reset active_run_id, keep attempt count
                sqlx::query(
                    "UPDATE orch_tasks SET state = 'scheduled', active_run_id = NULL, updated_at = $2 WHERE id = $1 AND state IN ('active', 'verifying')",
                )
                .bind(task_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "TaskPassed" => {
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

                // TaskPassed: verifying -> passed (no changes to active_run_id)
                sqlx::query(
                    "UPDATE orch_tasks SET state = 'passed', updated_at = $2 WHERE id = $1 AND state = 'verifying'",
                )
                .bind(task_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "TaskFailed" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // TaskFailed: active|verifying -> failed (store failure_reason)
                sqlx::query(
                    "UPDATE orch_tasks SET state = 'failed', failure_reason = $2, updated_at = $3 WHERE id = $1 AND state IN ('active', 'verifying')",
                )
                .bind(task_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "TaskCancelled" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // TaskCancelled: scheduled|active -> cancelled (store cancel_reason)
                sqlx::query(
                    "UPDATE orch_tasks SET state = 'cancelled', cancel_reason = $2, updated_at = $3 WHERE id = $1 AND state IN ('scheduled', 'active')",
                )
                .bind(task_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "TaskSkipped" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // TaskSkipped: scheduled -> skipped (store skip_reason)
                sqlx::query(
                    "UPDATE orch_tasks SET state = 'skipped', skip_reason = $2, updated_at = $3 WHERE id = $1 AND state = 'scheduled'",
                )
                .bind(task_id)
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

    /// Returns the set of task event types (those starting with "Task").
    fn task_event_types() -> Vec<&'static str> {
        EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Task"))
            .map(|e| e.event_type)
            .collect()
    }

    // ── Golden test: projector covers all required state-changing events ──

    #[test]
    fn task_projector_handles_all_task_events() {
        let projector = TaskProjector::new();
        let handled = projector.handles();

        // Every Task* event in EVENT_KINDS that is StateChanging must be handled
        let task_events: Vec<&str> = EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Task"))
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .map(|e| e.event_type)
            .collect();

        for event_type in &task_events {
            assert!(
                handled.contains(event_type),
                "TaskProjector does not handle state-changing event: {event_type}"
            );
        }
    }

    #[test]
    fn handles_returns_correct_count() {
        let projector = TaskProjector::new();
        assert_eq!(projector.handles().len(), 6, "expected 6 task events");
    }

    #[test]
    fn handles_only_task_events() {
        let projector = TaskProjector::new();
        let task_types = task_event_types();
        for event_type in projector.handles() {
            assert!(
                task_types.contains(event_type),
                "TaskProjector handles event not in task set: {event_type}"
            );
        }
    }

    #[test]
    fn no_duplicate_handles() {
        let projector = TaskProjector::new();
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
        let projector = TaskProjector::new();
        assert_eq!(projector.name(), "TaskProjector");
    }

    #[test]
    fn default_impl_works() {
        let projector = TaskProjector::default();
        assert_eq!(projector.name(), "TaskProjector");
    }

    // ── Golden test: fixture helper produces valid envelopes ──

    #[test]
    fn fixture_creates_valid_envelope() {
        let instance_id = uuid::Uuid::new_v4();
        let task_id = uuid::Uuid::new_v4();
        let cycle_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "task_id": task_id,
            "cycle_id": cycle_id,
            "task_key": "implement-login",
            "phase": "implement",
            "ordinal": 1,
            "title": "Implement login",
            "description": "Build the login feature",
            "acceptance": {"criteria": "tests pass"},
            "max_retries": 3,
        });
        let event = fixture(instance_id, "TaskScheduled", payload);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_type, "TaskScheduled");
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
        let e1 = fixture(instance_id, "TaskScheduled", payload.clone());
        let e2 = fixture(instance_id, "TaskScheduled", payload);
        assert_ne!(e1.event_id, e2.event_id, "each fixture should get a unique event_id");
    }

    // ── CI coverage gate: all handled events exist in EVENT_KINDS ──

    #[test]
    fn all_handled_events_exist_in_event_kinds() {
        let projector = TaskProjector::new();
        let known_types: Vec<&str> = EVENT_KINDS.iter().map(|e| e.event_type).collect();
        for event_type in projector.handles() {
            assert!(
                known_types.contains(event_type),
                "TaskProjector handles unknown event: {event_type}"
            );
        }
    }

    #[test]
    fn all_handled_events_are_state_changing() {
        let projector = TaskProjector::new();
        for event_type in projector.handles() {
            let entry = EVENT_KINDS
                .iter()
                .find(|e| e.event_type == *event_type)
                .unwrap_or_else(|| panic!("event type not in EVENT_KINDS: {event_type}"));
            assert_eq!(
                entry.criticality,
                EventCriticality::StateChanging,
                "TaskProjector handles non-state-changing event: {event_type}"
            );
        }
    }

    // ── Coverage gate: handles() exactly matches the canonical task event set ──

    #[test]
    fn handles_matches_canonical_task_event_set() {
        let projector = TaskProjector::new();
        let handled: std::collections::HashSet<&&str> = projector.handles().iter().collect();
        let canonical: std::collections::HashSet<&str> = task_event_types().into_iter().collect();

        // Every canonical task event must be handled
        for event_type in &canonical {
            assert!(
                handled.contains(event_type),
                "TaskProjector missing canonical task event: {event_type}"
            );
        }

        // Every handled event must be in the canonical set
        for event_type in &handled {
            assert!(
                canonical.contains(**event_type),
                "TaskProjector handles extra event not in canonical set: {event_type}"
            );
        }
    }
}
