//! End-to-end lifecycle integration test for the OpenClaw orchestrator.
//!
//! Drives the full lifecycle through events and projections:
//!   instance create -> provision -> cycle create -> plan -> approve -> budget reserve
//!   -> schedule tasks -> scheduler claims runs -> worker start -> worker complete
//!   -> budget settle per-run -> task pass -> cycle completing -> cycle completed
//!   -> budget settle cycle -> verify final state
//!
//! Requires a running Postgres database with migrations applied.
//! Run with:
//!   DATABASE_URL=postgres://... cargo test -p openclaw-orchestrator --test lifecycle_integration -- --ignored

use openclaw_orchestrator::app::budget_projector::BudgetProjector;
use openclaw_orchestrator::app::cycle_projector::CycleProjector;
use openclaw_orchestrator::app::errors::AppError;
use openclaw_orchestrator::app::instance_projector::InstanceProjector;
use openclaw_orchestrator::app::projector::Projector;
use openclaw_orchestrator::app::run_projector::RunProjector;
use openclaw_orchestrator::app::scheduler::Scheduler;
use openclaw_orchestrator::app::task_projector::TaskProjector;
use openclaw_orchestrator::domain::events::EventEnvelope;
use openclaw_orchestrator::domain::ports::EventStore;
use openclaw_orchestrator::infra::event_store::PgEventStore;

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Routes events to the correct projector(s).
struct ProjectorDispatcher {
    projectors: Vec<Box<dyn Projector>>,
}

impl ProjectorDispatcher {
    fn new() -> Self {
        Self {
            projectors: vec![
                Box::new(InstanceProjector::new()),
                Box::new(CycleProjector::new()),
                Box::new(TaskProjector::new()),
                Box::new(RunProjector::new()),
                Box::new(BudgetProjector::new()),
            ],
        }
    }

    async fn dispatch(&self, event: &EventEnvelope, pool: &PgPool) -> Result<(), AppError> {
        for projector in &self.projectors {
            if projector.handles().contains(&event.event_type.as_str()) {
                projector.handle(event, pool).await?;
            }
        }
        Ok(())
    }
}

/// Emit an event via the event store, then dispatch it to all matching projectors.
async fn emit_and_project(
    event_store: &PgEventStore,
    dispatcher: &ProjectorDispatcher,
    pool: &PgPool,
    event: EventEnvelope,
) -> Result<i64, Box<dyn std::error::Error>> {
    let seq = event_store.emit(event.clone()).await?;
    let mut projected = event;
    projected.seq = seq;
    dispatcher.dispatch(&projected, pool).await?;
    Ok(seq)
}

/// Build an EventEnvelope with sensible defaults. The `seq` field is set to 0;
/// the actual sequence number is assigned by the event store on emit.
fn make_event(instance_id: Uuid, event_type: &str, payload: serde_json::Value) -> EventEnvelope {
    let now = Utc::now();
    EventEnvelope {
        event_id: Uuid::new_v4(),
        instance_id,
        seq: 0,
        event_type: event_type.to_string(),
        event_version: 1,
        payload,
        idempotency_key: None,
        correlation_id: None,
        causation_id: None,
        occurred_at: now,
        recorded_at: now,
    }
}

