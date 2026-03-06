//! Event dispatcher — routes domain events to handlers.
//!
//! Subscribes to the event broadcast channel and handles:
//! - `PlanRequested` → triggers plan generation via ClaudeCodePlanner
//! - Run completion notifications → emits RunCompleted/RunFailed events + projections
//!
//! Task scheduling after plan approval is handled inline in the approve_plan route.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use sha2::Digest;
use sqlx::PgPool;
use uuid::Uuid;

use openclaw_orchestrator::app::planner::PlannerService;
use openclaw_orchestrator::domain::events::EventEnvelope;
use openclaw_orchestrator::domain::planner::{
    PlanConstraints, PlanningContext, RepoContext,
};
use openclaw_orchestrator::domain::ports::EventStore;
use openclaw_orchestrator::infra::worker::WorkerExitNotification;

/// Maximum number of event_ids to track for deduplication.
/// Events older than this window are forgotten — acceptable because
/// duplicates arrive within milliseconds of each other.
const DEDUP_WINDOW: usize = 256;

/// Spawn the event dispatcher background task.
///
/// Subscribes to the event broadcast and handles PlanRequested events
/// by generating plans via the PlannerService.
pub fn spawn_event_dispatcher(
    pool: PgPool,
    event_store: Arc<dyn EventStore>,
    planner_service: Arc<PlannerService>,
    mut event_rx: tokio::sync::broadcast::Receiver<EventEnvelope>,
    instance_id: Uuid,
) {
    tokio::spawn(async move {
        tracing::info!("event dispatcher started");
        // Deduplicate events — they arrive twice (in-process broadcast + LISTEN/NOTIFY replay)
        let mut seen: HashSet<Uuid> = HashSet::new();
        let mut seen_order: Vec<Uuid> = Vec::new();
        loop {
            match event_rx.recv().await {
                Ok(event) => {
                    // Dedup by event_id
                    if !seen.insert(event.event_id) {
                        tracing::debug!(event_id = %event.event_id, event_type = %event.event_type, "duplicate event skipped");
                        continue;
                    }
                    seen_order.push(event.event_id);
                    // Evict oldest when window is full
                    if seen_order.len() > DEDUP_WINDOW {
                        let old = seen_order.remove(0);
                        seen.remove(&old);
                    }
                    handle_event(&pool, &event_store, &planner_service, &event, event.instance_id).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "event dispatcher lagged, skipping events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::info!("event dispatcher: broadcast closed, shutting down");
                    break;
                }
            }
        }
    });
}

/// Handle a single event.
async fn handle_event(
    pool: &PgPool,
    _event_store: &Arc<dyn EventStore>,
    planner_service: &Arc<PlannerService>,
    event: &EventEnvelope,
    instance_id: Uuid,
) {
    match event.event_type.as_str() {
        "PlanRequested" => {
            handle_plan_requested(pool, planner_service, event, instance_id).await;
        }
        _ => {
            // Most events don't need dispatcher handling
        }
    }
}

