use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::Subcommand;
use colored::Colorize;
use uuid::Uuid;

// ── Subcommand types ──

#[derive(Subcommand)]
pub enum OrchAction {
    /// Instance management
    Instance {
        #[command(subcommand)]
        action: InstanceAction,
    },
    /// Cycle management
    Cycle {
        #[command(subcommand)]
        action: CycleAction,
    },
    /// Manual reconciliation for an instance
    Reconcile {
        /// Instance ID
        instance_id: Uuid,
    },
    /// Skip a blocked event (mark as skipped)
    SkipEvent {
        /// Event ID to skip
        event_id: i64,
    },
    /// Resume a blocked instance
    Resume {
        /// Instance ID to resume
        instance_id: Uuid,
    },
    /// Maintenance mode management
    Maintenance {
        #[command(subcommand)]
        action: MaintenanceAction,
    },
    /// Rebuild projections from event log
    RebuildProjections {
        /// Instance ID
        instance_id: Uuid,
    },
}

#[derive(Subcommand)]
pub enum InstanceAction {
    /// List all instances
    List,
    /// Show instance status
    Status {
        /// Instance ID
        instance_id: Uuid,
    },
    /// Create a new instance
    Create {
        /// Project ID
        #[arg(long)]
        project_id: Uuid,
        /// Instance name
        #[arg(long)]
        name: String,
        /// Data directory
        #[arg(long)]
        data_dir: String,
        /// Auth token (will be hashed)
        #[arg(long)]
        token: String,
    },
}

#[derive(Subcommand)]
pub enum CycleAction {
    /// List cycles for an instance
    List {
        /// Instance ID
        instance_id: Uuid,
    },
    /// Create a new cycle
    Create {
        /// Instance ID
        #[arg(long)]
        instance_id: Uuid,
        /// Objective/prompt for the cycle
        #[arg(long)]
        objective: String,
    },
    /// Approve a cycle's plan
    Approve {
        /// Cycle ID
        cycle_id: Uuid,
    },
    /// Cancel a cycle
    Cancel {
        /// Cycle ID
        cycle_id: Uuid,
        /// Reason for cancellation
        #[arg(long, default_value = "admin_cancelled")]
        reason: String,
    },
}

#[derive(Subcommand)]
pub enum MaintenanceAction {
    /// Enter maintenance mode
    Enter {
        /// Instance ID
        instance_id: Uuid,
        /// Optional reason
        #[arg(long)]
        reason: Option<String>,
    },
    /// Exit maintenance mode
    Exit {
        /// Instance ID
        instance_id: Uuid,
    },
}

// ── Row types for sqlx queries ──

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct InstanceRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    state: String,
    block_reason: Option<String>,
    pid: Option<i32>,
    data_dir: String,
    started_at: DateTime<Utc>,
    last_heartbeat: DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct InstanceDetailRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    state: String,
    block_reason: Option<String>,
    pid: Option<i32>,
    bind_addr: Option<String>,
    data_dir: String,
    started_at: DateTime<Utc>,
    last_heartbeat: DateTime<Utc>,
    config: serde_json::Value,
}

