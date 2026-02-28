use crate::app::errors::AppError;
use crate::app::projector::Projector;
use crate::domain::events::EventEnvelope;
use crate::infra::errors::InfraError;
use sqlx::PgPool;

/// Projects budget lifecycle events into the `orch_budget_ledger` table.
///
/// Handles all 6 state-changing budget events:
/// - PlanBudgetReserved  -> reserve budget for a cycle's plan
/// - RunBudgetReserved   -> reserve budget for a run
/// - RunCostObserved     -> record actual cost during/after a run
/// - RunBudgetSettled    -> settle run budget (release unused reservation)
/// - CycleBudgetSettled  -> settle entire cycle budget (release remaining)
/// - GlobalBudgetCapHit  -> marker entry when global cap is reached
///
/// BudgetWarningIssued is informational and NOT handled by this projector.
///
/// # Append-only ledger
///
/// Unlike other projectors that UPDATE rows, the budget projector INSERTs into
/// an append-only ledger. Each entry records:
/// - `amount_cents`: positive for reservations/spend, negative for releases
/// - `balance_after`: running balance per instance, computed atomically via subquery
///
/// # Idempotency
///
/// Uses `event_id` as the ledger entry's primary key (`id`). INSERT ON CONFLICT
/// (id) DO NOTHING ensures replaying the same event is a no-op.
///
/// # Co-emission rules (Section 31.3)
///
/// The following pairs MUST be co-emitted in the same transaction:
/// 1. PlanApproved + PlanBudgetReserved
/// 2. RunClaimed + RunBudgetReserved
/// 3. RunCompleted + RunBudgetSettled (always)
/// 4. CycleCancelled + CycleBudgetSettled
/// 5. CycleFailed + CycleBudgetSettled
/// 6. CycleCompleted + CycleBudgetSettled
/// 7. GlobalBudgetCapHit blocks new cycle creation
/// 8. BudgetWarningIssued is informational — no projection needed
///
/// Enforcement of co-emission rules happens at the command handler level,
/// NOT here in the projector. The projector processes events independently
/// and trusts the event stream ordering.
///
/// # Budget math
///
/// All amounts are integer cents (BudgetCents = i64). NEVER use floats for money.
pub struct BudgetProjector;

impl BudgetProjector {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BudgetProjector {
    fn default() -> Self {
        Self::new()
    }
}

/// SQL pattern for all budget ledger inserts.
///
/// Uses a subquery to atomically compute `balance_after` from the latest
/// ledger entry for the same instance. ON CONFLICT (id) DO NOTHING ensures
/// idempotency (same event_id replayed = no new row).
const INSERT_LEDGER_SQL: &str = r#"
INSERT INTO orch_budget_ledger (id, instance_id, cycle_id, run_id, event_type, amount_cents, balance_after, created_at)
SELECT $1, $2, $3, $4, $5, $6,
    COALESCE((SELECT balance_after FROM orch_budget_ledger WHERE instance_id = $2 ORDER BY created_at DESC, id DESC LIMIT 1), 0) + $6,
    $7
ON CONFLICT (id) DO NOTHING
"#;

#[async_trait::async_trait]
impl Projector for BudgetProjector {
    fn name(&self) -> &'static str {
        "BudgetProjector"
    }

    fn handles(&self) -> &[&'static str] {
        &[
            "PlanBudgetReserved",
            "RunBudgetReserved",
            "RunCostObserved",
            "RunBudgetSettled",
            "CycleBudgetSettled",
            "GlobalBudgetCapHit",
        ]
    }

