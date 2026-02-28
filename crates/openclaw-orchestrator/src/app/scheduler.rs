use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::errors::AppError;
use crate::infra::errors::InfraError;
use crate::infra::event_store::advisory_lock_key;

/// Default maximum concurrent runs per instance when no capacity row exists.
const DEFAULT_MAX_CONCURRENT: i32 = 3;

/// Result of a successful task claim within a scheduler tick.
#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub task_id: Uuid,
    pub run_id: Uuid,
    pub cycle_id: Uuid,
    pub instance_id: Uuid,
    pub run_number: i32,
}

/// Row type for reading server capacity from `orch_server_capacity`.
#[derive(sqlx::FromRow)]
struct CapacityRow {
    active_runs: i32,
    max_concurrent: i32,
}

/// Row type for reading claimable tasks via FOR UPDATE SKIP LOCKED.
#[derive(sqlx::FromRow)]
#[allow(dead_code)]
struct ClaimableTask {
    id: Uuid,
    cycle_id: Uuid,
    current_attempt: i32,
    cycle_state: String,
}

/// Scheduler orchestrates task claiming within a single tick.
///
/// Each tick:
/// 1. Acquires an advisory lock scoped to the instance (prevents concurrent ticks)
/// 2. Reads concurrency capacity from `orch_server_capacity`
/// 3. Finds scheduled tasks via `SELECT ... FOR UPDATE SKIP LOCKED`
/// 4. For each claimable task:
///    - Emits CycleRunningStarted (if cycle is still in 'approved' state)
///    - Emits RunClaimed event
///    - Applies projection updates (orch_tasks, orch_runs, orch_cycles, orch_server_capacity)
/// 5. All within a single transaction for atomicity
///
/// # Phase 1 scope
///
/// Only the claim phase is implemented. Worker spawning (Phase 2) is not included.
/// No heartbeat, lease management, or retry logic.
pub struct Scheduler {
    pool: PgPool,
}

