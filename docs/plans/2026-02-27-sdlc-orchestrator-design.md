# AI SDLC Orchestrator — Design Document

**Date**: 2026-02-27
**Status**: Approved
**Location**: New crates in `openclaw-rs` workspace

## Overview

A production-grade, local-first orchestrator that manages multiple parallel Claude Code
worker sessions across repositories/worktrees/branches. Provides structured upgrade cycles,
verification gates, observability, and a web UI control plane.

## Decisions Log

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Where to build | New crates in openclaw-rs workspace | Reuse Postgres, config, tools, gateway |
| Worker execution | `claude --print --output-format json` subprocess | Pragmatic, debuggable, uses existing install |
| Worker iteration | Single-shot with chained continuation | Stateless per invocation but iterative repair |
| Plan generation | Claude Code generates plan (structured JSON) | User reviews/approves in UI before execution |
| UI stack | Vanilla TS + esbuild + `include_str!()` | Same pattern as aitan-dashboard, single binary |
| Git operations | CLI wrapper with trait (no git2) | More reliable, AI-friendly, trait allows swap |
| Instance model | Process-bound for MVP (pid/bind in orch_instances) | One instance = one project = one UI server |

## Architecture

```
openclaw-rs workspace
├── crates/
│   ├── openclaw-core/           (existing — config, paths)
│   ├── openclaw-agent/          (existing — LLM runtime, tools)
│   ├── openclaw-gateway/        (existing — Telegram/Discord, add orch commands)
│   ├── openclaw-db/             (existing — Postgres, add orch_* migrations)
│   ├── openclaw-cli/            (existing — add orch subcommands)
│   ├── openclaw-mcp/            (existing)
│   ├── openclaw-orchestrator/   (NEW — pure domain library)
│   └── openclaw-ui/             (NEW — binary: Actix server + embedded frontend)
```

### Component Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                     openclaw-rs workspace                    │
│                                                             │
│  ┌──────────────┐   ┌──────────────┐   ┌──────────────┐   │
│  │ openclaw-    │   │ openclaw-    │   │ openclaw-    │   │
│  │ gateway      │   │ core         │   │ db           │   │
│  │ (Telegram/   │   │ (config,     │   │ (Postgres)   │   │
│  │  Discord)    │   │  paths)      │   │              │   │
│  └──────┬───────┘   └──────────────┘   └──────┬───────┘   │
│         │                                      │           │
│  ┌──────┴──────────────────────────────────────┴────────┐  │
│  │              openclaw-orchestrator (library)          │  │
│  │                                                      │  │
│  │  ┌──────────┐ ┌───────────┐ ┌─────────────────┐     │  │
│  │  │ Planner  │ │ Scheduler │ │ Worker Manager  │     │  │
│  │  │ (claude  │ │ (queue,   │ │ (spawn, monitor │     │  │
│  │  │  --print │ │  SKIP     │ │  reattach,      │     │  │
│  │  │  → plan) │ │  LOCKED)  │ │  log tailing)   │     │  │
│  │  └──────────┘ └───────────┘ └────────┬────────┘     │  │
│  │  ┌──────────┐ ┌───────────┐          │              │  │
│  │  │ Git Mgr  │ │ Verifier  │    ┌─────┴──────┐      │  │
│  │  │ (trait:  │ │ (pipeline │    │ Worker 1   │      │  │
│  │  │  CLI     │ │  runner)  │    │ Worker 2   │      │  │
│  │  │  impl)   │ │           │    │ Worker N   │      │  │
│  │  └──────────┘ └───────────┘    └────────────┘      │  │
│  │  ┌──────────┐ ┌───────────┐                         │  │
│  │  │ Event    │ │ Budget    │                         │  │
│  │  │ Bus +    │ │ Tracker   │                         │  │
│  │  │ Replay   │ │           │                         │  │
│  │  └──────────┘ └───────────┘                         │  │
│  └──────────────────────┬──────────────────────────────┘  │
│                         │                                  │
│  ┌──────────────────────┴──────────────────────────────┐  │
│  │              openclaw-ui (binary)                    │  │
│  │  Actix-web: REST API + WebSocket + embedded TS      │  │
│  │  Binds to 127.0.0.1:<port> (per-instance)           │  │
│  └─────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                          │
                   ┌──────┴──────┐
                   │  PostgreSQL │  (shared with gateway)
                   │  orch_*     │  (namespaced tables)
                   └─────────────┘
