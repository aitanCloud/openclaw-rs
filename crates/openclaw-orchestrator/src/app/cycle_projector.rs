use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use sqlx::PgPool;

/// Projects cycle lifecycle events into the `orch_cycles` table.
///
/// Handles all 12 cycle state-changing events:
/// - CycleCreated         -> INSERT (created)
/// - PlanRequested        -> created -> planning
/// - PlanGenerated        -> planning -> plan_ready (stores plan JSON)
/// - PlanApproved         -> plan_ready -> approved
/// - PlanGenerationFailed -> planning -> failed (stores failure_reason)
/// - CycleRunningStarted  -> approved -> running
/// - CycleCompleting      -> running -> completing
/// - CycleBlocked         -> running -> blocked (stores block_reason)
/// - CycleUnblocked       -> blocked -> running (clears block_reason)
/// - CycleCompleted       -> completing -> completed
/// - CycleFailed          -> running|completing -> failed (stores failure_reason)
/// - CycleCancelled       -> plan_ready|running -> cancelled (stores cancel_reason)
///
/// Note on co-emission (Section 31.3 rule 1):
/// PlanApproved and PlanBudgetReserved are co-emitted in the same transaction.
/// This projector handles PlanApproved only; PlanBudgetReserved is handled by
/// the BudgetProjector (Task 1.3).
pub struct CycleProjector;