/// Handle PlanRequested: generate a plan via Claude CLI.
async fn handle_plan_requested(
    pool: &PgPool,
    planner_service: &Arc<PlannerService>,
    event: &EventEnvelope,
    instance_id: Uuid,
) {
    let cycle_id = match event.payload.get("cycle_id").and_then(|v| v.as_str()) {
        Some(id) => match Uuid::parse_str(id) {
            Ok(uuid) => uuid,
            Err(_) => {
                tracing::error!("PlanRequested: invalid cycle_id");
                return;
            }
        },
        None => {
            tracing::error!("PlanRequested: missing cycle_id");
            return;
        }
    };

    let objective = event
        .payload
        .get("objective")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if objective.is_empty() {
        tracing::error!(cycle_id = %cycle_id, "PlanRequested: empty objective");
        return;
    }

    // Read project info for context
    let project_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT project_id FROM orch_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(Uuid::nil());

    // Build a minimal planning context
    let context = PlanningContext {
        cycle_id,
        project_id,
        objective: objective.clone(),
        repo_context: RepoContext {
            file_tree_summary: String::new(),
            recent_commits: vec![],
            relevant_files: vec![],
        },
        constraints: PlanConstraints {
            max_tasks: 10,
            max_concurrent: 3,
            budget_remaining: 100_000, // $1000 default
            forbidden_paths: vec![],
        },
        previous_cycle_summary: None,
        context_hash: format!("sha256-{}", hex::encode(sha2::Sha256::digest(objective.as_bytes()))),
    };

    tracing::info!(
        cycle_id = %cycle_id,
        instance_id = %instance_id,
        "dispatching plan generation"
    );

    // Clone for the spawned task
    let planner_svc = planner_service.clone();
    let pool = pool.clone();

    tokio::spawn(async move {
        match planner_svc.generate_plan_core(instance_id, cycle_id, &context, 1).await {
            Ok(proposal) => {
                tracing::info!(
                    cycle_id = %cycle_id,
                    tasks = proposal.tasks.len(),
                    "plan generated successfully"
                );

                // Update orch_cycles with the plan and set state to plan_ready
                let plan_json = serde_json::to_value(&proposal).unwrap_or_default();
                if let Err(e) = sqlx::query(
                    "UPDATE orch_cycles SET plan = $1, state = 'plan_ready', updated_at = $2 WHERE id = $3 AND instance_id = $4",
                )
                .bind(&plan_json)
                .bind(Utc::now())
                .bind(cycle_id)
                .bind(instance_id)
                .execute(&pool)
                .await
                {
                    tracing::error!(cycle_id = %cycle_id, error = %e, "failed to update cycle with plan");
                }
            }
            Err(e) => {
                tracing::error!(
                    cycle_id = %cycle_id,
                    error = %e,
                    "plan generation failed"
                );

                // Update cycle state to failed
                if let Err(db_err) = sqlx::query(
                    "UPDATE orch_cycles SET state = 'failed', failure_reason = $1, updated_at = $2 WHERE id = $3 AND instance_id = $4",
                )
                .bind(e.to_string().as_str())
                .bind(Utc::now())
                .bind(cycle_id)
                .bind(instance_id)
                .execute(&pool)
                .await
                {
                    tracing::error!(cycle_id = %cycle_id, error = %db_err, "failed to update cycle to failed");
                }
            }
        }
    });
}

/// Spawn the worker completion handler.
///
/// Listens for worker exit notifications and emits RunCompleted/RunFailed events
/// plus inline projection updates.
pub fn spawn_completion_handler(
    pool: PgPool,
    event_store: Arc<dyn EventStore>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<WorkerExitNotification>,
) {
    tokio::spawn(async move {
        tracing::info!("worker completion handler started");
        while let Some(notif) = rx.recv().await {
            handle_worker_exit(&pool, &event_store, notif).await;
        }
        tracing::info!("worker completion handler: channel closed");
    });
}

