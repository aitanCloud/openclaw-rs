use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Canonical event envelope for all domain events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub instance_id: Uuid,
    pub seq: i64,
    pub event_type: String,
    pub event_version: u16,
    pub payload: serde_json::Value,
    pub idempotency_key: Option<String>,
    pub correlation_id: Option<Uuid>,
    pub causation_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

/// Whether an event changes domain state or is purely informational.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCriticality {
    StateChanging,
    Informational,
}

/// Static metadata for each known event type.
#[derive(Debug, Clone)]
pub struct EventKindEntry {
    pub event_type: &'static str,
    pub current_version: u16,
    pub criticality: EventCriticality,
}

/// Authoritative registry of all event types in the system.
///
/// 42 state-changing + 9 informational = 51 total.
pub const EVENT_KINDS: &[EventKindEntry] = &[
    // ── Instance (7 state-changing) ──
    EventKindEntry {
        event_type: "InstanceCreated",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceProvisioned",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceProvisioningFailed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceBlocked",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceUnblocked",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceSuspended",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "InstanceResumed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    // ── Cycle (11 state-changing) ──
    EventKindEntry {
        event_type: "CycleCreated",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "PlanRequested",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "PlanGenerated",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "PlanApproved",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleRunningStarted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleCompleting",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleBlocked",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleUnblocked",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleCompleted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleFailed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleCancelled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    // ── Task (6 state-changing) ──
    EventKindEntry {
        event_type: "TaskScheduled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "TaskRetryScheduled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "TaskPassed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "TaskFailed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "TaskCancelled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "TaskSkipped",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    // ── Run (7 state-changing) ──
    EventKindEntry {
        event_type: "RunClaimed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunStarted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunCompleted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunFailed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunTimedOut",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunCancelled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunAbandoned",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    // ── Merge (5 state-changing) ──
    EventKindEntry {
        event_type: "MergeAttempted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "MergeSucceeded",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "MergeConflicted",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "MergeFailed",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "MergeSkipped",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    // ── Budget (6 state-changing + 1 informational) ──
    EventKindEntry {
        event_type: "PlanBudgetReserved",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunBudgetReserved",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunCostObserved",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "RunBudgetSettled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "CycleBudgetSettled",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "GlobalBudgetCapHit",
        current_version: 1,
        criticality: EventCriticality::StateChanging,
    },
    EventKindEntry {
        event_type: "BudgetWarningIssued",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    // ── Informational (8) ──
    EventKindEntry {
        event_type: "WorkerOutput",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "DiagnosticsRecorded",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "NotificationSent",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "RecoveryStarted",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "RecoveryCompleted",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "ArtifactsPurged",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "CircuitOpened",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
    EventKindEntry {
        event_type: "CircuitClosed",
        current_version: 1,
        criticality: EventCriticality::Informational,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn total_event_count() {
        assert_eq!(EVENT_KINDS.len(), 51, "expected 51 total event kinds");
    }

    #[test]
    fn state_changing_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .count();
        assert_eq!(count, 42, "expected 42 state-changing event kinds");
    }

    #[test]
    fn informational_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| e.criticality == EventCriticality::Informational)
            .count();
        assert_eq!(count, 9, "expected 9 informational event kinds");
    }

    #[test]
    fn no_duplicate_event_types() {
        let mut seen = HashSet::new();
        for entry in EVENT_KINDS {
            assert!(
                seen.insert(entry.event_type),
                "duplicate event_type: {}",
                entry.event_type
            );
        }
    }

    #[test]
    fn all_versions_at_least_one() {
        for entry in EVENT_KINDS {
            assert!(
                entry.current_version >= 1,
                "event_type {} has version 0",
                entry.event_type
            );
        }
    }

    #[test]
    fn instance_events_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Instance"))
            .count();
        assert_eq!(count, 7, "expected 7 Instance events");
    }

    #[test]
    fn cycle_events_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| {
                (e.event_type.starts_with("Cycle")
                    && !e.event_type.starts_with("CycleBudget"))
                    || (e.event_type.starts_with("Plan")
                        && !e.event_type.starts_with("PlanBudget"))
            })
            .count();
        assert_eq!(count, 11, "expected 11 Cycle events");
    }

    #[test]
    fn task_events_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Task"))
            .count();
        assert_eq!(count, 6, "expected 6 Task events");
    }

    #[test]
    fn run_events_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| {
                e.event_type.starts_with("Run")
                    && !e.event_type.starts_with("RunBudget")
                    && !e.event_type.starts_with("RunCost")
            })
            .count();
        assert_eq!(count, 7, "expected 7 Run events");
    }

    #[test]
    fn merge_events_count() {
        let count = EVENT_KINDS
            .iter()
            .filter(|e| e.event_type.starts_with("Merge"))
            .count();
        assert_eq!(count, 5, "expected 5 Merge events");
    }

    #[test]
    fn event_envelope_serde_round_trip() {
        let now = Utc::now();
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            seq: 42,
            event_type: "CycleCreated".to_string(),
            event_version: 1,
            payload: serde_json::json!({"name": "test-cycle"}),
            idempotency_key: Some("idem-1".to_string()),
            correlation_id: Some(Uuid::new_v4()),
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };

        let json = serde_json::to_string(&envelope).expect("serialize");
        let restored: EventEnvelope = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(envelope.event_id, restored.event_id);
        assert_eq!(envelope.instance_id, restored.instance_id);
        assert_eq!(envelope.seq, restored.seq);
        assert_eq!(envelope.event_type, restored.event_type);
        assert_eq!(envelope.event_version, restored.event_version);
        assert_eq!(envelope.idempotency_key, restored.idempotency_key);
        assert_eq!(envelope.correlation_id, restored.correlation_id);
        assert_eq!(envelope.causation_id, restored.causation_id);
    }
}