// ── The Test ─────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore] // Requires DATABASE_URL pointing at a Postgres database with migrations applied
async fn test_full_lifecycle() {
    // ── Setup ────────────────────────────────────────────────────────────
    let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPool::connect(&db_url).await.expect("failed to connect to database");

    let event_store = PgEventStore::new(pool.clone());
    let dispatcher = ProjectorDispatcher::new();
    let scheduler = Scheduler::new(pool.clone());

    // All IDs are unique per test run -- no cleanup needed for append-only tables.
    let instance_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let cycle_id = Uuid::new_v4();

    // ─── Phase 1: Create project ─────────────────────────────────────────
    sqlx::query("INSERT INTO orch_projects (id, name, repo_path) VALUES ($1, $2, $3)")
        .bind(project_id)
        .bind(format!("lifecycle-test-{}", &instance_id.to_string()[..8]))
        .bind("/tmp/lifecycle-test-repo")
        .execute(&pool)
        .await
        .expect("insert project");

    // ─── Phase 2: Create and provision instance ──────────────────────────
    let event = make_event(
        instance_id,
        "InstanceCreated",
        json!({
            "project_id": project_id,
            "name": format!("instance-{}", &instance_id.to_string()[..8]),
            "data_dir": "/tmp/lifecycle-test-data",
            "token_hash": "test-hash-lifecycle"
        }),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("InstanceCreated");

    // Verify instance is provisioning
    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query instance state");
    assert_eq!(state, "provisioning", "instance should start in provisioning state");

    let event = make_event(instance_id, "InstanceProvisioned", json!({}));
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("InstanceProvisioned");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query instance state");
    assert_eq!(state, "active", "instance should be active after provisioning");

    // Set up server capacity for the scheduler
    sqlx::query(
        "INSERT INTO orch_server_capacity (instance_id, active_runs, max_concurrent) VALUES ($1, 0, 3)",
    )
    .bind(instance_id)
    .execute(&pool)
    .await
    .expect("insert server capacity");

    // ─── Phase 3: Create cycle and generate plan ─────────────────────────
    let event = make_event(
        instance_id,
        "CycleCreated",
        json!({
            "cycle_id": cycle_id,
            "prompt": "Implement user authentication"
        }),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("CycleCreated");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "created");

    // Request plan
    let event = make_event(
        instance_id,
        "PlanRequested",
        json!({"cycle_id": cycle_id}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("PlanRequested");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "planning");

    // Generate plan
    let plan = json!({
        "phases": [{
            "name": "Phase 1",
            "tasks": [
                {"key": "task-1", "title": "Write auth module", "description": "Implement JWT auth"},
                {"key": "task-2", "title": "Write tests", "description": "Add auth tests"}
            ]
        }]
    });
    let event = make_event(
        instance_id,
        "PlanGenerated",
        json!({"cycle_id": cycle_id, "plan": plan}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("PlanGenerated");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "plan_ready");

    // Approve plan (co-emitted with PlanBudgetReserved in production)
    let event = make_event(
        instance_id,
        "PlanApproved",
        json!({"cycle_id": cycle_id}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("PlanApproved");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "approved");

    // Reserve plan budget (co-emitted with PlanApproved)
    let event = make_event(
        instance_id,
        "PlanBudgetReserved",
        json!({"cycle_id": cycle_id, "amount_cents": 5000}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("PlanBudgetReserved");

    // Verify budget ledger entry
    let ledger_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM orch_budget_ledger WHERE instance_id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query budget ledger count");
    assert_eq!(ledger_count, 1, "should have 1 budget ledger entry after plan reservation");

    let balance: i64 = sqlx::query_scalar(
        "SELECT balance_after FROM orch_budget_ledger WHERE instance_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(instance_id)
    .fetch_one(&pool)
    .await
    .expect("query budget balance");
    assert_eq!(balance, 5000, "balance after plan reservation should be 5000 cents");

    // ─── Phase 4: Schedule tasks ─────────────────────────────────────────
    let task1_id = Uuid::new_v4();
    let task2_id = Uuid::new_v4();

    let event = make_event(
        instance_id,
        "TaskScheduled",
        json!({
            "task_id": task1_id,
            "cycle_id": cycle_id,
            "task_key": "task-1",
            "phase": "Phase 1",
            "ordinal": 1,
            "title": "Write auth module",
            "description": "Implement JWT auth",
            "acceptance": {"tests_pass": true},
            "max_retries": 3
        }),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("TaskScheduled task-1");

    let event = make_event(
        instance_id,
        "TaskScheduled",
        json!({
            "task_id": task2_id,
            "cycle_id": cycle_id,
            "task_key": "task-2",
            "phase": "Phase 1",
            "ordinal": 2,
            "title": "Write tests",
            "description": "Add auth tests",
            "acceptance": {"coverage": ">80%"},
            "max_retries": 3
        }),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("TaskScheduled task-2");

    // Verify tasks are 'scheduled'
    let task_states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM orch_tasks WHERE cycle_id = $1 ORDER BY ordinal",
    )
    .bind(cycle_id)
    .fetch_all(&pool)
    .await
    .expect("query task states");
    assert_eq!(task_states, vec!["scheduled", "scheduled"]);

    // ─── Phase 5: Scheduler claims tasks ─────────────────────────────────
    // Verify cycle is 'approved' before tick
    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state before tick");
    assert_eq!(state, "approved");

    let claims = scheduler.tick(instance_id).await.expect("scheduler tick");
    assert_eq!(claims.len(), 2, "scheduler should claim both tasks");

    // Verify scheduler set cycle to 'running'
    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state after tick");
    assert_eq!(state, "running", "cycle should be running after scheduler tick");

    // Verify tasks are 'active'
    let task1_state: String =
        sqlx::query_scalar("SELECT state FROM orch_tasks WHERE id = $1")
            .bind(task1_id)
            .fetch_one(&pool)
            .await
            .expect("query task1 state");
    assert_eq!(task1_state, "active", "task1 should be active after scheduler claim");

    let task2_state: String =
        sqlx::query_scalar("SELECT state FROM orch_tasks WHERE id = $1")
            .bind(task2_id)
            .fetch_one(&pool)
            .await
            .expect("query task2 state");
    assert_eq!(task2_state, "active", "task2 should be active after scheduler claim");

    // Verify runs exist in 'claimed' state
    for claim in &claims {
        let run_state: String =
            sqlx::query_scalar("SELECT state FROM orch_runs WHERE id = $1")
                .bind(claim.run_id)
                .fetch_one(&pool)
                .await
                .expect("query run state");
        assert_eq!(run_state, "claimed", "run should be in claimed state");
    }

    // Verify server capacity was incremented
    let active_runs: i32 =
        sqlx::query_scalar("SELECT active_runs FROM orch_server_capacity WHERE instance_id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query active runs");
    assert_eq!(active_runs, 2, "server capacity should show 2 active runs");

    // Verify claim results map to our task IDs
    let claimed_task_ids: Vec<Uuid> = claims.iter().map(|c| c.task_id).collect();
    assert!(
        claimed_task_ids.contains(&task1_id) && claimed_task_ids.contains(&task2_id),
        "claims should reference both task IDs"
    );

    // ─── Phase 6: Simulate worker execution ──────────────────────────────
    // Workers send proof-of-life (RunStarted) then complete the run
    for claim in &claims {
        // RunStarted: claimed -> running
        let event = make_event(
            instance_id,
            "RunStarted",
            json!({"run_id": claim.run_id}),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("RunStarted");

        // Verify run is now 'running'
        let run_state: String =
            sqlx::query_scalar("SELECT state FROM orch_runs WHERE id = $1")
                .bind(claim.run_id)
                .fetch_one(&pool)
                .await
                .expect("query run state after start");
        assert_eq!(run_state, "running");

        // Reserve run budget
        let event = make_event(
            instance_id,
            "RunBudgetReserved",
            json!({
                "cycle_id": cycle_id,
                "run_id": claim.run_id,
                "amount_cents": 1500
            }),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("RunBudgetReserved");
    }

    // RunCompleted for both
    for claim in &claims {
        let event = make_event(
            instance_id,
            "RunCompleted",
            json!({
                "run_id": claim.run_id,
                "exit_code": 0,
                "output_json": {"result": "success"},
                "cost_cents": 1200
            }),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("RunCompleted");

        // Verify run is now 'completed'
        let run_state: String =
            sqlx::query_scalar("SELECT state FROM orch_runs WHERE id = $1")
                .bind(claim.run_id)
                .fetch_one(&pool)
                .await
                .expect("query run state after completion");
        assert_eq!(run_state, "completed");

        // Settle run budget (release unused reservation: reserved 1500 - actual 1200 = release 300)
        let event = make_event(
            instance_id,
            "RunBudgetSettled",
            json!({
                "cycle_id": cycle_id,
                "run_id": claim.run_id,
                "amount_cents": -300
            }),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("RunBudgetSettled");

        // Record observed cost
        let event = make_event(
            instance_id,
            "RunCostObserved",
            json!({
                "cycle_id": cycle_id,
                "run_id": claim.run_id,
                "amount_cents": 1200
            }),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("RunCostObserved");
    }

    // ─── Phase 7: Tasks pass verification ────────────────────────────────
    // The domain state machine requires: active -> verifying -> passed
    // The active -> verifying transition is triggered by RunCompleted at the
    // command handler level (not yet built as a projector). We simulate this
    // by directly updating the task state to 'verifying' before emitting TaskPassed.
    for claim in &claims {
        sqlx::query("UPDATE orch_tasks SET state = 'verifying' WHERE id = $1 AND state = 'active'")
            .bind(claim.task_id)
            .execute(&pool)
            .await
            .expect("transition task to verifying");

        let event = make_event(
            instance_id,
            "TaskPassed",
            json!({
                "task_id": claim.task_id,
                "evidence_run_id": claim.run_id
            }),
        );
        emit_and_project(&event_store, &dispatcher, &pool, event)
            .await
            .expect("TaskPassed");
    }

    // Verify all tasks are 'passed'
    let task_states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM orch_tasks WHERE cycle_id = $1 ORDER BY ordinal",
    )
    .bind(cycle_id)
    .fetch_all(&pool)
    .await
    .expect("query task states after pass");
    assert_eq!(task_states, vec!["passed", "passed"]);

    // ─── Phase 8: Cycle completes ────────────────────────────────────────
    let event = make_event(
        instance_id,
        "CycleCompleting",
        json!({"cycle_id": cycle_id}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("CycleCompleting");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "completing");

    let event = make_event(
        instance_id,
        "CycleCompleted",
        json!({"cycle_id": cycle_id}),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("CycleCompleted");

    let state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query cycle state");
    assert_eq!(state, "completed");

    // Settle cycle budget (release remaining reservation)
    // Plan reserved 5000, runs reserved 2*1500=3000, settled 2*(-300)=-600, cost 2*1200=2400
    // Net budget used: 5000 + 3000 - 600 + 2400 = 9800
    // Remaining plan reservation to release: -(5000 - 2*1200) = -2600
    let event = make_event(
        instance_id,
        "CycleBudgetSettled",
        json!({
            "cycle_id": cycle_id,
            "amount_cents": -2600
        }),
    );
    emit_and_project(&event_store, &dispatcher, &pool, event)
        .await
        .expect("CycleBudgetSettled");

    // ─── Final Verification ──────────────────────────────────────────────

    // 1. Cycle is completed
    let final_cycle_state: String =
        sqlx::query_scalar("SELECT state FROM orch_cycles WHERE id = $1")
            .bind(cycle_id)
            .fetch_one(&pool)
            .await
            .expect("query final cycle state");
    assert_eq!(final_cycle_state, "completed", "cycle should be completed");

    // 2. All tasks are passed
    let final_task_states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM orch_tasks WHERE cycle_id = $1 ORDER BY ordinal",
    )
    .bind(cycle_id)
    .fetch_all(&pool)
    .await
    .expect("query final task states");
    assert_eq!(
        final_task_states,
        vec!["passed", "passed"],
        "all tasks should be passed"
    );

    // 3. All runs are completed
    for claim in &claims {
        let final_run_state: String =
            sqlx::query_scalar("SELECT state FROM orch_runs WHERE id = $1")
                .bind(claim.run_id)
                .fetch_one(&pool)
                .await
                .expect("query final run state");
        assert_eq!(final_run_state, "completed", "all runs should be completed");
    }

    // 4. Budget ledger has expected entries
    // Expected entries: PlanBudgetReserved(1) + RunBudgetReserved(2) + RunBudgetSettled(2)
    //                  + RunCostObserved(2) + CycleBudgetSettled(1) = 8
    let ledger_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM orch_budget_ledger WHERE instance_id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query budget ledger count");
    assert_eq!(ledger_count, 8, "budget ledger should have 8 entries");

    // 5. Final budget balance
    let final_balance: i64 = sqlx::query_scalar(
        "SELECT balance_after FROM orch_budget_ledger WHERE instance_id = $1 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(instance_id)
    .fetch_one(&pool)
    .await
    .expect("query final budget balance");
    // 5000 (plan) + 1500 + 1500 (run reserves) + (-300) + (-300) (run settles)
    // + 1200 + 1200 (run costs) + (-2600) (cycle settle) = 7200
    assert_eq!(
        final_balance, 7200,
        "final budget balance should reflect all budget operations"
    );

    // 6. Event replay produces all lifecycle events
    let events = event_store
        .replay(instance_id, 0)
        .await
        .expect("replay events");
    // Manual events: InstanceCreated(1) + InstanceProvisioned(1) + CycleCreated(1)
    //   + PlanRequested(1) + PlanGenerated(1) + PlanApproved(1) + PlanBudgetReserved(1)
    //   + TaskScheduled(2) + RunStarted(2) + RunBudgetReserved(2) + RunCompleted(2)
    //   + RunBudgetSettled(2) + RunCostObserved(2) + TaskPassed(2) + CycleCompleting(1)
    //   + CycleCompleted(1) + CycleBudgetSettled(1) = 24
    // Scheduler events: CycleRunningStarted(1) + RunClaimed(2) = 3
    // Total: 27
    assert!(
        events.len() >= 25,
        "should have at least 25 events for full lifecycle, got {}",
        events.len()
    );

    // 7. Instance is still active (not affected by cycle completion)
    let instance_state: String =
        sqlx::query_scalar("SELECT state FROM orch_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&pool)
            .await
            .expect("query instance state");
    assert_eq!(instance_state, "active", "instance should remain active after cycle completes");

    // 8. Verify event ordering: events are monotonically increasing by seq
    let seqs: Vec<i64> =
        sqlx::query_scalar("SELECT seq FROM orch_events WHERE instance_id = $1 ORDER BY seq ASC")
            .bind(instance_id)
            .fetch_all(&pool)
            .await
            .expect("query event seqs");
    for window in seqs.windows(2) {
        assert!(
            window[1] > window[0],
            "event sequence numbers must be strictly increasing: {} vs {}",
            window[0],
            window[1]
        );
    }

    // 9. Verify scheduler events are interspersed correctly
    let scheduler_events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM orch_events WHERE instance_id = $1 AND event_type IN ('CycleRunningStarted', 'RunClaimed') ORDER BY seq",
    )
    .bind(instance_id)
    .fetch_all(&pool)
    .await
    .expect("query scheduler events");
    assert!(
        scheduler_events.contains(&"CycleRunningStarted".to_string()),
        "scheduler should have emitted CycleRunningStarted"
    );
    assert_eq!(
        scheduler_events
            .iter()
            .filter(|e| *e == "RunClaimed")
            .count(),
        2,
        "scheduler should have emitted 2 RunClaimed events"
    );

    // No cleanup: unique UUIDs per test run, append-only events table has delete triggers.
    // All test data is isolated by instance_id.
}