/// Handle a worker process exit.
async fn handle_worker_exit(
    pool: &PgPool,
    event_store: &Arc<dyn EventStore>,
    notif: WorkerExitNotification,
) {
    let now = Utc::now();
    let success = notif.exit_code == Some(0);

    // Read stdout to extract cost, output, and usage info
    let worker_output = parse_worker_stdout(&notif.log_stdout).await;

    // Look up the task_id for this run
    let task_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT task_id FROM orch_runs WHERE id = $1 AND instance_id = $2",
    )
    .bind(notif.run_id)
    .bind(notif.instance_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    if success {
        // Emit RunCompleted
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id: notif.instance_id,
            seq: 0,
            event_type: "RunCompleted".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "run_id": notif.run_id,
                "task_id": task_id,
                "exit_code": 0,
                "cost_cents": worker_output.cost_cents,
            }),
            idempotency_key: Some(format!("run-completed-{}", notif.run_id)),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };
        if let Err(e) = event_store.emit(envelope).await {
            tracing::error!(run_id = %notif.run_id, error = %e, "failed to emit RunCompleted");
        }

        // Update run projection (including output_json)
        let _ = sqlx::query(
            "UPDATE orch_runs SET state = 'completed', exit_code = 0, cost_cents = $1, output_json = $2, finished_at = $3 WHERE id = $4",
        )
        .bind(worker_output.cost_cents)
        .bind(&worker_output.output_json)
        .bind(now)
        .bind(notif.run_id)
        .execute(pool)
        .await;

        // Update task state to passed
        if let Some(tid) = task_id {
            let _ = sqlx::query(
                "UPDATE orch_tasks SET state = 'passed', updated_at = $1 WHERE id = $2",
            )
            .bind(now)
            .bind(tid)
            .execute(pool)
            .await;
        }
    } else {
        let exit_code = notif.exit_code.unwrap_or(-1);
        let failure_msg = format!("worker exited with code {}", exit_code);

        // Emit RunFailed
        let envelope = EventEnvelope {
            event_id: Uuid::new_v4(),
            instance_id: notif.instance_id,
            seq: 0,
            event_type: "RunFailed".to_string(),
            event_version: 1,
            payload: serde_json::json!({
                "run_id": notif.run_id,
                "task_id": task_id,
                "exit_code": exit_code,
                "failure_category": "ProcessExit",
                "error_message": failure_msg,
                "cost_cents": worker_output.cost_cents,
            }),
            idempotency_key: Some(format!("run-failed-{}", notif.run_id)),
            correlation_id: None,
            causation_id: None,
            occurred_at: now,
            recorded_at: now,
        };
        if let Err(e) = event_store.emit(envelope).await {
            tracing::error!(run_id = %notif.run_id, error = %e, "failed to emit RunFailed");
        }

        // Update run projection (including output_json)
        let _ = sqlx::query(
            "UPDATE orch_runs SET state = 'failed', exit_code = $1, cost_cents = $2, output_json = $3, failure_category = 'ProcessExit', finished_at = $4 WHERE id = $5",
        )
        .bind(exit_code)
        .bind(worker_output.cost_cents)
        .bind(&worker_output.output_json)
        .bind(now)
        .bind(notif.run_id)
        .execute(pool)
        .await;

        // Update task state to failed
        if let Some(tid) = task_id {
            let _ = sqlx::query(
                "UPDATE orch_tasks SET state = 'failed', failure_reason = $1, updated_at = $2 WHERE id = $3",
            )
            .bind(&failure_msg)
            .bind(now)
            .bind(tid)
            .execute(pool)
            .await;
        }
    }

    // Decrement active_runs in server capacity
    let _ = sqlx::query(
        "UPDATE orch_server_capacity SET active_runs = GREATEST(active_runs - 1, 0), updated_at = $1 WHERE instance_id = $2",
    )
    .bind(now)
    .bind(notif.instance_id)
    .execute(pool)
    .await;

    // Check if all tasks in the cycle are terminal → update cycle state
    if let Some(tid) = task_id {
        check_cycle_completion(pool, event_store, notif.instance_id, tid).await;
    }

    tracing::info!(
        run_id = %notif.run_id,
        exit_code = ?notif.exit_code,
        success = success,
        cost_cents = worker_output.cost_cents,
        has_output = worker_output.result_text.is_some(),
        "worker exit handled"
    );
}