#[derive(sqlx::FromRow)]
struct ProjectNameRow {
    name: String,
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct CycleRow {
    id: Uuid,
    instance_id: Uuid,
    state: String,
    prompt: String,
    block_reason: Option<String>,
    failure_reason: Option<String>,
    cancel_reason: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct TaskSummaryRow {
    state: String,
    count: i64,
}

#[derive(sqlx::FromRow)]
struct RunSummaryRow {
    state: String,
    count: i64,
}

#[derive(sqlx::FromRow)]
struct EventCountRow {
    count: i64,
}

#[derive(sqlx::FromRow)]
struct CapacityRow {
    active_runs: i32,
    max_concurrent: i32,
}

#[derive(sqlx::FromRow)]
struct InstanceStateRow {
    state: String,
    block_reason: Option<String>,
}

#[derive(sqlx::FromRow)]
struct CycleStateRow {
    state: String,
    instance_id: Uuid,
}

// ── Main dispatch ──

pub async fn run(action: OrchAction) -> Result<()> {
    let pool = match openclaw_db::pool() {
        Some(p) => p,
        None => {
            eprintln!("{}", "Postgres not connected. Set DATABASE_URL env var.".red());
            std::process::exit(1);
        }
    };

    match action {
        OrchAction::Instance { action } => match action {
            InstanceAction::List => instance_list(pool).await,
            InstanceAction::Status { instance_id } => instance_status(pool, instance_id).await,
            InstanceAction::Create { .. } => {
                eprintln!(
                    "{} Instance creation requires the full provisioning workflow (git clone, worktree setup).",
                    "Not implemented:".yellow().bold()
                );
                eprintln!("Use the orchestrator API or gateway bot to create instances.");
                Ok(())
            }
        },
        OrchAction::Cycle { action } => match action {
            CycleAction::List { instance_id } => cycle_list(pool, instance_id).await,
            CycleAction::Create { .. } => {
                eprintln!(
                    "{} Cycle creation requires planner initialization.",
                    "Not implemented:".yellow().bold()
                );
                eprintln!("Use the orchestrator API or gateway bot to create cycles.");
                Ok(())
            }
            CycleAction::Approve { cycle_id } => cycle_approve(pool, cycle_id).await,
            CycleAction::Cancel { cycle_id, reason } => {
                cycle_cancel(pool, cycle_id, &reason).await
            }
        },
        OrchAction::Reconcile { .. } => {
            eprintln!(
                "{} Reconciliation requires WorkerManager for process reattachment.",
                "Not implemented:".yellow().bold()
            );
            eprintln!("Use the orchestrator API to trigger reconciliation.");
            Ok(())
        }
        OrchAction::SkipEvent { .. } => {
            eprintln!(
                "{} Event skipping requires projector integration.",
                "Not implemented:".yellow().bold()
            );
            eprintln!("Use the orchestrator API to skip events.");
            Ok(())
        }
        OrchAction::Resume { instance_id } => resume_instance(pool, instance_id).await,
        OrchAction::Maintenance { action } => match action {
            MaintenanceAction::Enter {
                instance_id,
                reason,
            } => maintenance_enter(pool, instance_id, reason).await,
            MaintenanceAction::Exit { instance_id } => {
                maintenance_exit(pool, instance_id).await
            }
        },
        OrchAction::RebuildProjections { .. } => {
            eprintln!(
                "{} Projection rebuild requires the full projector stack.",
                "Not implemented:".yellow().bold()
            );
            eprintln!("Use the orchestrator API or enter maintenance mode and rebuild via the API.");
            Ok(())
        }
    }
}

// ── Instance commands ──

async fn instance_list(pool: &sqlx::PgPool) -> Result<()> {
    let instances = sqlx::query_as::<_, InstanceRow>(
        r#"
        SELECT id, project_id, name, state, block_reason, pid,
               data_dir, started_at, last_heartbeat
        FROM orch_instances
        ORDER BY started_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    if instances.is_empty() {
        println!("{}", "No orchestrator instances found.".dimmed());
        return Ok(());
    }

    println!(
        "{}",
        format!("Orchestrator Instances ({})", instances.len())
            .bold()
            .cyan()
    );
    println!();

    for inst in &instances {
        // Look up project name
        let project_name = sqlx::query_as::<_, ProjectNameRow>(
            "SELECT name FROM orch_projects WHERE id = $1",
        )
        .bind(inst.project_id)
        .fetch_optional(pool)
        .await?
        .map(|r| r.name)
        .unwrap_or_else(|| "unknown".to_string());

        let state_display = format_state(&inst.state, inst.block_reason.as_deref());
        let heartbeat_ago = format_ago(inst.last_heartbeat);

        println!(
            "  {} {} [{}]",
            inst.name.bold(),
            format!("({})", project_name).dimmed(),
            state_display
        );
        println!(
            "    ID: {}  PID: {}  Heartbeat: {} ago",
            inst.id.to_string().dimmed(),
            inst.pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string()),
            heartbeat_ago,
        );
        println!("    Dir: {}", inst.data_dir.dimmed());
        println!();
    }

    Ok(())
}

async fn instance_status(pool: &sqlx::PgPool, instance_id: Uuid) -> Result<()> {
    let inst = sqlx::query_as::<_, InstanceDetailRow>(
        r#"
        SELECT id, project_id, name, state, block_reason, pid,
               bind_addr, data_dir, started_at, last_heartbeat, config
        FROM orch_instances
        WHERE id = $1
        "#,
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    let inst = match inst {
        Some(i) => i,
        None => {
            eprintln!("{} Instance {} not found.", "Error:".red().bold(), instance_id);
            std::process::exit(1);
        }
    };

    // Project name
    let project_name = sqlx::query_as::<_, ProjectNameRow>(
        "SELECT name FROM orch_projects WHERE id = $1",
    )
    .bind(inst.project_id)
    .fetch_optional(pool)
    .await?
    .map(|r| r.name)
    .unwrap_or_else(|| "unknown".to_string());

    let state_display = format_state(&inst.state, inst.block_reason.as_deref());

    println!("{}", "Instance Status".bold().cyan());
    println!("  {} {}", "Name:".bold(), inst.name);
    println!("  {} {}", "ID:".bold(), inst.id);
    println!("  {} {} ({})", "Project:".bold(), project_name, inst.project_id.to_string().dimmed());
    println!("  {} {}", "State:".bold(), state_display);
    if let Some(ref reason) = inst.block_reason {
        println!("  {} {}", "Block Reason:".bold(), reason.yellow());
    }
    println!(
        "  {} {}",
        "PID:".bold(),
        inst.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "  {} {}",
        "Bind Addr:".bold(),
        inst.bind_addr.as_deref().unwrap_or("-")
    );
    println!("  {} {}", "Data Dir:".bold(), inst.data_dir);
    println!("  {} {}", "Started:".bold(), inst.started_at.format("%Y-%m-%d %H:%M:%S UTC"));
    println!(
        "  {} {} ({} ago)",
        "Last Heartbeat:".bold(),
        inst.last_heartbeat.format("%Y-%m-%d %H:%M:%S UTC"),
        format_ago(inst.last_heartbeat)
    );

    // Cycle summary
    let cycles = sqlx::query_as::<_, CycleRow>(
        r#"
        SELECT id, instance_id, state, prompt, block_reason, failure_reason,
               cancel_reason, created_at, updated_at
        FROM orch_cycles
        WHERE instance_id = $1
        ORDER BY created_at DESC
        LIMIT 5
        "#,
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    println!();
    println!("  {}", "Recent Cycles:".bold());
    if cycles.is_empty() {
        println!("    {}", "No cycles.".dimmed());
    } else {
        for cycle in &cycles {
            let prompt_preview = if cycle.prompt.len() > 60 {
                format!("{}...", &cycle.prompt[..57])
            } else {
                cycle.prompt.clone()
            };
            println!(
                "    {} [{}] {}",
                cycle.id.to_string()[..8].dimmed(),
                format_cycle_state(&cycle.state),
                prompt_preview,
            );
        }
    }

    // Task summary by state
    let task_summary = sqlx::query_as::<_, TaskSummaryRow>(
        "SELECT state, count(*) as count FROM orch_tasks WHERE instance_id = $1 GROUP BY state ORDER BY state",
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    if !task_summary.is_empty() {
        println!();
        println!("  {}", "Tasks by State:".bold());
        for ts in &task_summary {
            println!("    {}: {}", ts.state, ts.count);
        }
    }

    // Active runs
    let run_summary = sqlx::query_as::<_, RunSummaryRow>(
        "SELECT state, count(*) as count FROM orch_runs WHERE instance_id = $1 AND state IN ('claimed', 'running') GROUP BY state ORDER BY state",
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    if !run_summary.is_empty() {
        println!();
        println!("  {}", "Active Runs:".bold());
        for rs in &run_summary {
            println!("    {}: {}", rs.state, rs.count);
        }
    }

    // Capacity
    let capacity = sqlx::query_as::<_, CapacityRow>(
        "SELECT active_runs, max_concurrent FROM orch_server_capacity WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    if let Some(cap) = capacity {
        println!();
        println!(
            "  {} {}/{}",
            "Capacity:".bold(),
            cap.active_runs,
            cap.max_concurrent
        );
    }

    // Event count
    let event_count = sqlx::query_as::<_, EventCountRow>(
        "SELECT count(*) as count FROM orch_events WHERE instance_id = $1",
    )
    .bind(instance_id)
    .fetch_one(pool)
    .await?;

    println!(
        "  {} {}",
        "Total Events:".bold(),
        event_count.count
    );

    Ok(())
}

// ── Cycle commands ──

async fn cycle_list(pool: &sqlx::PgPool, instance_id: Uuid) -> Result<()> {
    // Verify instance exists
    let exists = sqlx::query_as::<_, InstanceStateRow>(
        "SELECT state, block_reason FROM orch_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    if exists.is_none() {
        eprintln!("{} Instance {} not found.", "Error:".red().bold(), instance_id);
        std::process::exit(1);
    }

    let cycles = sqlx::query_as::<_, CycleRow>(
        r#"
        SELECT id, instance_id, state, prompt, block_reason, failure_reason,
               cancel_reason, created_at, updated_at
        FROM orch_cycles
        WHERE instance_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    if cycles.is_empty() {
        println!("{}", "No cycles found for this instance.".dimmed());
        return Ok(());
    }

    println!(
        "{}",
        format!("Cycles for Instance {} ({})", &instance_id.to_string()[..8], cycles.len())
            .bold()
            .cyan()
    );
    println!();

    for cycle in &cycles {
        let prompt_preview = if cycle.prompt.len() > 80 {
            format!("{}...", &cycle.prompt[..77])
        } else {
            cycle.prompt.clone()
        };

        println!(
            "  {} [{}]",
            cycle.id.to_string().dimmed(),
            format_cycle_state(&cycle.state),
        );
        println!("    {}", prompt_preview);
        println!(
            "    Created: {}  Updated: {}",
            cycle.created_at.format("%Y-%m-%d %H:%M:%S"),
            cycle.updated_at.format("%Y-%m-%d %H:%M:%S"),
        );
        if let Some(ref reason) = cycle.block_reason {
            println!("    Block: {}", reason.yellow());
        }
        if let Some(ref reason) = cycle.failure_reason {
            println!("    Failure: {}", reason.red());
        }
        if let Some(ref reason) = cycle.cancel_reason {
            println!("    Cancel: {}", reason.yellow());
        }

        // Task counts for this cycle
        let tasks = sqlx::query_as::<_, TaskSummaryRow>(
            "SELECT state, count(*) as count FROM orch_tasks WHERE cycle_id = $1 GROUP BY state ORDER BY state",
        )
        .bind(cycle.id)
        .fetch_all(pool)
        .await?;

        if !tasks.is_empty() {
            let summary: Vec<String> = tasks
                .iter()
                .map(|t| format!("{}={}", t.state, t.count))
                .collect();
            println!("    Tasks: {}", summary.join(", "));
        }

        println!();
    }

    Ok(())
}

async fn cycle_approve(pool: &sqlx::PgPool, cycle_id: Uuid) -> Result<()> {
    // Look up the cycle
    let cycle = sqlx::query_as::<_, CycleStateRow>(
        "SELECT state, instance_id FROM orch_cycles WHERE id = $1",
    )
    .bind(cycle_id)
    .fetch_optional(pool)
    .await?;

    let cycle = match cycle {
        Some(c) => c,
        None => {
            eprintln!("{} Cycle {} not found.", "Error:".red().bold(), cycle_id);
            std::process::exit(1);
        }
    };

    if cycle.state != "plan_ready" {
        eprintln!(
            "{} Cycle is in '{}' state, must be 'plan_ready' to approve.",
            "Error:".red().bold(),
            cycle.state
        );
        std::process::exit(1);
    }

    // Emit PlanApproved event via direct SQL
    let now = Utc::now();
    let event_id = Uuid::new_v4();
    let idempotency_key = format!("cli-approve-{}-{}", cycle_id, now.timestamp_millis());

    emit_event(
        pool,
        event_id,
        cycle.instance_id,
        "PlanApproved",
        &serde_json::json!({
            "cycle_id": cycle_id.to_string(),
            "actor": { "kind": "Admin", "id": "cli" },
        }),
        Some(&idempotency_key),
        now,
    )
    .await?;

    // Update projection table
    sqlx::query("UPDATE orch_cycles SET state = 'approved', updated_at = $1 WHERE id = $2")
        .bind(now)
        .bind(cycle_id)
        .execute(pool)
        .await?;

    println!(
        "{} Cycle {} approved.",
        "OK".green().bold(),
        cycle_id.to_string()[..8].bold()
    );

    Ok(())
}

async fn cycle_cancel(pool: &sqlx::PgPool, cycle_id: Uuid, reason: &str) -> Result<()> {
    // Look up the cycle
    let cycle = sqlx::query_as::<_, CycleStateRow>(
        "SELECT state, instance_id FROM orch_cycles WHERE id = $1",
    )
    .bind(cycle_id)
    .fetch_optional(pool)
    .await?;

    let cycle = match cycle {
        Some(c) => c,
        None => {
            eprintln!("{} Cycle {} not found.", "Error:".red().bold(), cycle_id);
            std::process::exit(1);
        }
    };

    // Can cancel from most non-terminal states
    let terminal = ["completed", "failed", "cancelled"];
    if terminal.contains(&cycle.state.as_str()) {
        eprintln!(
            "{} Cycle is in terminal '{}' state, cannot cancel.",
            "Error:".red().bold(),
            cycle.state
        );
        std::process::exit(1);
    }

    // Emit CycleCancelled event
    let now = Utc::now();
    let event_id = Uuid::new_v4();
    let idempotency_key = format!("cli-cancel-{}-{}", cycle_id, now.timestamp_millis());

    emit_event(
        pool,
        event_id,
        cycle.instance_id,
        "CycleCancelled",
        &serde_json::json!({
            "cycle_id": cycle_id.to_string(),
            "reason": reason,
            "actor": { "kind": "Admin", "id": "cli" },
        }),
        Some(&idempotency_key),
        now,
    )
    .await?;

    // Update projection table
    sqlx::query(
        "UPDATE orch_cycles SET state = 'cancelled', cancel_reason = $1, updated_at = $2 WHERE id = $3",
    )
    .bind(reason)
    .bind(now)
    .bind(cycle_id)
    .execute(pool)
    .await?;

    println!(
        "{} Cycle {} cancelled (reason: {}).",
        "OK".green().bold(),
        cycle_id.to_string()[..8].bold(),
        reason
    );

    Ok(())
}

// ── Maintenance commands ──

async fn maintenance_enter(
    pool: &sqlx::PgPool,
    instance_id: Uuid,
    reason: Option<String>,
) -> Result<()> {
    // Check current state
    let state = sqlx::query_as::<_, InstanceStateRow>(
        "SELECT state, block_reason FROM orch_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    let state = match state {
        Some(s) => s,
        None => {
            eprintln!("{} Instance {} not found.", "Error:".red().bold(), instance_id);
            std::process::exit(1);
        }
    };

    if state.state != "active" {
        eprintln!(
            "{} Instance is in '{}' state, must be 'active' to enter maintenance.",
            "Error:".red().bold(),
            state.state
        );
        std::process::exit(1);
    }

    let details = reason.unwrap_or_default();
    let now = Utc::now();
    let event_id = Uuid::new_v4();
    let idempotency_key = format!(
        "cli-maintenance-enter-{}-{}",
        instance_id,
        now.timestamp_millis()
    );

    // Emit InstanceBlocked event
    emit_event(
        pool,
        event_id,
        instance_id,
        "InstanceBlocked",
        &serde_json::json!({
            "reason": "Maintenance",
            "details": details,
            "actor": { "kind": "Admin", "id": "cli" },
        }),
        Some(&idempotency_key),
        now,
    )
    .await?;

    // Update projection table
    sqlx::query(
        "UPDATE orch_instances SET state = 'blocked', block_reason = 'Maintenance', last_heartbeat = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(instance_id)
    .execute(pool)
    .await?;

    println!(
        "{} Instance {} entered maintenance mode.",
        "OK".green().bold(),
        instance_id.to_string()[..8].bold()
    );

    Ok(())
}

async fn maintenance_exit(pool: &sqlx::PgPool, instance_id: Uuid) -> Result<()> {
    // Check current state
    let state = sqlx::query_as::<_, InstanceStateRow>(
        "SELECT state, block_reason FROM orch_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    let state = match state {
        Some(s) => s,
        None => {
            eprintln!("{} Instance {} not found.", "Error:".red().bold(), instance_id);
            std::process::exit(1);
        }
    };

    if state.state != "blocked" {
        eprintln!(
            "{} Instance is in '{}' state, must be 'blocked' to exit maintenance.",
            "Error:".red().bold(),
            state.state
        );
        std::process::exit(1);
    }

    let now = Utc::now();
    let event_id = Uuid::new_v4();
    let idempotency_key = format!(
        "cli-maintenance-exit-{}-{}",
        instance_id,
        now.timestamp_millis()
    );

    // Emit InstanceUnblocked event
    emit_event(
        pool,
        event_id,
        instance_id,
        "InstanceUnblocked",
        &serde_json::json!({
            "actor": { "kind": "Admin", "id": "cli" },
        }),
        Some(&idempotency_key),
        now,
    )
    .await?;

    // Update projection table
    sqlx::query(
        "UPDATE orch_instances SET state = 'active', block_reason = NULL, last_heartbeat = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(instance_id)
    .execute(pool)
    .await?;

    println!(
        "{} Instance {} exited maintenance mode.",
        "OK".green().bold(),
        instance_id.to_string()[..8].bold()
    );

    Ok(())
}

// ── Resume command ──

async fn resume_instance(pool: &sqlx::PgPool, instance_id: Uuid) -> Result<()> {
    // Check current state
    let state = sqlx::query_as::<_, InstanceStateRow>(
        "SELECT state, block_reason FROM orch_instances WHERE id = $1",
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    let state = match state {
        Some(s) => s,
        None => {
            eprintln!("{} Instance {} not found.", "Error:".red().bold(), instance_id);
            std::process::exit(1);
        }
    };

    if state.state != "blocked" {
        eprintln!(
            "{} Instance is in '{}' state, must be 'blocked' to resume.",
            "Error:".red().bold(),
            state.state
        );
        std::process::exit(1);
    }

    let now = Utc::now();
    let event_id = Uuid::new_v4();
    let idempotency_key = format!("cli-resume-{}-{}", instance_id, now.timestamp_millis());

    // Emit InstanceUnblocked event
    emit_event(
        pool,
        event_id,
        instance_id,
        "InstanceUnblocked",
        &serde_json::json!({
            "previous_block_reason": state.block_reason,
            "actor": { "kind": "Admin", "id": "cli" },
        }),
        Some(&idempotency_key),
        now,
    )
    .await?;

    // Update projection table
    sqlx::query(
        "UPDATE orch_instances SET state = 'active', block_reason = NULL, last_heartbeat = $1 WHERE id = $2",
    )
    .bind(now)
    .bind(instance_id)
    .execute(pool)
    .await?;

    println!(
        "{} Instance {} resumed (was blocked: {}).",
        "OK".green().bold(),
        instance_id.to_string()[..8].bold(),
        state
            .block_reason
            .as_deref()
            .unwrap_or("unknown")
    );

    Ok(())
}

// ── Event emission helper ──

/// Emit an event via direct SQL INSERT into orch_events.
///
/// This mirrors the PgEventStore::emit() logic but without requiring the full
/// event store infrastructure. Sequence numbers are computed atomically.
async fn emit_event(
    pool: &sqlx::PgPool,
    event_id: Uuid,
    instance_id: Uuid,
    event_type: &str,
    payload: &serde_json::Value,
    idempotency_key: Option<&str>,
    occurred_at: DateTime<Utc>,
) -> Result<i64> {
    // Get next seq for this instance
    let max_seq: Option<i64> =
        sqlx::query_scalar("SELECT MAX(seq) FROM orch_events WHERE instance_id = $1")
            .bind(instance_id)
            .fetch_one(pool)
            .await?;
    let seq = max_seq.unwrap_or(0) + 1;

    let result = sqlx::query_scalar::<_, i64>(
        r#"
        INSERT INTO orch_events (
            event_id, instance_id, seq, event_type, event_version,
            payload, idempotency_key, occurred_at, recorded_at
        )
        VALUES ($1, $2, $3, $4, 1, $5, $6, $7, now())
        ON CONFLICT (instance_id, idempotency_key)
            WHERE idempotency_key IS NOT NULL
            DO UPDATE SET recorded_at = orch_events.recorded_at
        RETURNING seq
        "#,
    )
    .bind(event_id)
    .bind(instance_id)
    .bind(seq)
    .bind(event_type)
    .bind(payload)
    .bind(idempotency_key)
    .bind(occurred_at)
    .fetch_one(pool)
    .await?;

    // Best-effort NOTIFY for cross-process subscribers
    let notify_payload = format!("{}:{}", instance_id, result);
    let _ = sqlx::query("SELECT pg_notify('orch_events_channel', $1)")
        .bind(&notify_payload)
        .execute(pool)
        .await;

    Ok(result)
}

// ── Formatting helpers ──

fn format_state(state: &str, block_reason: Option<&str>) -> colored::ColoredString {
    match state {
        "active" => "active".green().bold(),
        "blocked" => {
            let label = match block_reason {
                Some(reason) => format!("blocked ({})", reason),
                None => "blocked".to_string(),
            };
            label.yellow().bold()
        }
        "provisioning" => "provisioning".blue().bold(),
        "provisioning_failed" => "provisioning_failed".red().bold(),
        "suspended" => "suspended".red().bold(),
        _ => state.to_string().normal(),
    }
}

fn format_cycle_state(state: &str) -> colored::ColoredString {
    match state {
        "created" => "created".blue(),
        "planning" => "planning".blue(),
        "plan_ready" => "plan_ready".yellow(),
        "approved" => "approved".green(),
        "running" => "running".green().bold(),
        "blocked" => "blocked".yellow().bold(),
        "completing" => "completing".cyan(),
        "completed" => "completed".green(),
        "failed" => "failed".red(),
        "cancelled" => "cancelled".red(),
        _ => state.to_string().normal(),
    }
}

fn format_ago(dt: DateTime<Utc>) -> String {
    let diff = Utc::now() - dt;
    let secs = diff.num_seconds();

    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}