```

### Key Constraint: Orchestrator is Actix-Agnostic

`openclaw-orchestrator` is a pure library crate. It does not import Actix types.
`openclaw-ui` is the thin adapter that wires HTTP routes and WebSocket to orchestrator functions.

## Entity Model

### Hierarchy

```
Project (repo identity, default config)
  └── Instance (process-bound runtime, one per project for MVP)
        └── Cycle (one per raw prompt/upgrade request)
              ├── Phase (ordered stages)
              │     └── Task (work unit with acceptance criteria)
              │           └── TaskRun (attempt: branch, worktree, worker, logs, cost)
              │                 └── Artifact (verification evidence)
              └── Event (append-only log of everything)
```

### Relationship: Project → Instance

`orch_projects` is repo identity and default config. It does NOT reference `instance_id`.
`orch_instances` references `project_id`. For MVP: one instance per project.
Everything downstream keys off `instance_id`.

## State Machines

### Cycle States

```
Draft → Planned → Approved → Running → Verifying → Merging → Done
                                 ↓         ↓          ↓
                              Failed    Failed     Conflicted
                                 ↓         ↓          ↓
                              (retry)  (retry)    (manual)
```

### Task States

```
Pending → Scheduled → Running → Verifying → Passed → Merged
                        ↓          ↓
                     Failed     Failed
                        ↓          ↓
                     Repairing  Repairing → Running (retry, max N)
                        ↓
                     Abandoned (max retries exceeded)
```

### TaskRun States

```
Spawning → Running → Completed
              ↓
           Failed → (new run created if retries remain)
```

On orchestrator restart:
```
Running → check PID alive?
  yes → Reattached (resume log tailing, emit WorkerReattached)
  no  → Failed (emit WorkerFailed, create new run if retries remain)