    async fn handle(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        match event.event_type.as_str() {
            "PlanBudgetReserved" => {
                // Payload: {"cycle_id": UUID, "amount_cents": i64}
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
                let amount_cents: i64 = serde_json::from_value(
                    event
                        .payload
                        .get("amount_cents")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing amount_cents".into(),
                            ))
                        })?,
                )?;

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(Some(cycle_id))
                    .bind(None::<uuid::Uuid>) // no run_id for plan budget
                    .bind(&event.event_type)
                    .bind(amount_cents)
                    .bind(event.occurred_at)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunBudgetReserved" => {
                // Payload: {"cycle_id": UUID, "run_id": UUID, "amount_cents": i64}
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
                let amount_cents: i64 = serde_json::from_value(
                    event
                        .payload
                        .get("amount_cents")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing amount_cents".into(),
                            ))
                        })?,
                )?;

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(Some(cycle_id))
                    .bind(Some(run_id))
                    .bind(&event.event_type)
                    .bind(amount_cents)
                    .bind(event.occurred_at)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunCostObserved" => {
                // Payload: {"cycle_id": UUID, "run_id": UUID, "amount_cents": i64}
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
                let amount_cents: i64 = serde_json::from_value(
                    event
                        .payload
                        .get("amount_cents")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing amount_cents".into(),
                            ))
                        })?,
                )?;

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(Some(cycle_id))
                    .bind(Some(run_id))
                    .bind(&event.event_type)
                    .bind(amount_cents)
                    .bind(event.occurred_at)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "RunBudgetSettled" => {
                // Payload: {"cycle_id": UUID, "run_id": UUID, "amount_cents": i64}
                // amount_cents is negative (release) or zero
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
                let amount_cents: i64 = serde_json::from_value(
                    event
                        .payload
                        .get("amount_cents")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing amount_cents".into(),
                            ))
                        })?,
                )?;

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(Some(cycle_id))
                    .bind(Some(run_id))
                    .bind(&event.event_type)
                    .bind(amount_cents)
                    .bind(event.occurred_at)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "CycleBudgetSettled" => {
                // Payload: {"cycle_id": UUID, "amount_cents": i64}
                // amount_cents is negative (release remaining reservation)
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
                let amount_cents: i64 = serde_json::from_value(
                    event
                        .payload
                        .get("amount_cents")
                        .cloned()
                        .ok_or_else(|| {
                            AppError::Domain(crate::domain::errors::DomainError::Precondition(
                                "missing amount_cents".into(),
                            ))
                        })?,
                )?;

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(Some(cycle_id))
                    .bind(None::<uuid::Uuid>) // no run_id for cycle budget
                    .bind(&event.event_type)
                    .bind(amount_cents)
                    .bind(event.occurred_at)
                    .execute(pool)
                    .await
                    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                Ok(())
            }
            "GlobalBudgetCapHit" => {
                // Payload: {"cap_cents": i64, "current_total_cents": i64}
                // Marker entry: amount_cents = 0, no cycle_id or run_id

                sqlx::query(INSERT_LEDGER_SQL)
                    .bind(event.event_id)
                    .bind(event.instance_id)
                    .bind(None::<uuid::Uuid>) // no cycle_id
                    .bind(None::<uuid::Uuid>) // no run_id
                    .bind(&event.event_type)
                    .bind(0_i64) // marker entry
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
    use crate::domain::types::{BasisPoints, BudgetCents};

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

    /// Returns the canonical set of budget event types that the BudgetProjector must handle.
    /// These are all StateChanging events in EVENT_KINDS whose names match budget prefixes.
    fn budget_event_types() -> Vec<&'static str> {
        EVENT_KINDS
            .iter()
            .filter(|e| e.criticality == EventCriticality::StateChanging)
            .filter(|e| {
                e.event_type.starts_with("PlanBudget")
                    || e.event_type.starts_with("RunBudget")
                    || e.event_type.starts_with("RunCost")
                    || e.event_type.starts_with("CycleBudget")
                    || e.event_type.starts_with("GlobalBudget")
            })
            .map(|e| e.event_type)
            .collect()
    }

    // ── Golden test: projector covers all required state-changing events ──

    #[test]
    fn budget_projector_handles_all_budget_events() {
        let projector = BudgetProjector::new();
        let handled = projector.handles();

        let budget_events = budget_event_types();
        for event_type in &budget_events {
            assert!(
                handled.contains(event_type),
                "BudgetProjector does not handle state-changing event: {event_type}"
            );
        }
    }

    #[test]
    fn handles_returns_correct_count() {
        let projector = BudgetProjector::new();
        assert_eq!(projector.handles().len(), 6, "expected 6 budget events");
    }

    #[test]
    fn budget_warning_issued_is_not_handled() {
        let projector = BudgetProjector::new();
        let handled = projector.handles();
        assert!(
            !handled.contains(&"BudgetWarningIssued"),
            "BudgetProjector must NOT handle informational BudgetWarningIssued"
        );
    }

    #[test]
    fn no_duplicate_handles() {
        let projector = BudgetProjector::new();
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
        let projector = BudgetProjector::new();
        assert_eq!(projector.name(), "BudgetProjector");
    }

    #[test]
    fn default_impl_works() {
        let projector = BudgetProjector::default();
        assert_eq!(projector.name(), "BudgetProjector");
    }

    // ── CI coverage gate: all handled events exist in EVENT_KINDS ──

    #[test]
    fn all_handled_events_exist_in_event_kinds() {
        let projector = BudgetProjector::new();
        let known_types: Vec<&str> = EVENT_KINDS.iter().map(|e| e.event_type).collect();
        for event_type in projector.handles() {
            assert!(
                known_types.contains(event_type),
                "BudgetProjector handles unknown event: {event_type}"
            );
        }
    }

    #[test]
    fn all_handled_events_are_state_changing() {
        let projector = BudgetProjector::new();
        for event_type in projector.handles() {
            let entry = EVENT_KINDS
                .iter()
                .find(|e| e.event_type == *event_type)
                .unwrap_or_else(|| panic!("event type not in EVENT_KINDS: {event_type}"));
            assert_eq!(
                entry.criticality,
                EventCriticality::StateChanging,
                "BudgetProjector handles non-state-changing event: {event_type}"
            );
        }
    }

    // ── Coverage gate: handles() exactly matches the canonical budget event set ──

    #[test]
    fn handles_matches_canonical_budget_event_set() {
        let projector = BudgetProjector::new();
        let handled: std::collections::HashSet<&&str> = projector.handles().iter().collect();
        let canonical: std::collections::HashSet<&str> = budget_event_types().into_iter().collect();

        // Every canonical budget event must be handled
        for event_type in &canonical {
            assert!(
                handled.contains(event_type),
                "BudgetProjector missing canonical budget event: {event_type}"
            );
        }

        // Every handled event must be in the canonical set
        for event_type in &handled {
            assert!(
                canonical.contains(**event_type),
                "BudgetProjector handles extra event not in canonical set: {event_type}"
            );
        }
    }

    #[test]
    fn budget_warning_issued_exists_as_informational() {
        let entry = EVENT_KINDS
            .iter()
            .find(|e| e.event_type == "BudgetWarningIssued")
            .expect("BudgetWarningIssued must exist in EVENT_KINDS");
        assert_eq!(
            entry.criticality,
            EventCriticality::Informational,
            "BudgetWarningIssued must be classified as Informational"
        );
    }

    // ── Budget math compile-time checks ──

    #[test]
    fn budget_cents_is_i64() {
        // Compile-time proof: BudgetCents is i64
        let _amount: BudgetCents = -500_i64;
        let _zero: BudgetCents = 0_i64;
        let _positive: BudgetCents = 100_000_i64;
    }

    #[test]
    fn basis_points_is_u16() {
        // Compile-time proof: BasisPoints is u16
        let _full: BasisPoints = 10_000_u16;
        let _half: BasisPoints = 5_000_u16;
        let _zero: BasisPoints = 0_u16;
    }

    #[test]
    fn budget_math_is_integer_only() {
        // Verify budget amounts are integer-only (no float contamination)
        let reservation: BudgetCents = 10_000; // $100.00
        let actual_cost: BudgetCents = 7_500; // $75.00
        let settlement: BudgetCents = -(reservation - actual_cost); // -$25.00 (release)

        assert_eq!(settlement, -2_500);
        assert_eq!(reservation + settlement, actual_cost);

        // Running balance: reserve, spend, settle
        let balance_after_reserve: BudgetCents = 0 + reservation;
        assert_eq!(balance_after_reserve, 10_000);

        let balance_after_cost: BudgetCents = balance_after_reserve + actual_cost;
        assert_eq!(balance_after_cost, 17_500);

        let balance_after_settle: BudgetCents = balance_after_cost + settlement;
        assert_eq!(balance_after_settle, 15_000);
    }

    #[test]
    fn settlement_releases_correct_amounts() {
        // Scenario: reserve 500, actual cost 300, settlement should release -200
        let reserved: BudgetCents = 500;
        let actual: BudgetCents = 300;
        let release: BudgetCents = -(reserved - actual);
        assert_eq!(release, -200);

        // Scenario: reserve 1000, actual cost 1000, settlement releases 0
        let reserved2: BudgetCents = 1000;
        let actual2: BudgetCents = 1000;
        let release2: BudgetCents = -(reserved2 - actual2);
        assert_eq!(release2, 0);

        // Scenario: reserve 0, actual cost 0, settlement releases 0
        let release3: BudgetCents = -(0 - 0);
        assert_eq!(release3, 0);
    }

    // ── Fixture tests ──

    #[test]
    fn fixture_creates_valid_budget_envelope() {
        let instance_id = uuid::Uuid::new_v4();
        let cycle_id = uuid::Uuid::new_v4();
        let payload = serde_json::json!({
            "cycle_id": cycle_id,
            "amount_cents": 5000_i64,
        });
        let event = fixture(instance_id, "PlanBudgetReserved", payload);
        assert_eq!(event.instance_id, instance_id);
        assert_eq!(event.event_type, "PlanBudgetReserved");
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
        let e1 = fixture(instance_id, "PlanBudgetReserved", payload.clone());
        let e2 = fixture(instance_id, "PlanBudgetReserved", payload);
        assert_ne!(
            e1.event_id, e2.event_id,
            "each fixture should get a unique event_id"
        );
    }
}