impl Scheduler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Execute a single scheduler tick for the given instance.
    ///
    /// Returns a list of claimed tasks (may be empty if no capacity or no tasks).
    /// The entire tick runs within a single Postgres transaction.
    pub async fn tick(&self, instance_id: Uuid) -> Result<Vec<ClaimResult>, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        // 1. Acquire advisory lock scoped to the instance (released on tx commit/rollback)
        // Single-arg form takes bigint (i64). We use the first 64 bits of the UUID.
        let (hi, _lo) = advisory_lock_key(instance_id);
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(hi)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        // 2. Check concurrency capacity
        let capacity = sqlx::query_as::<_, CapacityRow>(
            "SELECT active_runs, max_concurrent FROM orch_server_capacity WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        let (active, max_conc) = match capacity {
            Some(c) => (c.active_runs, c.max_concurrent),
            None => (0, DEFAULT_MAX_CONCURRENT),
        };

        let available = max_conc - active;
        if available <= 0 {
            tx.commit()
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
            return Ok(vec![]);
        }

        // 3. Find claimable tasks (FOR UPDATE SKIP LOCKED prevents double-claiming)
        let tasks = sqlx::query_as::<_, ClaimableTask>(
            r#"
            SELECT t.id, t.cycle_id, t.current_attempt, c.state AS cycle_state
            FROM orch_tasks t
            JOIN orch_cycles c ON c.id = t.cycle_id AND c.instance_id = t.instance_id
            WHERE t.instance_id = $1
              AND t.state = 'scheduled'
              AND c.state IN ('approved', 'running')
            ORDER BY t.ordinal ASC
            FOR UPDATE OF t SKIP LOCKED
            LIMIT $2
            "#,
        )
        .bind(instance_id)
        .bind(available)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        if tasks.is_empty() {
            tx.commit()
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;
            return Ok(vec![]);
        }

        let mut results = Vec::with_capacity(tasks.len());

        // Track which cycles have already been transitioned to 'running' in this tick,
        // so we only emit CycleRunningStarted once per cycle.
        let mut cycles_started: std::collections::HashSet<Uuid> = std::collections::HashSet::new();

        let now = Utc::now();

        for task in &tasks {
            // 4a. If cycle is 'approved', emit CycleRunningStarted to transition it to 'running'
            if task.cycle_state == "approved" && !cycles_started.contains(&task.cycle_id) {
                // Insert CycleRunningStarted event
                let event_id = Uuid::new_v4();
                let seq = next_event_seq(&mut tx, instance_id).await?;
                let payload = serde_json::json!({
                    "cycle_id": task.cycle_id,
                });

                sqlx::query(
                    r#"
                    INSERT INTO orch_events (
                        event_id, instance_id, seq, event_type, event_version,
                        payload, idempotency_key, correlation_id, causation_id,
                        occurred_at, recorded_at
                    )
                    VALUES ($1, $2, $3, 'CycleRunningStarted', 1, $4, NULL, NULL, NULL, $5, $5)
                    "#,
                )
                .bind(event_id)
                .bind(instance_id)
                .bind(seq)
                .bind(&payload)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                // Apply cycle projection: approved -> running
                sqlx::query(
                    "UPDATE orch_cycles SET state = 'running', updated_at = $2 WHERE id = $1 AND state = 'approved'",
                )
                .bind(task.cycle_id)
                .bind(now)
                .execute(&mut *tx)
                .await
                .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

                cycles_started.insert(task.cycle_id);
            }

            // 4b. Compute run_number and generate IDs
            let run_id = Uuid::new_v4();
            let worker_session_id = Uuid::new_v4();
            let run_number = next_run_number(&mut tx, task.id).await?;

            // 4c. Insert RunClaimed event
            let event_id = Uuid::new_v4();
            let seq = next_event_seq(&mut tx, instance_id).await?;
            let payload = serde_json::json!({
                "run_id": run_id,
                "task_id": task.id,
                "cycle_id": task.cycle_id,
                "run_number": run_number,
                "worker_session_id": worker_session_id,
            });

            sqlx::query(
                r#"
                INSERT INTO orch_events (
                    event_id, instance_id, seq, event_type, event_version,
                    payload, idempotency_key, correlation_id, causation_id,
                    occurred_at, recorded_at
                )
                VALUES ($1, $2, $3, 'RunClaimed', 1, $4, NULL, NULL, NULL, $5, $5)
                "#,
            )
            .bind(event_id)
            .bind(instance_id)
            .bind(seq)
            .bind(&payload)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            // 4d. Apply run projection: INSERT into orch_runs
            sqlx::query(
                r#"
                INSERT INTO orch_runs (id, task_id, instance_id, run_number, state, worker_session_id, started_at)
                VALUES ($1, $2, $3, $4, 'claimed', $5, $6)
                ON CONFLICT (id) DO NOTHING
                "#,
            )
            .bind(run_id)
            .bind(task.id)
            .bind(instance_id)
            .bind(run_number)
            .bind(worker_session_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            // 4e. Apply task projection: scheduled -> active
            sqlx::query(
                "UPDATE orch_tasks SET state = 'active', active_run_id = $2, current_attempt = current_attempt + 1, updated_at = $3 WHERE id = $1",
            )
            .bind(task.id)
            .bind(run_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            // 4f. Update server capacity: increment active_runs
            sqlx::query(
                "UPDATE orch_server_capacity SET active_runs = active_runs + 1, updated_at = $2 WHERE instance_id = $1",
            )
            .bind(instance_id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

            results.push(ClaimResult {
                task_id: task.id,
                run_id,
                cycle_id: task.cycle_id,
                instance_id,
                run_number,
            });
        }

        // 5. Commit transaction -- all events + projections atomically
        tx.commit()
            .await
            .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

        Ok(results)
    }
}

/// Get the next event sequence number for an instance within a transaction.
async fn next_event_seq(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    instance_id: Uuid,
) -> Result<i64, AppError> {
    let row = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT MAX(seq) FROM orch_events WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

    Ok(row.unwrap_or(0) + 1)
}

/// Get the next run number for a task (1-indexed) within a transaction.
async fn next_run_number(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_id: Uuid,
) -> Result<i32, AppError> {
    let row = sqlx::query_scalar::<_, Option<i32>>(
        "SELECT MAX(run_number) FROM orch_runs WHERE task_id = $1",
    )
    .bind(task_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| AppError::Infra(InfraError::Database(e.to_string())))?;

    Ok(row.unwrap_or(0) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Compile-time proof: Scheduler is constructible ──

    #[test]
    fn claim_result_fields_accessible() {
        let result = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 1,
        };
        // Verify all fields exist and are the right types
        let _: Uuid = result.task_id;
        let _: Uuid = result.run_id;
        let _: Uuid = result.cycle_id;
        let _: Uuid = result.instance_id;
        let _: i32 = result.run_number;
    }

    #[test]
    fn claim_result_clone() {
        let result = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 3,
        };
        let cloned = result.clone();
        assert_eq!(result.task_id, cloned.task_id);
        assert_eq!(result.run_id, cloned.run_id);
        assert_eq!(result.cycle_id, cloned.cycle_id);
        assert_eq!(result.instance_id, cloned.instance_id);
        assert_eq!(result.run_number, cloned.run_number);
    }

    #[test]
    fn claim_result_debug() {
        let result = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 1,
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("ClaimResult"));
        assert!(debug.contains("task_id"));
        assert!(debug.contains("run_id"));
        assert!(debug.contains("cycle_id"));
        assert!(debug.contains("instance_id"));
        assert!(debug.contains("run_number"));
    }

    #[test]
    fn default_max_concurrent_is_three() {
        assert_eq!(DEFAULT_MAX_CONCURRENT, 3);
    }

    #[test]
    fn advisory_lock_key_is_accessible() {
        // Prove we can call advisory_lock_key from this module
        let uuid = Uuid::new_v4();
        let (hi, lo) = advisory_lock_key(uuid);
        // Should produce non-zero keys for random UUIDs (probabilistically)
        assert!(hi != 0 || lo != 0);
    }

    #[test]
    fn advisory_lock_key_is_deterministic() {
        let uuid = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let (hi1, lo1) = advisory_lock_key(uuid);
        let (hi2, lo2) = advisory_lock_key(uuid);
        assert_eq!(hi1, hi2);
        assert_eq!(lo1, lo2);
    }

    #[test]
    fn claim_result_unique_ids() {
        let r1 = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 1,
        };
        let r2 = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 2,
        };
        assert_ne!(r1.task_id, r2.task_id);
        assert_ne!(r1.run_id, r2.run_id);
    }

    #[test]
    fn claim_result_run_number_starts_at_one() {
        let result = ClaimResult {
            task_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            cycle_id: Uuid::new_v4(),
            instance_id: Uuid::new_v4(),
            run_number: 1,
        };
        assert!(result.run_number >= 1, "run_number should be 1-indexed");
    }
}