/// Check if all tasks in a cycle are terminal, and if so update cycle state.
async fn check_cycle_completion(
    pool: &PgPool,
    _event_store: &Arc<dyn EventStore>,
    instance_id: Uuid,
    task_id: Uuid,
) {
    // Get the cycle_id for this task
    let cycle_id = match sqlx::query_scalar::<_, Uuid>(
        "SELECT cycle_id FROM orch_tasks WHERE id = $1 AND instance_id = $2",
    )
    .bind(task_id)
    .bind(instance_id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(id)) => id,
        _ => return,
    };

    // Count non-terminal tasks
    #[derive(sqlx::FromRow)]
    struct CountRow {
        total: i64,
        non_terminal: i64,
        passed: i64,
    }

    let counts = match sqlx::query_as::<_, CountRow>(
        r#"
        SELECT
            COUNT(*) as total,
            COUNT(*) FILTER (WHERE state NOT IN ('passed', 'failed', 'cancelled', 'skipped')) as non_terminal,
            COUNT(*) FILTER (WHERE state = 'passed') as passed
        FROM orch_tasks
        WHERE cycle_id = $1 AND instance_id = $2
        "#,
    )
    .bind(cycle_id)
    .bind(instance_id)
    .fetch_one(pool)
    .await
    {
        Ok(c) => c,
        Err(_) => return,
    };

    if counts.non_terminal > 0 || counts.total == 0 {
        return; // Still running tasks
    }

    let now = Utc::now();
    let new_state = if counts.passed == counts.total {
        "completed"
    } else {
        "failed"
    };

    let failure_reason = if new_state == "failed" {
        Some(format!(
            "{}/{} tasks passed",
            counts.passed, counts.total
        ))
    } else {
        None
    };

    let _ = sqlx::query(
        "UPDATE orch_cycles SET state = $1, failure_reason = $2, updated_at = $3 WHERE id = $4 AND instance_id = $5",
    )
    .bind(new_state)
    .bind(failure_reason.as_deref())
    .bind(now)
    .bind(cycle_id)
    .bind(instance_id)
    .execute(pool)
    .await;

    tracing::info!(
        cycle_id = %cycle_id,
        state = new_state,
        passed = counts.passed,
        total = counts.total,
        "cycle state updated after all tasks terminal"
    );
}

/// Parsed worker output from Claude CLI's stdout JSON.
struct WorkerOutput {
    cost_cents: i64,
    result_text: Option<String>,
    output_json: Option<serde_json::Value>,
}

/// A single Bash tool invocation and its raw captured output.
#[derive(Debug, Clone, serde::Serialize)]
struct ToolOutput {
    tool_name: String,
    command: Option<String>,
    stdout: String,
    stderr: String,
}

/// Strip ANSI escape sequences (CSI codes like `\x1b[47C`, colors, etc.)
/// from terminal output to produce clean plain text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            // Consume CSI sequence: ESC [ <params> <final byte>
            if chars.peek() == Some(&'[') {
                chars.next(); // consume '['
                // Consume parameter bytes (0x30-0x3F) and intermediate bytes (0x20-0x2F)
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == ';' || c == '?' || (c >= ' ' && c <= '/') {
                        chars.next();
                    } else {
                        break;
                    }
                }
                // Consume final byte (0x40-0x7E)
                if let Some(&c) = chars.peek() {
                    if c >= '@' && c <= '~' {
                        chars.next();
                    }
                }
            }
            // Also handle OSC sequences: ESC ] ... BEL/ST
            else if chars.peek() == Some(&']') {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' { break; }
                    if c == '\x1b' {
                        if chars.peek() == Some(&'\\') { chars.next(); }
                        break;
                    }
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Extract raw Bash tool outputs from Claude CLI's multi-event JSONL.
///
/// Correlates `assistant` events (containing `tool_use` with command info)
/// with subsequent `user` events (containing `tool_use_result` with stdout/stderr).
/// Only extracts Bash tool outputs — Read/Write/Edit/Grep are not user-facing.
fn extract_tool_outputs(events: &[serde_json::Value]) -> Vec<ToolOutput> {
    use std::collections::HashMap;

    // Build a map of tool_use_id → (tool_name, command) from assistant events.
    let mut tool_info: HashMap<String, (String, Option<String>)> = HashMap::new();

    for event in events {
        if event.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };
        for item in content {
            if item.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
            let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let command = item
                .get("input")
                .and_then(|i| i.get("command"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            tool_info.insert(id.to_string(), (name.to_string(), command));
        }
    }

    // Extract tool results from user events, correlating via tool_use_id.
    let mut outputs = Vec::new();

    for event in events {
        if event.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }

        // Check message.content for tool_result entries (has tool_use_id for correlation).
        let content_items = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());

        if let Some(items) = content_items {
            for item in items {
                if item.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                    continue;
                }
                let tool_use_id = item
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                let (tool_name, command) = tool_info
                    .get(tool_use_id)
                    .cloned()
                    .unwrap_or_else(|| ("Unknown".to_string(), None));

                // Only include Bash tool outputs.
                if tool_name != "Bash" {
                    continue;
                }

                // Get stdout from tool_use_result (richer) or fall back to tool_result content.
                let tur = event.get("tool_use_result");
                let stdout = tur
                    .and_then(|r| r.get("stdout"))
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("content").and_then(|v| v.as_str()))
                    .unwrap_or_default()
                    .to_string();
                let stderr = tur
                    .and_then(|r| r.get("stderr"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                if !stdout.is_empty() || !stderr.is_empty() {
                    outputs.push(ToolOutput {
                        tool_name,
                        command,
                        stdout: strip_ansi(&stdout),
                        stderr: strip_ansi(&stderr),
                    });
                }
            }
        }
    }

    outputs
}