```

### Invariants (enforced in code)

1. No task moves to `Passed` without verification artifacts where `passed = true`
2. No merge without all tasks in `Passed`
3. Worker count never exceeds `max_concurrent` (enforced by SKIP LOCKED scheduler)
4. Events are append-only (enforced by DB trigger — no UPDATE/DELETE)
5. Every state transition produces an event
6. Retry count bounded by `max_retries`; exceeded → `Abandoned`
7. One active run per task (previous run must be terminal before new run starts)
8. Event `seq` is monotonic per `instance_id` with unique constraint

## Database Schema

```sql
-- Projects: repo identity + default config (instance-independent)
CREATE TABLE orch_projects (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    repo_path   TEXT NOT NULL,
    config      JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Instances: process-bound runtime (one per project for MVP)
CREATE TABLE orch_instances (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES orch_projects(id),
    name            TEXT NOT NULL,
    pid             INT NOT NULL,
    bind_addr       TEXT NOT NULL,
    data_dir        TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_heartbeat  TIMESTAMPTZ NOT NULL DEFAULT now(),
    config          JSONB NOT NULL DEFAULT '{}',
    UNIQUE(project_id, name)
);

-- Cycles: one per raw prompt
CREATE TABLE orch_cycles (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    state       TEXT NOT NULL DEFAULT 'draft',
    prompt      TEXT NOT NULL,
    plan        JSONB,
    guardrails  JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tasks: work units (definition + mutable state)
CREATE TABLE orch_tasks (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cycle_id        UUID NOT NULL REFERENCES orch_cycles(id),
    instance_id     UUID NOT NULL REFERENCES orch_instances(id),
    phase           TEXT NOT NULL,
    ordinal         INT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'pending',
    title           TEXT NOT NULL,
    description     TEXT NOT NULL,
    acceptance      JSONB NOT NULL,
    max_retries     INT NOT NULL DEFAULT 3,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(cycle_id, phase, ordinal)
);

-- Task runs: each attempt is a separate row
CREATE TABLE orch_task_runs (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id         UUID NOT NULL REFERENCES orch_tasks(id),
    instance_id     UUID NOT NULL REFERENCES orch_instances(id),
    run_number      INT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'spawning',
    branch          TEXT,
    worktree_path   TEXT,
    pid             INT,
    prompt_sent     TEXT,
    output_json     JSONB,
    exit_code       INT,
    cost_usd        REAL,
    log_stdout      TEXT NOT NULL,       -- file path
    log_stderr      TEXT NOT NULL,       -- file path
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    UNIQUE(task_id, run_number)
);

-- Artifacts: verification evidence (append-only)
CREATE TABLE orch_artifacts (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id      UUID NOT NULL REFERENCES orch_task_runs(id),
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    kind        TEXT NOT NULL,
    content     JSONB NOT NULL,
    passed      BOOLEAN NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Events: append-only log with strict per-instance ordering
CREATE TABLE orch_events (
    id          BIGSERIAL PRIMARY KEY,
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    seq         BIGINT NOT NULL,
    timestamp   TIMESTAMPTZ NOT NULL DEFAULT now(),
    cycle_id    UUID REFERENCES orch_cycles(id),
    task_id     UUID REFERENCES orch_tasks(id),
    run_id      UUID REFERENCES orch_task_runs(id),
    event_type  TEXT NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    actor       TEXT NOT NULL DEFAULT 'orchestrator',
    UNIQUE(instance_id, seq)
);

-- Budgets: per-project
CREATE TABLE orch_budgets (
    project_id      UUID PRIMARY KEY REFERENCES orch_projects(id),
    max_concurrent  INT NOT NULL DEFAULT 3,
    max_retries     INT NOT NULL DEFAULT 3,
    daily_cost_cap  REAL,
    total_cost      REAL NOT NULL DEFAULT 0.0,
    today_cost      REAL NOT NULL DEFAULT 0.0,
    today_date      DATE NOT NULL DEFAULT CURRENT_DATE
);

-- Append-only enforcement
CREATE OR REPLACE FUNCTION deny_mutation() RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'table % is append-only: % denied', TG_TABLE_NAME, TG_OP;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER orch_events_no_update
    BEFORE UPDATE OR DELETE ON orch_events
    FOR EACH ROW EXECUTE FUNCTION deny_mutation();

CREATE TRIGGER orch_artifacts_no_update
    BEFORE UPDATE OR DELETE ON orch_artifacts
    FOR EACH ROW EXECUTE FUNCTION deny_mutation();

-- Indexes
CREATE INDEX idx_events_instance_seq ON orch_events(instance_id, seq);
CREATE INDEX idx_events_cycle ON orch_events(cycle_id, seq);
CREATE INDEX idx_events_type ON orch_events(event_type, timestamp);
CREATE INDEX idx_task_runs_task ON orch_task_runs(task_id, run_number);
CREATE INDEX idx_task_runs_state ON orch_task_runs(instance_id, state);
CREATE INDEX idx_tasks_cycle_state ON orch_tasks(cycle_id, state);
CREATE INDEX idx_instances_heartbeat ON orch_instances(last_heartbeat);
```

## Event Types

| Event | Payload | Trigger |
|-------|---------|---------|
| `ProjectCreated` | `{name, repo_path}` | User creates project |
| `InstanceStarted` | `{pid, bind_addr}` | Binary starts |
| `InstanceHeartbeat` | `{}` | Periodic (30s) |
| `CycleCreated` | `{prompt}` | User submits prompt |
| `CyclePlanned` | `{plan, guardrails}` | Planner completes |
| `CycleApproved` | `{}` | User approves in UI |
| `TaskScheduled` | `{branch, worktree}` | Scheduler claims task |
| `RunSpawned` | `{run_number, pid, prompt_sent}` | Process started |
| `WorkerOutput` | `{chunk}` | Log tailer reads stdout |
| `RunCompleted` | `{exit_code, output_json, cost}` | Process exited cleanly |
| `RunFailed` | `{exit_code, stderr_tail}` | Non-zero exit |
| `WorkerReattached` | `{run_id, pid}` | After restart, PID alive |
| `VerifyStarted` | `{checks: [...]}` | Pipeline begins |
| `VerifyResult` | `{kind, passed, output}` | Per-check result |
| `TaskPassed` | `{run_id, artifacts: [...]}` | All checks green |
| `TaskFailed` | `{run_id, reason}` | Check failed |
| `TaskRepairing` | `{new_run_number}` | Retry initiated |
| `TaskAbandoned` | `{total_runs, reason}` | Max retries exceeded |
| `MergeStarted` | `{source, target}` | Merge initiated |
| `MergeCompleted` | `{commit_sha}` | Clean merge |
| `MergeConflicted` | `{files: [...]}` | Conflict detected |
| `BudgetWarning` | `{usage, cap}` | >80% of cap |
| `BudgetExceeded` | `{usage, cap}` | Hard stop |

## Component: openclaw-orchestrator (pure library)

```
openclaw-orchestrator/src/
├── lib.rs              -- public API surface
├── model.rs            -- Rust types: Project, Instance, Cycle, Task, TaskRun, Artifact
├── state_machine.rs    -- CycleState, TaskState, RunState enums + validated transitions
├── events.rs           -- OrchestratorEvent enum, EventSink trait, PgEventStore impl
├── planner.rs          -- prompt → plan (invokes claude --print with schema prompt)
├── scheduler.rs        -- claim tasks with SKIP LOCKED, enforce concurrency
├── worker.rs           -- spawn/monitor/reattach claude CLI, tail log files
├── verifier.rs         -- VerifyCheck trait + built-in checks (cargo, npm, custom)
├── git.rs              -- GitProvider trait + GitCli impl (shell out to git)
├── merger.rs           -- merge flow: check readiness, merge, conflict detection
├── budget.rs           -- cost tracking, daily caps, throttle decisions
├── config.rs           -- OrchestratorConfig (TOML)
├── instance.rs         -- register/heartbeat/shutdown, startup replay
└── replay.rs           -- rebuild in-memory state from orch_events
```

### Key Abstractions

```rust
// Event bus — the spine of all state changes
pub trait EventSink: Send + Sync {
    fn emit(&self, event: OrchestratorEvent) -> Result<EventSeq>;
    fn replay(&self, instance_id: Uuid, since_seq: i64) -> Result<Vec<OrchestratorEvent>>;
    fn subscribe(&self) -> broadcast::Receiver<OrchestratorEvent>;
}

// State machine — all transitions validated, produces events
impl TaskState {
    pub fn transition(&self, trigger: TaskTrigger) -> Result<TaskState, InvalidTransition>;
}

// Git provider — trait for testability
pub trait GitProvider: Send + Sync {
    fn create_worktree(&self, repo: &Path, branch: &str) -> Result<PathBuf>;
    fn remove_worktree(&self, path: &Path) -> Result<()>;
    fn merge(&self, repo: &Path, source: &str, target: &str) -> Result<MergeResult>;
    fn diff_stat(&self, repo: &Path, base: &str, head: &str) -> Result<DiffStat>;
    fn current_sha(&self, repo: &Path) -> Result<String>;
}

// Verification check — trait for extensibility
pub trait VerifyCheck: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult>;
}
```

### Scheduler: SKIP LOCKED Concurrency

```rust
pub async fn claim_next_task(pool: &PgPool, instance_id: Uuid) -> Result<Option<Task>> {
    // Check budget allows another worker
    let budget = get_budget(pool, instance_id).await?;
    let active = count_active_runs(pool, instance_id).await?;
    if active >= budget.max_concurrent as i64 {
        return Ok(None); // at capacity
    }

    sqlx::query_as!(Task, r#"
        UPDATE orch_tasks SET state = 'scheduled', updated_at = now()
        WHERE id = (
            SELECT id FROM orch_tasks
            WHERE instance_id = $1 AND state = 'pending'
            ORDER BY ordinal
            LIMIT 1
            FOR UPDATE SKIP LOCKED
        )
        RETURNING *
    "#, instance_id)
    .fetch_optional(pool)
    .await
}
```

### Worker: Log Tailing into EventSink

```rust
/// Spawn a worker and start tailing its logs into the event bus.
pub async fn spawn_worker(
    task: &Task,
    run: &TaskRun,
    prompt: &str,
    event_sink: Arc<dyn EventSink>,
) -> Result<WorkerHandle> {
    let log_dir = PathBuf::from(&run.log_stdout).parent().unwrap().to_owned();
    std::fs::create_dir_all(&log_dir)?;

    let stdout_file = File::create(&run.log_stdout)?;
    let stderr_file = File::create(&run.log_stderr)?;

    let child = Command::new("claude")
        .args(["--print", "--output-format", "json", "--max-turns", "50"])
        .arg(prompt)
        .current_dir(run.worktree_path.as_ref().unwrap())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()?;

    let pid = child.id();
    let handle = WorkerHandle { run_id: run.id, pid, child };

    // Start server-side log tailer — emits WorkerOutput events into the bus
    let stdout_path = run.log_stdout.clone();
    let run_id = run.id;
    let instance_id = task.instance_id;
    tokio::spawn(async move {
        tail_and_emit(stdout_path, instance_id, run_id, event_sink).await;
    });

    Ok(handle)
}

/// Tail a log file and emit each chunk as a WorkerOutput event.
async fn tail_and_emit(
    path: String,
    instance_id: Uuid,
    run_id: Uuid,
    sink: Arc<dyn EventSink>,
) {
    let mut pos = 0u64;
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match read_new_bytes(&path, &mut pos) {
            Ok(Some(chunk)) => {
                let _ = sink.emit(OrchestratorEvent::WorkerOutput {
                    instance_id,
                    run_id,
                    payload: serde_json::json!({ "chunk": chunk }),
                });
            }
            Ok(None) => {} // no new data
            Err(_) => break, // file gone or process done
        }
    }
}
```

### Restart Reattach (corrected)

```rust
pub async fn reattach_workers(
    pool: &PgPool,
    instance_id: Uuid,
    sink: Arc<dyn EventSink>,
) {
    let orphans = sqlx::query_as!(TaskRun,
        "SELECT * FROM orch_task_runs WHERE instance_id = $1 AND state = 'running'",
        instance_id
    ).fetch_all(pool).await.unwrap();

    for run in orphans {
        let pid = run.pid.unwrap() as u32;
        if is_pid_alive(pid) {
            // Emit reattach event, resume server-side log tailing
            sink.emit(WorkerReattached { run_id: run.id, pid });
            let sink_clone = sink.clone();
            tokio::spawn(async move {
                tail_and_emit(run.log_stdout, instance_id, run.id, sink_clone).await;
            });
        } else {
            // Process died — emit failure, maybe retry
            sink.emit(WorkerFailed {
                run_id: run.id,
                reason: "process died during orchestrator restart".into(),
            });
            update_run_state(pool, run.id, "failed").await;
            maybe_retry_task(pool, &run, &sink).await;
        }
    }
}
```

## Component: openclaw-ui (binary crate)

```
openclaw-ui/src/
├── main.rs         -- startup, config, Actix HttpServer, instance registration
├── api/
│   ├── projects.rs -- POST/GET /api/projects
│   ├── cycles.rs   -- POST /api/cycles, GET /api/cycles/{id}, POST .../approve
│   ├── tasks.rs    -- GET /api/tasks?cycle_id=..., GET /api/tasks/{id}
│   ├── runs.rs     -- GET /api/runs/{id}, GET /api/runs/{id}/logs (streaming)
│   ├── merge.rs    -- POST /api/merge/{cycle_id}
│   ├── budgets.rs  -- GET/PUT /api/budgets
│   └── events.rs   -- GET /api/events?since_seq=N (polling fallback)
├── ws.rs           -- WebSocket: subscribe by instance_id, push events by seq
├── auth.rs         -- Token auth middleware (constant-time compare, required for LAN)
├── frontend.rs     -- include_str!() embedded HTML/JS/CSS
└── metrics.rs      -- /metrics Prometheus endpoint
```

### WebSocket Protocol

```
Client → Server:  { "subscribe": { "instance_id": "...", "since_seq": 0 } }
Server → Client:  { "seq": 1, "event_type": "CycleCreated", "payload": {...} }
                  { "seq": 2, "event_type": "TaskScheduled", "payload": {...} }
                  ...
                  (events filtered by instance_id, ordered by seq)
                  (on reconnect, client sends last seen seq → server replays gap)
```

### Frontend (vanilla TS + esbuild)

```
ui-src/
├── index.html
├── app.ts          -- router, WS connection manager, event dispatcher
├── views/
│   ├── projects.ts -- project list, create project form
│   ├── cycle.ts    -- cycle detail: plan viewer, phase/task tree, approve button
│   ├── worker.ts   -- live log stream, command history, file change summary
│   ├── merge.ts    -- diff display, check results, merge/abort controls
│   └── budgets.ts  -- cost tracking, concurrency caps
├── store.ts        -- client-side event replay: events[] → derived view state
├── types.ts        -- API types (kept in sync with Rust models)
├── utils.ts        -- DOM helpers, formatters, WS reconnect logic
└── style.css
```

`store.ts` replays events to build view state. On WS reconnect, sends `since_seq` and
replays the gap. No REST polling needed for live state.

## Component: Verifier

Module within `openclaw-orchestrator`. Configured per-project via `orch_projects.config`:

```json
{
  "verify": {
    "checks": [
      { "type": "cargo_check" },
      { "type": "cargo_test" },
      { "type": "cargo_fmt" },
      { "type": "cargo_clippy" },
      { "type": "custom", "cmd": "npm", "args": ["run", "typecheck"] }
    ]
  }
}
```

Each check implements `VerifyCheck` trait. Results stored as `orch_artifacts`.
Task cannot transition to `Passed` unless all artifacts have `passed = true`.

## Component: Git Manager

Module within `openclaw-orchestrator`. Trait-based with CLI implementation:

```rust
pub struct GitCli;

impl GitProvider for GitCli {
    fn create_worktree(&self, repo: &Path, branch: &str) -> Result<PathBuf>;
    fn remove_worktree(&self, path: &Path) -> Result<()>;
    fn merge(&self, repo: &Path, source: &str, target: &str) -> Result<MergeResult>;
    fn diff_stat(&self, repo: &Path, base: &str, head: &str) -> Result<DiffStat>;
    fn current_sha(&self, repo: &Path) -> Result<String>;
}
```

Worktrees created at `<repo>/../worktrees/<branch>/`. Cleaned up after merge or abandon.

## Gateway Integration

New commands added to `openclaw-gateway` (Telegram/Discord):

| Command | Action |
|---------|--------|
| `/projects` | List projects |
| `/cycle <project> <prompt>` | Start new upgrade cycle |
| `/status [project]` | Cycle/worker status |
| `/approve <cycle_id>` | Approve a plan |
| `/workers` | List active workers |

Commands call `openclaw-orchestrator` functions directly (library dependency, no HTTP hop).

## Configuration (TOML)

```toml
[orchestrator]
instance_name = "dev-orch-1"
bind = "127.0.0.1:3130"
data_dir = "/var/lib/openclaw-orch"
auth_token = "change-me"

[orchestrator.defaults]
max_concurrent_workers = 3
max_retries = 3
daily_cost_cap_usd = 50.0
claude_binary = "claude"
claude_max_turns = 50

[orchestrator.security]
allow_lan_bind = false
redact_env_patterns = ["KEY", "SECRET", "TOKEN", "PASSWORD"]
```

## End-to-End Workflow

```
User                    Orchestrator              Claude CLI          Git
  │                         │                        │                │
  ├─ POST /api/cycles ─────►│                        │                │
  │  {project, prompt}      │                        │                │
  │                         ├─ emit CycleCreated     │                │
  │                         ├─ spawn planner ────────►│                │
  │                         │  claude --print         │                │
  │                         │  "generate plan..."     │                │
  │                         │◄── plan JSON ───────────┤                │
  │                         ├─ emit CyclePlanned      │                │
  │◄── WS: CyclePlanned ───┤                        │                │
  │                         │                        │                │
  ├─ POST .../approve ─────►├─ emit CycleApproved    │                │
  │                         │                        │                │
  │                         ├─ scheduler loop:       │                │
  │                         │  claim task (SKIP LOCKED)               │
  │                         │                        │                ├─ create worktree
  │                         │                        │                ├─ create branch
  │                         ├─ emit RunSpawned       │                │
  │                         ├─ spawn worker ─────────►│                │
  │                         │  claude --print         │ ── edits ────►│
  │                         │  (in worktree dir)      │                │
  │                         │                        │                │
  │                         ├─ tail stdout.log       │                │
  │                         ├─ emit WorkerOutput ───►│ (to WS clients)│
  │◄── WS: WorkerOutput ───┤                        │                │
  │                         │                        │                │
  │                         │◄── exit 0 ─────────────┤                │
  │                         ├─ emit RunCompleted      │                │
  │                         │                        │                │
  │                         ├─ run verifier:         │                │
  │                         │  cargo check / test / fmt               │
  │                         ├─ emit VerifyResult (×N) │                │
  │                         ├─ emit TaskPassed        │                │
  │◄── WS: TaskPassed ─────┤                        │                │
  │                         │                        │                │
  │  (all tasks passed)     │                        │                │
  ├─ POST /api/merge ──────►│                        │                ├─ merge branch
  │                         ├─ emit MergeCompleted   │                ├─ remove worktree
  │◄── WS: MergeCompleted ─┤                        │                │
```

## Security Model

- Default bind: `127.0.0.1` only
- LAN bind requires `allow_lan_bind = true` AND `auth_token` set
- Token auth: constant-time comparison on all API/WS requests
- Secrets redaction: env vars matching patterns stripped from logs/events
- Per-project command allowlist for verification checks
- Workers run in worktrees (filesystem isolation from main branch)
- Append-only event log (DB-enforced, no tampering)

## Multi-Instance Operation

Each instance:
- Has its own `bind_addr` (non-conflicting ports)
- Has its own `data_dir` (non-conflicting log files)
- Registers in `orch_instances` on startup
- All queries/events scoped by `instance_id`
- UI/WS partitioned by `instance_id`

Example: two projects running simultaneously:
```
openclaw-orch --config aitan-liquidator.toml  # port 3130
openclaw-orch --config openclaw-rs.toml       # port 3131
```
