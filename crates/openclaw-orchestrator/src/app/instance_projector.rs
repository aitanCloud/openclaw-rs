use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use sqlx::PgPool;

/// Projects instance lifecycle events into the `orch_instances` table.
///
/// Handles all 7 instance state-changing events:
/// - InstanceCreated -> INSERT (provisioning)
/// - InstanceProvisioned -> provisioning -> active
/// - InstanceProvisioningFailed -> provisioning -> provisioning_failed
/// - InstanceBlocked -> active -> blocked
/// - InstanceUnblocked -> blocked -> active
/// - InstanceSuspended -> active -> suspended
/// - InstanceResumed -> suspended -> active
pub struct InstanceProjector;

impl InstanceProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for InstanceProjector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Projector for InstanceProjector {
    fn name(&self) -> &'static str {
        "InstanceProjector"
    }

    fn handles(&self) -> &[&'static str] {
        &[
            "InstanceCreated",
            "InstanceProvisioned",
            "InstanceProvisioningFailed",
            "InstanceBlocked",
            "InstanceUnblocked",
            "InstanceSuspended",
            "InstanceResumed",
        ]
    }

    async fn handle(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        match event.event_type.as_str() {
            "InstanceCreated" => {
                let project_id: uuid::Uuid = serde_json::from_value(
                    event
                        .payload
                        .get("project_id")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing project_id".into(),
                            ))
                        })?,
                )?;
                let name: String = serde_json::from_value(
                    event.payload.get("name").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing name".into(),
                        ))
                    })?,
                )?;
                let data_dir: String = serde_json::from_value(
                    event.payload.get("data_dir").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing data_dir".into(),
                        ))
                    })?,
                )?;
                let token_hash: String = serde_json::from_value(
                    event.payload.get("token_hash").cloned().ok_or_else(|| {
                        AppError::Domain(crate::domain::errors::DomainError::Precondition(
                            "missing token_hash".into(),
                        ))
                    })?,
                )?;

                // Idempotent: INSERT ON CONFLICT DO NOTHING (same event_id processed twice = no-op)
                sqlx::query(
                    r#"
                    INSERT INTO orch_instances (id, project_id, name, state, data_dir, token_hash, started_at, last_heartbeat)
                    VALUES ($1, $2, $3, 'provisioning', $4, $5, $6, $6)
                    ON CONFLICT (id) DO NOTHING
                    "#,
                )
                .bind(event.instance_id)
                .bind(project_id)
                .bind(&name)
                .bind(&data_dir)
                .bind(&token_hash)
                .bind(event.occurred_at)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "InstanceProvisioned" => {
                sqlx::query(
                    "UPDATE orch_instances SET state = 'active' WHERE id = $1 AND state = 'provisioning'",
                )
                .bind(event.instance_id)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
                Ok(())
            }
            "InstanceProvisioningFailed" => {
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                sqlx::query(
                    "UPDATE orch_instances SET state = 'provisioning_failed', block_reason = $2 WHERE id = $1 AND state = 'provisioning'",
                )
                .bind(event.instance_id)
                .bind(reason)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
                Ok(())
            }
            "InstanceBlocked" => {
                let reason = event
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                sqlx::query(
                    "UPDATE orch_instances SET state = 'blocked', block_reason = $2 WHERE id = $1 AND state = 'active'",
                )
                .bind(event.instance_id)
                .bind(reason)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
                Ok(())
            }
            "InstanceUnblocked" => {
                sqlx::query(
                    "UPDATE orch_instances SET state = 'active', block_reason = NULL WHERE id = $1 AND state = 'blocked'",
                )
                .bind(event.instance_id)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
                Ok(())
            }
            "InstanceSuspended" => {
                sqlx::query(
                    "UPDATE orch_instances SET state = 'suspended' WHERE id = $1 AND state = 'active'",
                )
                .bind(event.instance_id)
                .execute(pool)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
                Ok(())
            }
            "InstanceResumed" => {
                sqlx::query(
                    "UPDATE orch_instances SET state = 'active' WHERE id = $1 AND state = 'suspended'",
                )
                .bind(event.instance_id)
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
    ///
    /// This is the golden fixture helper used by all projector tests.
    pub fn fixture(
        instance_id: uuid::Uuid,
        event_type: &str,
        payload: serde_json::Value,
    ) -> EventEnvelope {
        EventEnvelope {
            event_id: uuid::Uuid::new_v4(),
            instance_id,
            seq: 0, // will be set by event store
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

    // ── Golden test: projector covers all required state-changing events ──

    #[test]
    fn instance_projector_handles_all_instance_events() {
        let projector = InstanceProjector::new();
        let handled = projector.handles();

        // Every Instance* event in EVENT_KINDS that is StateChanging must be handled
        let instance_events: Vec<&str> = EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Instance"))
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .map(|e| e.event_type)
            .collect();

        for event_type in &instance_events {
            assert!(
                handled.contains(event_type),
                "InstanceProjector does not handle state-changing event: {event_type}"
            );
        }
    }

    #[test]
    fn handles_returns_correct_count() {
        let projector = InstanceProjector::new();
        assert_eq!(projector.handles().len(), 7, "expected 7 instance events");
    }

    #[test]
    fn handles_only_instance_events() {
        let projector = InstanceProjector::new();
        for event_type in projector.handles() {
            assert!(
                event_type.starts_with("Instance"),
                "InstanceProjector handles non-Instance event: {event_type}"
            );
        }
    }

    #[test]
    fn no_duplicate_handles() {
        let projector = InstanceProjector::new();
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
        let projector = InstanceProjector::new();
        assert_eq!(projector.name(), "InstanceProjector");
    }

    #[test]
    fn default_impl_works() {
        let projector = InstanceProjector::default();
        assert_eq!(projector.name(), "InstanceProjector");
    }

    // ── Golden test: fixture helper produces valid envelopes ──

    #[test]
    fn fixture_creates_valid_envelope() {
        let instance_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "project_id": uuid::Uuid::new_v4(),
            "name": "test-instance",
            "data_dir": "/tmp/test",
            "token_hash": "hash123"
        });
        let event = fixture(instance_id, "InstanceCreated", payload);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_type, "InstanceCreated");
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
        let e1 = fixture(instance_id, "InstanceCreated", payload.clone());
        let e2 = fixture(instance_id, "InstanceCreated", payload);
        assert_ne!(e1.event_id, e2.event_id, "each fixture should get a unique event_id");
    }

    // ── CI coverage gate: all Instance state-changing events are in EVENT_KINDS ──

    #[test]
    fn all_handled_events_exist_in_event_kinds() {
        let projector = InstanceProjector::new();
        let known_types: Vec<&str> = EVENT_KINDS.iter().map(|e| e.event_type).collect();
        for event_type in projector.handles() {
            assert!(
                known_types.contains(event_type),
                "InstanceProjector handles unknown event: {event_type}"
            );
        }
    }

    #[test]
    fn all_handled_events_are_state_changing() {
        let projector = InstanceProjector::new();
        for event_type in projector.handles() {
            let entry = EVENT_KINDS
                .iter()
                .find(|e| e.event_type == *event_type)
                .unwrap_or_else(|| panic!("event type not in EVENT_KINDS: {event_type}"));
            assert_eq!(
                entry.criticality,
                EventCriticality::StateChanging,
                "InstanceProjector handles non-state-changing event: {event_type}"
            );
        }
    }
}