/// Parse Claude CLI's stdout JSON output.
///
/// Handles two formats:
/// - **Compact**: single JSON object with `type: "result"` (simple 1-2 turn tasks)
/// - **Multi-event**: JSON array of events including tool_use/tool_result pairs
///
/// For multi-event format, extracts raw Bash stdout/stderr as ground truth
/// and computes SHA256 for immutability proof.
async fn parse_worker_stdout(path: &str) -> WorkerOutput {
    let default = WorkerOutput { cost_cents: 0, result_text: None, output_json: None };

    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return default,
    };

    // Phase 1: Parse all events and find the result object.
    let mut all_events: Vec<serde_json::Value> = Vec::new();
    let mut result_event: Option<serde_json::Value> = None;
    let mut output_format = "compact";

    // Try JSONL first (one JSON object per line).
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            all_events.push(v.clone());
            if v.get("type").and_then(|t| t.as_str()) == Some("result")
                || v.get("total_cost_usd").is_some()
            {
                result_event = Some(v);
            }
        }
    }

    // If line-by-line yielded nothing, try parsing the whole content.
    if all_events.is_empty() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(content.trim()) {
            if let Some(arr) = v.as_array() {
                // JSON array of events (verbose/multi-event format).
                output_format = "multi_event";
                for item in arr {
                    all_events.push(item.clone());
                    if item.get("type").and_then(|t| t.as_str()) == Some("result")
                        || item.get("total_cost_usd").is_some()
                    {
                        result_event = Some(item.clone());
                    }
                }
            } else {
                // Single compact JSON object.
                result_event = Some(v);
            }
        }
    } else if all_events.len() > 1 {
        output_format = "multi_event";
    }

    let Some(parsed) = result_event else { return default; };

    // Phase 2: Extract standard fields from the result event.
    let cost_cents = parsed
        .get("total_cost_usd")
        .and_then(|v| v.as_f64())
        .map(|usd| (usd * 100.0) as i64)
        .unwrap_or(0);

    let result_text = parsed.get("result").and_then(|v| v.as_str()).map(|s| s.to_string());
    let duration_ms = parsed.get("duration_ms").and_then(|v| v.as_i64());
    let num_turns = parsed.get("num_turns").and_then(|v| v.as_i64());
    let input_tokens = parsed.get("usage").map(|u| {
        let base = u.get("input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let cache_create = u.get("cache_creation_input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        let cache_read = u.get("cache_read_input_tokens").and_then(|v| v.as_i64()).unwrap_or(0);
        base + cache_create + cache_read
    });
    let output_tokens = parsed
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_i64());

    // Phase 3: Extract tool outputs (multi-event only).
    let tool_outputs = if output_format == "multi_event" {
        extract_tool_outputs(&all_events)
    } else {
        vec![]
    };

    // Phase 4: Compute SHA256 of concatenated raw stdout for immutability proof.
    let stdout_sha256 = if !tool_outputs.is_empty() {
        let concatenated: String = tool_outputs
            .iter()
            .map(|to| to.stdout.as_str())
            .collect::<Vec<_>>()
            .join("\n---\n");
        Some(hex::encode(sha2::Sha256::digest(concatenated.as_bytes())))
    } else {
        None
    };

    // Phase 5: Build output_json.
    let output_json = Some(serde_json::json!({
        "result": result_text,
        "duration_ms": duration_ms,
        "num_turns": num_turns,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "tool_outputs": tool_outputs,
        "stdout_sha256": stdout_sha256,
        "output_format": output_format,
    }));

    WorkerOutput { cost_cents, result_text, output_json }
}