impl CycleProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CycleProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Projector for CycleProjector {
    fn name(&self) -> &'static str {
        "CycleProjector"
    }

    fn handles(&self) -> &[&'static str] {
        &[
            "CycleCreated",
            "PlanRequested",
            "PlanGenerated",
            "PlanApproved",
            "PlanGenerationFailed",
            "CycleRunningStarted",
            "CycleCompleting",
            "CycleBlocked",
            "CycleUnblocked",
            "CycleCompleted",
            "CycleFailed",
            "CycleCancelled",
        ]
    }

    async fn handle(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        match event.event_type.as_str() {
            "CycleCreated" => {
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
                let prompt: String = serde_json::from_value(
                    event.payload.get("prompt").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing prompt".into(),
                        ))
                    })?,
                )?;

                // Idempotent: INSERT ON CONFLICT DO NOTHING (same event_id processed twice = no-op)
                sqlx::query(
                    r#"
                    INSERT INTO orch_cycles (id, instance_id, state, prompt, created_at, updated_at)
                    VALUES ($1, $2, 'created', $3, $4, $4)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                )
                .bind(cycle_id)
                .bind(event.instance_id)
                .bind(&prompt)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "PlanRequested" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'planning', updated_at = $2 WHERE id = $1 AND state = 'created'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "PlanGenerated" => {
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
                let plan = event.payload.get("plan").cloned().ok_or_else(|| {
                    AppError::Domain(crate::domain::errors::DomainError::Precondition(
                        "missing plan".into(),
                    ))
                })?;

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'plan_ready', plan = $2, updated_at = $3 WHERE id = $1 AND state = 'planning'",
                )
                .bind(cycle_id)
                .bind(&plan)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "PlanApproved" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'approved', updated_at = $2 WHERE id = $1 AND state = 'plan_ready'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "PlanGenerationFailed" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'failed', failure_reason = $2, updated_at = $3 WHERE id = $1 AND state = 'planning'",
                )
                .bind(cycle_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleRunningStarted" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'running', updated_at = $2 WHERE id = $1 AND state = 'approved'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleCompleting" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'completing', updated_at = $2 WHERE id = $1 AND state = 'running'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleBlocked" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'blocked', block_reason = $2, updated_at = $3 WHERE id = $1 AND state = 'running'",
                )
                .bind(cycle_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleUnblocked" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'running', block_reason = NULL, updated_at = $2 WHERE id = $1 AND state = 'blocked'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleCompleted" => {
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

                sqlx::query(
                    "UPDATE orch_cycles SET state = 'completed', updated_at = $2 WHERE id = $1 AND state = 'completing'",
                )
                .bind(cycle_id)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleFailed" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // CycleFailed can transition from running OR completing
                sqlx::query(
                    "UPDATE orch_cycles SET state = 'failed', failure_reason = $2, updated_at = $3 WHERE id = $1 AND state IN ('running', 'completing')",
                )
                .bind(cycle_id)
                .bind(reason)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleCancelled" => {
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
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                // CycleCancelled can transition from plan_ready OR running
                sqlx::query(
                    "UPDATE orch_cycles SET state = 'cancelled', cancel_reason = $2, updated_at = $3 WHERE id = $1 AND state IN ('plan_ready', 'running')",
                )
                .bind(cycle_id)
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

    /// Returns the set of cycle/plan event types (excluding budget events).
    /// This is the canonical filter for which events belong to the CycleProjector.
    fn cycle_event_types() -> Vec<&'static str> {
        EVENT_KINDS
            .iter()
            .filter(|e| {
                (e.event_type.starts_with("Cycle")
                    && !e.event_type.starts_with("CycleBudget"))
                    || (e.event_type.starts_with("Plan")
                        && !e.event_type.starts_with("PlanBudget"))
            })
            .map(|e| e.event_type)
            .collect()
    }

    // ── Golden test: projector covers all required state-changing events ──

    #[test]
    fn cycle_projector_handles_all_cycle_events() {
        let projector = CycleProjector::new();
        let handled = projector.handles();

        // Every Cycle*/Plan* event (excluding budget) in EVENT_KINDS that is StateChanging must be handled
        let cycle_events: Vec<&str> = EVENT_KINDS
            .iter()
            .filter(|e| {
                (e.event_type.starts_with("Cycle")
                    && !e.event_type.starts_with("CycleBudget"))
                    || (e.event_type.starts_with("Plan")
                        && !e.event_type.starts_with("PlanBudget"))
            })
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .map(|e| e.event_type)
            .collect();

        for event_type in &cycle_events {
            assert!(
                handled.contains(event_type),
                "CycleProjector does not handle state-changing event: {event_type}"
            );
        }
    }

    #[test]
    fn handles_returns_correct_count() {
        let projector = CycleProjector::new();
        assert_eq!(projector.handles().len(), 12, "expected 12 cycle events");
    }

    #[test]
    fn handles_only_cycle_and_plan_events() {
        let projector = CycleProjector::new();
        let cycle_types = cycle_event_types();
        for event_type in projector.handles() {
            assert!(
                cycle_types.contains(event_type),
                "CycleProjector handles event not in cycle/plan set: {event_type}"
            );
        }
    }

    #[test]
    fn no_duplicate_handles() {
        let projector = CycleProjector::new();
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
        let projector = CycleProjector::new();
        assert_eq!(projector.name(), "CycleProjector");
    }

    #[test]
    fn default_impl_works() {
        let projector = CycleProjector::default();
        assert_eq!(projector.name(), "CycleProjector");
    }

    // ── Golden test: fixture helper produces valid envelopes ──

    #[test]
    fn fixture_creates_valid_envelope() {
        let instance_id = uuid::Uuid::new_v4();
        let cycle_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "cycle_id": cycle_id,
            "prompt": "implement feature X",
            "instance_id": instance_id,
        });
        let event = fixture(instance_id, "CycleCreated", payload);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_type, "CycleCreated");
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
        let e1 = fixture(instance_id, "CycleCreated", payload.clone());
        let e2 = fixture(instance_id, "CycleCreated", payload);
        assert_ne!(e1.event_id, e2.event_id, "each fixture should get a unique event_id");
    }

    // ── CI coverage gate: all handled events exist in EVENT_KINDS ──

    #[test]
    fn all_handled_events_exist_in_event_kinds() {
        let projector = CycleProjector::new();
        let known_types: Vec<&str> = EVENT_KINDS.iter().map(|e| e.event_type).collect();
        for event_type in projector.handles() {
            assert!(
                known_types.contains(event_type),
                "CycleProjector handles unknown event: {event_type}"
            );
        }
    }

    #[test]
    fn all_handled_events_are_state_changing() {
        let projector = CycleProjector::new();
        for event_type in projector.handles() {
            let entry = EVENT_KINDS
                .iter()
                .find(|e| e.event_type == *event_type)
                .unwrap_or_else(|| panic!("event type not in EVENT_KINDS: {event_type}"));
            assert_eq!(
                entry.criticality,
                EventCriticality::StateChanging,
                "CycleProjector handles non-state-changing event: {event_type}"
            );
        }
    }

    // ── Coverage gate: handles() exactly matches the canonical cycle event set ──

    #[test]
    fn handles_matches_canonical_cycle_event_set() {
        let projector = CycleProjector::new();
        let handled: std::collections::HashSet<&&str> = projector.handles().iter().collect();
        let canonical: std::collections::HashSet<&str> = cycle_event_types().into_iter().collect();

        // Every canonical cycle event must be handled
        for event_type in &canonical {
            assert!(
                handled.contains(event_type),
                "CycleProjector missing canonical cycle event: {event_type}"
            );
        }

        // Every handled event must be in the canonical set
        for event_type in &handled {
            assert!(
                canonical.contains(**event_type),
                "CycleProjector handles extra event not in canonical set: {event_type}"
            );
        }
    }
}
