# SDLC Orchestrator Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a local-first AI SDLC Orchestrator that manages parallel Claude Code workers across git worktrees, with verification gates, event-sourced state, and a web UI control plane.

**Architecture:** Two new crates in the openclaw-rs workspace. `openclaw-orchestrator` is a pure domain library (no HTTP types) with event-sourced state, SKIP LOCKED scheduling, and git CLI wrappers. `openclaw-ui` is a thin Axum binary that serves a REST API, WebSocket, and embedded vanilla TypeScript frontend. Workers are `claude --print --output-format json` subprocesses with log files tailed server-side into the event bus.

**Tech Stack:** Rust 2021, sqlx 0.8 (Postgres), Axum 0.8, tokio 1, tokio-tungstenite, esbuild, vanilla TypeScript. Existing workspace deps reused where possible.

**Design doc:** `docs/plans/2026-02-27-sdlc-orchestrator-design.md`

**Note:** The existing gateway uses Axum 0.8 (not Actix). The UI binary uses Axum for consistency.

---

## Phase 1: Foundation

### Task 1: Create crate skeletons and wire workspace

**Files:**
- Create: `crates/openclaw-orchestrator/Cargo.toml`
- Create: `crates/openclaw-orchestrator/src/lib.rs`
- Create: `crates/openclaw-ui/Cargo.toml`
- Create: `crates/openclaw-ui/src/main.rs`
- Modify: `Cargo.toml` (workspace root — add members)

**Step 1: Create openclaw-orchestrator crate**

`crates/openclaw-orchestrator/Cargo.toml`:
```toml
[package]
name = "openclaw-orchestrator"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
openclaw-db = { path = "../openclaw-db" }
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "chrono", "uuid", "json"] }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
```

`crates/openclaw-orchestrator/src/lib.rs`:
```rust
pub mod model;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {
        assert!(true);
    }
}
```

`crates/openclaw-orchestrator/src/model.rs`:
```rust
// Placeholder — populated in Task 3
```

**Step 2: Create openclaw-ui crate**

`crates/openclaw-ui/Cargo.toml`:
```toml
[package]
name = "openclaw-ui"
version.workspace = true
edition.workspace = true
license.workspace = true

[[bin]]
name = "openclaw-orch"
path = "src/main.rs"

[dependencies]
openclaw-orchestrator = { path = "../openclaw-orchestrator" }
openclaw-db = { path = "../openclaw-db" }
axum = "0.8"
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
tower-http = { version = "0.6", features = ["cors"] }
```

`crates/openclaw-ui/src/main.rs`:
```rust
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    info!("openclaw-orch starting");
    Ok(())
}
```

**Step 3: Add both crates to workspace**

In root `Cargo.toml`, add to `[workspace] members`:
```toml
members = [
    "crates/openclaw-cli",
    "crates/openclaw-core",
    "crates/openclaw-agent",
    "crates/openclaw-gateway",
    "crates/openclaw-db",
    "crates/openclaw-mcp",
    "crates/openclaw-orchestrator",
    "crates/openclaw-ui",
]
```

**Step 4: Verify it compiles**

Run: `cargo check -p openclaw-orchestrator -p openclaw-ui`
Expected: compiles with no errors

**Step 5: Run the placeholder test**

Run: `cargo test -p openclaw-orchestrator`
Expected: 1 test passed

**Step 6: Commit**

```bash
git add crates/openclaw-orchestrator/ crates/openclaw-ui/ Cargo.toml Cargo.lock
git commit -m "feat(orch): scaffold orchestrator and UI crates"
```

---

### Task 2: Database migration for orch_* tables

**Files:**
- Create: `migrations/002_orchestrator.sql`

**Step 1: Write the migration**

`migrations/002_orchestrator.sql`:
```sql
-- ============================================================
-- SDLC ORCHESTRATOR TABLES
-- ============================================================

-- Projects: repo identity + default config (instance-independent)
CREATE TABLE IF NOT EXISTS orch_projects (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name        TEXT NOT NULL UNIQUE,
    repo_path   TEXT NOT NULL,
    config      JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Instances: process-bound runtime (one per project for MVP)
CREATE TABLE IF NOT EXISTS orch_instances (
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

-- Cycles: one per raw prompt/upgrade request
CREATE TABLE IF NOT EXISTS orch_cycles (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    state       TEXT NOT NULL DEFAULT 'draft',
    prompt      TEXT NOT NULL,
    plan        JSONB,
    guardrails  JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Tasks: work units with acceptance criteria
CREATE TABLE IF NOT EXISTS orch_tasks (
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
CREATE TABLE IF NOT EXISTS orch_task_runs (
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
    log_stdout      TEXT NOT NULL,
    log_stderr      TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    UNIQUE(task_id, run_number)
);

-- Artifacts: verification evidence (append-only)
CREATE TABLE IF NOT EXISTS orch_artifacts (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id      UUID NOT NULL REFERENCES orch_task_runs(id),
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    kind        TEXT NOT NULL,
    content     JSONB NOT NULL,
    passed      BOOLEAN NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Events: append-only log with strict per-instance ordering
CREATE TABLE IF NOT EXISTS orch_events (
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
CREATE TABLE IF NOT EXISTS orch_budgets (
    project_id      UUID PRIMARY KEY REFERENCES orch_projects(id),
    max_concurrent  INT NOT NULL DEFAULT 3,
    max_retries     INT NOT NULL DEFAULT 3,
    daily_cost_cap  REAL,
    total_cost      REAL NOT NULL DEFAULT 0.0,
    today_cost      REAL NOT NULL DEFAULT 0.0,
    today_date      DATE NOT NULL DEFAULT CURRENT_DATE
);

-- Append-only enforcement triggers
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
CREATE INDEX IF NOT EXISTS idx_orch_events_instance_seq ON orch_events(instance_id, seq);
CREATE INDEX IF NOT EXISTS idx_orch_events_cycle ON orch_events(cycle_id, seq);
CREATE INDEX IF NOT EXISTS idx_orch_events_type ON orch_events(event_type, timestamp);
CREATE INDEX IF NOT EXISTS idx_orch_task_runs_task ON orch_task_runs(task_id, run_number);
CREATE INDEX IF NOT EXISTS idx_orch_task_runs_state ON orch_task_runs(instance_id, state);
CREATE INDEX IF NOT EXISTS idx_orch_tasks_cycle_state ON orch_tasks(cycle_id, state);
CREATE INDEX IF NOT EXISTS idx_orch_instances_heartbeat ON orch_instances(last_heartbeat);
```

**Step 2: Verify migration applies**

Run: `psql -U openclaw -d openclaw -f migrations/002_orchestrator.sql`
Expected: CREATE TABLE (×8), CREATE FUNCTION, CREATE TRIGGER (×2), CREATE INDEX (×7)

**Step 3: Verify append-only triggers work**

Run:
```sql
-- Insert a test project + instance + event, then try to UPDATE the event
INSERT INTO orch_projects (name, repo_path) VALUES ('test-proj', '/tmp/test');
INSERT INTO orch_instances (project_id, name, pid, bind_addr, data_dir)
    SELECT id, 'test', 1, '127.0.0.1:3130', '/tmp/orch' FROM orch_projects WHERE name = 'test-proj';
INSERT INTO orch_events (instance_id, seq, event_type, payload)
    SELECT id, 1, 'TestEvent', '{}' FROM orch_instances WHERE name = 'test';
-- This must fail:
UPDATE orch_events SET event_type = 'Tampered' WHERE seq = 1;
```
Expected: ERROR: table orch_events is append-only: UPDATE denied

**Step 4: Clean up test data and commit**

```sql
-- Clean up (delete in reverse FK order; triggers only block events/artifacts)
DELETE FROM orch_events;  -- will fail due to trigger, so:
-- Drop trigger temporarily for cleanup, or just drop and recreate tables
-- For dev: just use a fresh DB or keep test data
```

```bash
git add migrations/002_orchestrator.sql
git commit -m "feat(orch): add orchestrator database migration"
```

---

### Task 3: Domain model types

**Files:**
- Create: `crates/openclaw-orchestrator/src/model.rs`

**Step 1: Write model types with serde derive**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// === Projects ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub repo_path: String,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProject {
    pub name: String,
    pub repo_path: String,
    pub config: Option<serde_json::Value>,
}

// === Instances ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Instance {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub pid: i32,
    pub bind_addr: String,
    pub data_dir: String,
    pub started_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub config: serde_json::Value,
}

// === Cycles ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Cycle {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub state: String,
    pub prompt: String,
    pub plan: Option<serde_json::Value>,
    pub guardrails: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateCycle {
    pub prompt: String,
}

// === Tasks ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Task {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub instance_id: Uuid,
    pub phase: String,
    pub ordinal: i32,
    pub state: String,
    pub title: String,
    pub description: String,
    pub acceptance: serde_json::Value,
    pub max_retries: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// === Task Runs ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TaskRun {
    pub id: Uuid,
    pub task_id: Uuid,
    pub instance_id: Uuid,
    pub run_number: i32,
    pub state: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub pid: Option<i32>,
    pub prompt_sent: Option<String>,
    pub output_json: Option<serde_json::Value>,
    pub exit_code: Option<i32>,
    pub cost_usd: Option<f32>,
    pub log_stdout: String,
    pub log_stderr: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

// === Artifacts ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Artifact {
    pub id: Uuid,
    pub run_id: Uuid,
    pub instance_id: Uuid,
    pub kind: String,
    pub content: serde_json::Value,
    pub passed: bool,
    pub created_at: DateTime<Utc>,
}

// === Events ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Event {
    pub id: i64,
    pub instance_id: Uuid,
    pub seq: i64,
    pub timestamp: DateTime<Utc>,
    pub cycle_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub actor: String,
}

// === Budgets ===

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Budget {
    pub project_id: Uuid,
    pub max_concurrent: i32,
    pub max_retries: i32,
    pub daily_cost_cap: Option<f32>,
    pub total_cost: f32,
    pub today_cost: f32,
    pub today_date: chrono::NaiveDate,
}
```

**Step 2: Verify it compiles**

Run: `cargo check -p openclaw-orchestrator`
Expected: no errors

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/src/model.rs
git commit -m "feat(orch): add domain model types"
```

---

### Task 4: State machines with validated transitions

**Files:**
- Create: `crates/openclaw-orchestrator/src/state_machine.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs` (add module)

**Step 1: Write failing tests for state transitions**

In `state_machine.rs`, write tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_valid_transitions() {
        assert_eq!(CycleState::Draft.transition(CycleTrigger::Planned).unwrap(), CycleState::Planned);
        assert_eq!(CycleState::Planned.transition(CycleTrigger::Approved).unwrap(), CycleState::Approved);
        assert_eq!(CycleState::Approved.transition(CycleTrigger::Started).unwrap(), CycleState::Running);
        assert_eq!(CycleState::Running.transition(CycleTrigger::AllTasksPassed).unwrap(), CycleState::Verifying);
        assert_eq!(CycleState::Verifying.transition(CycleTrigger::VerifyPassed).unwrap(), CycleState::Merging);
        assert_eq!(CycleState::Merging.transition(CycleTrigger::MergeDone).unwrap(), CycleState::Done);
    }

    #[test]
    fn cycle_invalid_transition() {
        assert!(CycleState::Draft.transition(CycleTrigger::Approved).is_err());
        assert!(CycleState::Done.transition(CycleTrigger::Started).is_err());
    }

    #[test]
    fn cycle_failure_transitions() {
        assert_eq!(CycleState::Running.transition(CycleTrigger::Failed).unwrap(), CycleState::Failed);
        assert_eq!(CycleState::Failed.transition(CycleTrigger::Retry).unwrap(), CycleState::Running);
        assert_eq!(CycleState::Merging.transition(CycleTrigger::Conflicted).unwrap(), CycleState::Conflicted);
    }

    #[test]
    fn task_valid_transitions() {
        assert_eq!(TaskState::Pending.transition(TaskTrigger::Scheduled).unwrap(), TaskState::Scheduled);
        assert_eq!(TaskState::Scheduled.transition(TaskTrigger::Running).unwrap(), TaskState::Running);
        assert_eq!(TaskState::Running.transition(TaskTrigger::VerifyStarted).unwrap(), TaskState::Verifying);
        assert_eq!(TaskState::Verifying.transition(TaskTrigger::Passed).unwrap(), TaskState::Passed);
        assert_eq!(TaskState::Passed.transition(TaskTrigger::Merged).unwrap(), TaskState::Merged);
    }

    #[test]
    fn task_failure_and_repair() {
        assert_eq!(TaskState::Running.transition(TaskTrigger::Failed).unwrap(), TaskState::Failed);
        assert_eq!(TaskState::Failed.transition(TaskTrigger::Repair).unwrap(), TaskState::Repairing);
        assert_eq!(TaskState::Repairing.transition(TaskTrigger::Running).unwrap(), TaskState::Running);
    }

    #[test]
    fn task_abandon() {
        assert_eq!(TaskState::Failed.transition(TaskTrigger::Abandon).unwrap(), TaskState::Abandoned);
        // Abandoned is terminal
        assert!(TaskState::Abandoned.transition(TaskTrigger::Repair).is_err());
    }

    #[test]
    fn run_valid_transitions() {
        assert_eq!(RunState::Spawning.transition(RunTrigger::Started).unwrap(), RunState::Running);
        assert_eq!(RunState::Running.transition(RunTrigger::Completed).unwrap(), RunState::Completed);
        assert_eq!(RunState::Running.transition(RunTrigger::Failed).unwrap(), RunState::Failed);
    }

    #[test]
    fn run_reattach() {
        assert_eq!(RunState::Running.transition(RunTrigger::Reattached).unwrap(), RunState::Running);
    }

    #[test]
    fn state_roundtrip_from_str() {
        assert_eq!(CycleState::from_str("running").unwrap(), CycleState::Running);
        assert_eq!(CycleState::Running.as_str(), "running");
        assert_eq!(TaskState::from_str("repairing").unwrap(), TaskState::Repairing);
        assert!(CycleState::from_str("invalid").is_err());
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p openclaw-orchestrator`
Expected: FAIL — types don't exist yet

**Step 3: Implement state machines**

```rust
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CycleState {
    Draft,
    Planned,
    Approved,
    Running,
    Verifying,
    Merging,
    Done,
    Failed,
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleTrigger {
    Planned,
    Approved,
    Started,
    AllTasksPassed,
    VerifyPassed,
    MergeDone,
    Failed,
    Retry,
    Conflicted,
}

impl CycleState {
    pub fn transition(self, trigger: CycleTrigger) -> Result<CycleState, InvalidTransition> {
        use CycleState::*;
        use CycleTrigger as T;
        match (self, trigger) {
            (Draft, T::Planned) => Ok(Planned),
            (Planned, T::Approved) => Ok(Approved),
            (Approved, T::Started) => Ok(Running),
            (Running, T::AllTasksPassed) => Ok(Verifying),
            (Running, T::Failed) => Ok(Failed),
            (Verifying, T::VerifyPassed) => Ok(Merging),
            (Verifying, T::Failed) => Ok(Failed),
            (Merging, T::MergeDone) => Ok(Done),
            (Merging, T::Conflicted) => Ok(Conflicted),
            (Failed, T::Retry) => Ok(Running),
            _ => Err(InvalidTransition {
                from: format!("{:?}", self),
                trigger: format!("{:?}", trigger),
            }),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Planned => "planned",
            Self::Approved => "approved",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Merging => "merging",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Conflicted => "conflicted",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, InvalidTransition> {
        match s {
            "draft" => Ok(Self::Draft),
            "planned" => Ok(Self::Planned),
            "approved" => Ok(Self::Approved),
            "running" => Ok(Self::Running),
            "verifying" => Ok(Self::Verifying),
            "merging" => Ok(Self::Merging),
            "done" => Ok(Self::Done),
            "failed" => Ok(Self::Failed),
            "conflicted" => Ok(Self::Conflicted),
            _ => Err(InvalidTransition {
                from: s.to_string(),
                trigger: "parse".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Scheduled,
    Running,
    Verifying,
    Passed,
    Merged,
    Failed,
    Repairing,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTrigger {
    Scheduled,
    Running,
    VerifyStarted,
    Passed,
    Merged,
    Failed,
    Repair,
    Abandon,
}

impl TaskState {
    pub fn transition(self, trigger: TaskTrigger) -> Result<TaskState, InvalidTransition> {
        use TaskState::*;
        use TaskTrigger as T;
        match (self, trigger) {
            (Pending, T::Scheduled) => Ok(Scheduled),
            (Scheduled, T::Running) => Ok(Running),
            (Running, T::VerifyStarted) => Ok(Verifying),
            (Running, T::Failed) => Ok(Failed),
            (Verifying, T::Passed) => Ok(Passed),
            (Verifying, T::Failed) => Ok(Failed),
            (Passed, T::Merged) => Ok(Merged),
            (Failed, T::Repair) => Ok(Repairing),
            (Failed, T::Abandon) => Ok(Abandoned),
            (Repairing, T::Running) => Ok(Running),
            _ => Err(InvalidTransition {
                from: format!("{:?}", self),
                trigger: format!("{:?}", trigger),
            }),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Scheduled => "scheduled",
            Self::Running => "running",
            Self::Verifying => "verifying",
            Self::Passed => "passed",
            Self::Merged => "merged",
            Self::Failed => "failed",
            Self::Repairing => "repairing",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, InvalidTransition> {
        match s {
            "pending" => Ok(Self::Pending),
            "scheduled" => Ok(Self::Scheduled),
            "running" => Ok(Self::Running),
            "verifying" => Ok(Self::Verifying),
            "passed" => Ok(Self::Passed),
            "merged" => Ok(Self::Merged),
            "failed" => Ok(Self::Failed),
            "repairing" => Ok(Self::Repairing),
            "abandoned" => Ok(Self::Abandoned),
            _ => Err(InvalidTransition {
                from: s.to_string(),
                trigger: "parse".to_string(),
            }),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Merged | Self::Abandoned)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Spawning,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTrigger {
    Started,
    Completed,
    Failed,
    Reattached,
}

impl RunState {
    pub fn transition(self, trigger: RunTrigger) -> Result<RunState, InvalidTransition> {
        use RunState::*;
        use RunTrigger as T;
        match (self, trigger) {
            (Spawning, T::Started) => Ok(Running),
            (Running, T::Completed) => Ok(Completed),
            (Running, T::Failed) => Ok(Failed),
            (Running, T::Reattached) => Ok(Running),
            _ => Err(InvalidTransition {
                from: format!("{:?}", self),
                trigger: format!("{:?}", trigger),
            }),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Spawning => "spawning",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Result<Self, InvalidTransition> {
        match s {
            "spawning" => Ok(Self::Spawning),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(InvalidTransition {
                from: s.to_string(),
                trigger: "parse".to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InvalidTransition {
    pub from: String,
    pub trigger: String,
}

impl fmt::Display for InvalidTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid transition from '{}' via '{}'", self.from, self.trigger)
    }
}

impl std::error::Error for InvalidTransition {}
```

**Step 4: Add module to lib.rs**

Add `pub mod state_machine;` to `lib.rs`.

**Step 5: Run tests**

Run: `cargo test -p openclaw-orchestrator`
Expected: all tests pass

**Step 6: Commit**

```bash
git add crates/openclaw-orchestrator/src/
git commit -m "feat(orch): add state machines with validated transitions"
```

---

### Task 5: Event system (EventSink trait + PgEventStore)

**Files:**
- Create: `crates/openclaw-orchestrator/src/events.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Write the event types and EventSink trait**

```rust
use crate::model::Event;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

/// All orchestrator events. Each variant carries its own payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload")]
pub enum OrchestratorEvent {
    ProjectCreated { name: String, repo_path: String },
    InstanceStarted { pid: i32, bind_addr: String },
    InstanceHeartbeat,
    CycleCreated { prompt: String },
    CyclePlanned { plan: serde_json::Value, guardrails: serde_json::Value },
    CycleApproved,
    TaskScheduled { branch: String, worktree: String },
    RunSpawned { run_number: i32, pid: i32, prompt_sent: String },
    WorkerOutput { chunk: String },
    RunCompleted { exit_code: i32, output_json: serde_json::Value, cost_usd: f32 },
    RunFailed { exit_code: Option<i32>, stderr_tail: String },
    WorkerReattached { pid: i32 },
    VerifyStarted { checks: Vec<String> },
    VerifyResult { kind: String, passed: bool, output: String },
    TaskPassed { run_id: Uuid, artifact_ids: Vec<Uuid> },
    TaskFailed { run_id: Uuid, reason: String },
    TaskRepairing { new_run_number: i32 },
    TaskAbandoned { total_runs: i32, reason: String },
    MergeStarted { source: String, target: String },
    MergeCompleted { commit_sha: String },
    MergeConflicted { files: Vec<String> },
    BudgetWarning { usage: f32, cap: f32 },
    BudgetExceeded { usage: f32, cap: f32 },
}

/// Context for an event: which entities it relates to.
#[derive(Debug, Clone, Default)]
pub struct EventContext {
    pub cycle_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub actor: String,
}

/// The event bus trait. Orchestrator core depends on this, not on Postgres directly.
pub trait EventSink: Send + Sync {
    fn emit(
        &self,
        event: OrchestratorEvent,
        ctx: EventContext,
    ) -> impl std::future::Future<Output = Result<i64>> + Send;

    fn replay(
        &self,
        since_seq: i64,
    ) -> impl std::future::Future<Output = Result<Vec<Event>>> + Send;

    fn subscribe(&self) -> broadcast::Receiver<(i64, OrchestratorEvent)>;
}

/// Postgres-backed event store with monotonic seq per instance.
pub struct PgEventStore {
    pool: PgPool,
    instance_id: Uuid,
    seq: AtomicI64,
    tx: broadcast::Sender<(i64, OrchestratorEvent)>,
}

impl PgEventStore {
    pub async fn new(pool: PgPool, instance_id: Uuid) -> Result<Self> {
        // Load the last seq for this instance to resume monotonic ordering
        let last_seq: Option<i64> = sqlx::query_scalar(
            "SELECT MAX(seq) FROM orch_events WHERE instance_id = $1"
        )
        .bind(instance_id)
        .fetch_one(&pool)
        .await?;

        let (tx, _) = broadcast::channel(1024);

        Ok(Self {
            pool,
            instance_id,
            seq: AtomicI64::new(last_seq.unwrap_or(0)),
            tx,
        })
    }

    fn next_seq(&self) -> i64 {
        self.seq.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl EventSink for PgEventStore {
    async fn emit(&self, event: OrchestratorEvent, ctx: EventContext) -> Result<i64> {
        let seq = self.next_seq();
        let event_type = event_type_name(&event);
        let payload = serde_json::to_value(&event)?;

        sqlx::query(
            r#"INSERT INTO orch_events
               (instance_id, seq, cycle_id, task_id, run_id, event_type, payload, actor)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#
        )
        .bind(self.instance_id)
        .bind(seq)
        .bind(ctx.cycle_id)
        .bind(ctx.task_id)
        .bind(ctx.run_id)
        .bind(&event_type)
        .bind(&payload)
        .bind(&ctx.actor)
        .execute(&self.pool)
        .await?;

        // Broadcast to in-process subscribers (WS handlers)
        let _ = self.tx.send((seq, event));

        Ok(seq)
    }

    async fn replay(&self, since_seq: i64) -> Result<Vec<Event>> {
        let events = sqlx::query_as::<_, Event>(
            "SELECT * FROM orch_events WHERE instance_id = $1 AND seq > $2 ORDER BY seq"
        )
        .bind(self.instance_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(events)
    }

    fn subscribe(&self) -> broadcast::Receiver<(i64, OrchestratorEvent)> {
        self.tx.subscribe()
    }
}

fn event_type_name(event: &OrchestratorEvent) -> String {
    // Extract the variant name from the enum
    let json = serde_json::to_value(event).unwrap();
    json.get("event_type").unwrap().as_str().unwrap().to_string()
}
```

**Step 2: Add module to lib.rs and verify**

Run: `cargo check -p openclaw-orchestrator`
Expected: compiles

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/src/events.rs crates/openclaw-orchestrator/src/lib.rs
git commit -m "feat(orch): add event system with PgEventStore"
```

---

### Task 6: Git provider (trait + CLI implementation)

**Files:**
- Create: `crates/openclaw-orchestrator/src/git.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_test_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        Command::new("git").args(["init"]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(dir.path()).output().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();
        Command::new("git").args(["add", "."]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(dir.path()).output().unwrap();
        dir
    }

    #[test]
    fn current_sha_returns_hash() {
        let repo = init_test_repo();
        let git = GitCli;
        let sha = git.current_sha(repo.path()).unwrap();
        assert_eq!(sha.len(), 40);
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn create_and_remove_worktree() {
        let repo = init_test_repo();
        let git = GitCli;
        let wt = git.create_worktree(repo.path(), "feat-test").unwrap();
        assert!(wt.exists());
        assert!(wt.join("README.md").exists());
        git.remove_worktree(&wt).unwrap();
        assert!(!wt.exists());
    }

    #[test]
    fn diff_stat_empty_on_same_branch() {
        let repo = init_test_repo();
        let git = GitCli;
        let sha = git.current_sha(repo.path()).unwrap();
        let stat = git.diff_stat(repo.path(), &sha, "HEAD").unwrap();
        assert_eq!(stat.files_changed, 0);
    }

    #[test]
    fn merge_clean() {
        let repo = init_test_repo();
        let git = GitCli;
        // Create a branch with a change
        let wt = git.create_worktree(repo.path(), "feat-merge").unwrap();
        std::fs::write(wt.join("new.txt"), "content").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&wt).output().unwrap();
        Command::new("git").args(["commit", "-m", "add new.txt"]).current_dir(&wt).output().unwrap();
        git.remove_worktree(&wt).unwrap();

        // Merge back to main
        let result = git.merge(repo.path(), "feat-merge", "main").unwrap();
        assert!(matches!(result, MergeResult::Clean { .. }));
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p openclaw-orchestrator -- git`
Expected: FAIL — types don't exist yet

**Step 3: Implement GitProvider trait and GitCli**

Add `tempfile` to dev-dependencies in `crates/openclaw-orchestrator/Cargo.toml`:
```toml
[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
tempfile = "3"
```

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MergeResult {
    Clean { commit_sha: String },
    Conflict { files: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiffStat {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<String>,
}

pub trait GitProvider: Send + Sync {
    fn create_worktree(&self, repo: &Path, branch: &str) -> Result<PathBuf>;
    fn remove_worktree(&self, path: &Path) -> Result<()>;
    fn merge(&self, repo: &Path, source: &str, target: &str) -> Result<MergeResult>;
    fn diff_stat(&self, repo: &Path, base: &str, head: &str) -> Result<DiffStat>;
    fn current_sha(&self, repo: &Path) -> Result<String>;
}

pub struct GitCli;

impl GitProvider for GitCli {
    fn create_worktree(&self, repo: &Path, branch: &str) -> Result<PathBuf> {
        let worktrees_dir = repo.parent()
            .context("repo has no parent dir")?
            .join("worktrees");
        std::fs::create_dir_all(&worktrees_dir)?;
        let wt_path = worktrees_dir.join(branch);

        let output = Command::new("git")
            .args(["worktree", "add", wt_path.to_str().unwrap(), "-b", branch])
            .current_dir(repo)
            .output()
            .context("git worktree add failed")?;

        if !output.status.success() {
            anyhow::bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(wt_path)
    }

    fn remove_worktree(&self, path: &Path) -> Result<()> {
        let output = Command::new("git")
            .args(["worktree", "remove", path.to_str().unwrap(), "--force"])
            .output()
            .context("git worktree remove failed")?;

        if !output.status.success() {
            anyhow::bail!(
                "git worktree remove failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        Ok(())
    }

    fn merge(&self, repo: &Path, source: &str, target: &str) -> Result<MergeResult> {
        // Checkout target branch
        run_git(repo, &["checkout", target])?;

        let output = Command::new("git")
            .args(["merge", "--no-ff", source])
            .current_dir(repo)
            .output()
            .context("git merge failed to execute")?;

        if output.status.success() {
            let sha = self.current_sha(repo)?;
            Ok(MergeResult::Clean { commit_sha: sha })
        } else {
            // Abort the failed merge
            let _ = Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(repo)
                .output();

            let stderr = String::from_utf8_lossy(&output.stdout);
            let files = parse_conflict_files(&stderr);
            Ok(MergeResult::Conflict { files })
        }
    }

    fn diff_stat(&self, repo: &Path, base: &str, head: &str) -> Result<DiffStat> {
        let output = Command::new("git")
            .args(["diff", "--stat", "--numstat", &format!("{}...{}", base, head)])
            .current_dir(repo)
            .output()
            .context("git diff failed")?;

        if !output.status.success() {
            return Ok(DiffStat::default());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut stat = DiffStat::default();
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                stat.insertions += parts[0].parse::<usize>().unwrap_or(0);
                stat.deletions += parts[1].parse::<usize>().unwrap_or(0);
                stat.files.push(parts[2].to_string());
                stat.files_changed += 1;
            }
        }
        Ok(stat)
    }

    fn current_sha(&self, repo: &Path) -> Result<String> {
        let output = run_git(repo, &["rev-parse", "HEAD"])?;
        Ok(output.trim().to_string())
    }
}

fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("git {} failed to execute", args.join(" ")))?;

    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn parse_conflict_files(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|l| l.starts_with("CONFLICT") || l.contains("Merge conflict in"))
        .filter_map(|l| l.rsplit("in ").next())
        .map(|s| s.trim().to_string())
        .collect()
}
```

**Step 4: Run tests**

Run: `cargo test -p openclaw-orchestrator -- git`
Expected: all pass

**Step 5: Commit**

```bash
git add crates/openclaw-orchestrator/
git commit -m "feat(orch): add git provider trait with CLI implementation"
```

---

## Phase 2: Core Engine

### Task 7: Instance management (register, heartbeat, shutdown)

**Files:**
- Create: `crates/openclaw-orchestrator/src/instance.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Implement instance lifecycle**

```rust
use crate::events::{EventContext, EventSink, OrchestratorEvent};
use crate::model::Instance;
use anyhow::Result;
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// Register this orchestrator instance in the database.
/// Returns the instance ID for all subsequent operations.
pub async fn register(
    pool: &PgPool,
    project_id: Uuid,
    name: &str,
    bind_addr: &str,
    data_dir: &str,
) -> Result<Uuid> {
    let pid = std::process::id() as i32;

    let instance: Instance = sqlx::query_as(
        r#"INSERT INTO orch_instances (project_id, name, pid, bind_addr, data_dir)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (project_id, name) DO UPDATE SET
               pid = EXCLUDED.pid,
               bind_addr = EXCLUDED.bind_addr,
               data_dir = EXCLUDED.data_dir,
               started_at = now(),
               last_heartbeat = now()
           RETURNING *"#,
    )
    .bind(project_id)
    .bind(name)
    .bind(pid)
    .bind(bind_addr)
    .bind(data_dir)
    .fetch_one(pool)
    .await?;

    Ok(instance.id)
}

/// Update the heartbeat timestamp. Call every 30s from a background task.
pub async fn heartbeat(pool: &PgPool, instance_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE orch_instances SET last_heartbeat = now() WHERE id = $1")
        .bind(instance_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Get or create a project. Returns project ID.
pub async fn ensure_project(pool: &PgPool, name: &str, repo_path: &str) -> Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        r#"INSERT INTO orch_projects (name, repo_path)
           VALUES ($1, $2)
           ON CONFLICT (name) DO UPDATE SET repo_path = EXCLUDED.repo_path
           RETURNING id"#,
    )
    .bind(name)
    .bind(repo_path)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}
```

**Step 2: Verify compiles**

Run: `cargo check -p openclaw-orchestrator`
Expected: no errors

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/src/instance.rs crates/openclaw-orchestrator/src/lib.rs
git commit -m "feat(orch): add instance registration and heartbeat"
```

---

### Task 8: Worker manager (spawn, monitor, log tailing, reattach)

**Files:**
- Create: `crates/openclaw-orchestrator/src/worker.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`
- Modify: `crates/openclaw-orchestrator/Cargo.toml` (add nix dep for kill check)

**Step 1: Implement worker spawn and log tailing**

```rust
use crate::events::{EventContext, EventSink, OrchestratorEvent};
use crate::model::TaskRun;
use anyhow::{Context, Result};
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

pub struct WorkerHandle {
    pub run_id: Uuid,
    pub pid: u32,
    pub child: tokio::process::Child,
}

/// Spawn a Claude Code worker for a task run.
/// Redirects stdout/stderr to log files. Starts a background log tailer
/// that emits WorkerOutput events into the EventSink.
pub async fn spawn(
    run: &TaskRun,
    prompt: &str,
    worktree: &Path,
    claude_binary: &str,
    max_turns: u32,
    sink: Arc<dyn EventSink>,
) -> Result<WorkerHandle> {
    // Ensure log directory exists
    let stdout_path = Path::new(&run.log_stdout);
    if let Some(parent) = stdout_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let stdout_file = std::fs::File::create(&run.log_stdout)
        .with_context(|| format!("create stdout log: {}", run.log_stdout))?;
    let stderr_file = std::fs::File::create(&run.log_stderr)
        .with_context(|| format!("create stderr log: {}", run.log_stderr))?;

    let child = tokio::process::Command::new(claude_binary)
        .args([
            "--print",
            "--output-format", "json",
            "--max-turns", &max_turns.to_string(),
        ])
        .arg(prompt)
        .current_dir(worktree)
        .stdout(stdout_file)
        .stderr(stderr_file)
        .kill_on_drop(false) // workers survive orchestrator restart
        .spawn()
        .context("failed to spawn claude CLI")?;

    let pid = child.id().context("no PID for child process")?;

    // Start server-side log tailer
    let log_path = run.log_stdout.clone();
    let instance_id = run.instance_id;
    let run_id = run.id;
    tokio::spawn(tail_and_emit(log_path, instance_id, run_id, sink));

    Ok(WorkerHandle {
        run_id: run.id,
        pid,
        child,
    })
}

/// Tail a log file and emit each new chunk as a WorkerOutput event.
/// Runs until the file is deleted or the process exits.
async fn tail_and_emit(
    path: String,
    instance_id: Uuid,
    run_id: Uuid,
    sink: Arc<dyn EventSink>,
) {
    let mut pos: u64 = 0;
    let mut consecutive_empty = 0u32;

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        match read_new_bytes(&path, &mut pos).await {
            Ok(Some(chunk)) => {
                consecutive_empty = 0;
                let ctx = EventContext {
                    run_id: Some(run_id),
                    actor: "log-tailer".to_string(),
                    ..Default::default()
                };
                let _ = sink.emit(
                    OrchestratorEvent::WorkerOutput { chunk },
                    ctx,
                ).await;
            }
            Ok(None) => {
                consecutive_empty += 1;
                // If no new data for 5 minutes and process is dead, stop tailing
                if consecutive_empty > 600 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// Read new bytes from a file starting at `pos`. Updates `pos` to new position.
async fn read_new_bytes(path: &str, pos: &mut u64) -> Result<Option<String>> {
    let metadata = tokio::fs::metadata(path).await?;
    let file_len = metadata.len();

    if file_len <= *pos {
        return Ok(None);
    }

    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(*pos)).await?;

    let to_read = (file_len - *pos).min(32_768) as usize;
    let mut buf = vec![0u8; to_read];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);

    *pos += n as u64;
    Ok(Some(String::from_utf8_lossy(&buf).to_string()))
}

/// Check if a process is alive by sending signal 0.
pub fn is_pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// On restart: find orphaned running workers, reattach or mark failed.
pub async fn reattach_all(
    pool: &PgPool,
    instance_id: Uuid,
    sink: Arc<dyn EventSink>,
) -> Result<Vec<Uuid>> {
    let orphans: Vec<TaskRun> = sqlx::query_as(
        "SELECT * FROM orch_task_runs WHERE instance_id = $1 AND state = 'running'"
    )
    .bind(instance_id)
    .fetch_all(pool)
    .await?;

    let mut reattached = Vec::new();

    for run in orphans {
        let pid = match run.pid {
            Some(p) => p as u32,
            None => {
                mark_run_failed(pool, &run, &sink, "no PID recorded").await?;
                continue;
            }
        };

        if is_pid_alive(pid) {
            // Emit reattach event
            let ctx = EventContext {
                run_id: Some(run.id),
                actor: "reattach".to_string(),
                ..Default::default()
            };
            sink.emit(OrchestratorEvent::WorkerReattached { pid: pid as i32 }, ctx).await?;

            // Resume log tailing
            let log_path = run.log_stdout.clone();
            let sink_clone = sink.clone();
            tokio::spawn(tail_and_emit(log_path, instance_id, run.id, sink_clone));

            reattached.push(run.id);
        } else {
            mark_run_failed(pool, &run, &sink, "process died during orchestrator restart").await?;
        }
    }

    Ok(reattached)
}

async fn mark_run_failed(
    pool: &PgPool,
    run: &TaskRun,
    sink: &Arc<dyn EventSink>,
    reason: &str,
) -> Result<()> {
    sqlx::query(
        "UPDATE orch_task_runs SET state = 'failed', finished_at = now() WHERE id = $1"
    )
    .bind(run.id)
    .execute(pool)
    .await?;

    let ctx = EventContext {
        run_id: Some(run.id),
        task_id: Some(run.task_id),
        actor: "reattach".to_string(),
        ..Default::default()
    };
    sink.emit(OrchestratorEvent::RunFailed {
        exit_code: None,
        stderr_tail: reason.to_string(),
    }, ctx).await?;

    Ok(())
}
```

Add `libc` and `tokio` with `io-util` + `fs` to dependencies:
```toml
libc = "0.2"
```

**Step 2: Verify compiles**

Run: `cargo check -p openclaw-orchestrator`
Expected: no errors

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/
git commit -m "feat(orch): add worker manager with spawn, log tailing, and reattach"
```

---

### Task 9: Scheduler (SKIP LOCKED task claiming + concurrency enforcement)

**Files:**
- Create: `crates/openclaw-orchestrator/src/scheduler.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Implement scheduler**

```rust
use crate::events::{EventContext, EventSink, OrchestratorEvent};
use crate::git::GitProvider;
use crate::model::{Budget, Task, TaskRun};
use crate::worker;
use anyhow::Result;
use sqlx::PgPool;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

pub struct SchedulerConfig {
    pub instance_id: Uuid,
    pub data_dir: String,
    pub claude_binary: String,
    pub claude_max_turns: u32,
    pub poll_interval: Duration,
}

/// Run the scheduler loop. Polls for pending tasks, respects concurrency limits,
/// spawns workers. Runs until the shutdown signal is received.
pub async fn run(
    pool: PgPool,
    config: SchedulerConfig,
    git: Arc<dyn GitProvider>,
    sink: Arc<dyn EventSink>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.poll_interval) => {},
            _ = shutdown.changed() => {
                tracing::info!("scheduler shutting down");
                break;
            }
        }

        // Check concurrency budget
        let active = count_active_runs(&pool, config.instance_id).await?;
        let budget = get_budget(&pool, config.instance_id).await?;

        if let Some(budget) = &budget {
            if active >= budget.max_concurrent as i64 {
                continue; // at capacity
            }

            // Check daily cost cap
            if let Some(cap) = budget.daily_cost_cap {
                if budget.today_cost >= cap {
                    tracing::warn!("daily cost cap reached: {:.2}/{:.2}", budget.today_cost, cap);
                    continue;
                }
            }
        }

        // Claim next pending task (SKIP LOCKED prevents double-claim)
        let task = match claim_next_task(&pool, config.instance_id).await? {
            Some(t) => t,
            None => continue,
        };

        tracing::info!(task_id = %task.id, title = %task.title, "claimed task");

        // Create worktree and branch
        let cycle = get_cycle(&pool, task.cycle_id).await?;
        let project = get_project_for_instance(&pool, config.instance_id).await?;
        let repo_path = Path::new(&project.repo_path);

        let branch_name = format!("orch/{}/{}", task.cycle_id, task.ordinal);
        let worktree_path = match git.create_worktree(repo_path, &branch_name) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(task_id = %task.id, "failed to create worktree: {}", e);
                revert_task_to_pending(&pool, task.id).await?;
                continue;
            }
        };

        // Create task run
        let run_number = next_run_number(&pool, task.id).await?;
        let log_dir = format!("{}/{}", config.data_dir, task.id);
        let run = create_run(
            &pool,
            task.id,
            config.instance_id,
            run_number,
            &branch_name,
            worktree_path.to_str().unwrap(),
            &format!("{}/run-{}-stdout.log", log_dir, run_number),
            &format!("{}/run-{}-stderr.log", log_dir, run_number),
        ).await?;

        // Build prompt for Claude
        let prompt = build_worker_prompt(&task, &cycle, &project);

        // Emit event
        let ctx = EventContext {
            cycle_id: Some(task.cycle_id),
            task_id: Some(task.id),
            run_id: Some(run.id),
            actor: "scheduler".to_string(),
        };
        sink.emit(OrchestratorEvent::RunSpawned {
            run_number,
            pid: 0, // updated after spawn
            prompt_sent: prompt.clone(),
        }, ctx).await?;

        // Update task state to running
        update_task_state(&pool, task.id, "running").await?;

        // Spawn worker
        match worker::spawn(&run, &prompt, &worktree_path, &config.claude_binary, config.claude_max_turns, sink.clone()).await {
            Ok(handle) => {
                // Update run with actual PID
                update_run_pid(&pool, run.id, handle.pid as i32).await?;
                update_run_state(&pool, run.id, "running").await?;

                // Spawn a task to wait for the worker to finish
                let pool_clone = pool.clone();
                let sink_clone = sink.clone();
                let task_id = task.id;
                let cycle_id = task.cycle_id;
                let instance_id = config.instance_id;
                tokio::spawn(async move {
                    wait_for_worker(pool_clone, handle, task_id, cycle_id, instance_id, sink_clone).await;
                });
            }
            Err(e) => {
                tracing::error!(run_id = %run.id, "failed to spawn worker: {}", e);
                update_run_state(&pool, run.id, "failed").await?;
                revert_task_to_pending(&pool, task.id).await?;
            }
        }
    }

    Ok(())
}

async fn wait_for_worker(
    pool: PgPool,
    mut handle: worker::WorkerHandle,
    task_id: Uuid,
    cycle_id: Uuid,
    instance_id: Uuid,
    sink: Arc<dyn EventSink>,
) {
    let status = handle.child.wait().await;
    let exit_code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

    let ctx = EventContext {
        cycle_id: Some(cycle_id),
        task_id: Some(task_id),
        run_id: Some(handle.run_id),
        actor: "worker-monitor".to_string(),
    };

    if exit_code == 0 {
        // Read output JSON from stdout log
        let output = tokio::fs::read_to_string(
            format!("{}", &pool) // TODO: read from run.log_stdout
        ).await.ok();

        let _ = sqlx::query(
            "UPDATE orch_task_runs SET state = 'completed', exit_code = $1, finished_at = now() WHERE id = $2"
        )
        .bind(exit_code)
        .bind(handle.run_id)
        .execute(&pool)
        .await;

        let _ = sink.emit(OrchestratorEvent::RunCompleted {
            exit_code,
            output_json: serde_json::Value::Null,
            cost_usd: 0.0,
        }, ctx).await;

        // Transition task to verifying
        let _ = sqlx::query("UPDATE orch_tasks SET state = 'verifying', updated_at = now() WHERE id = $1")
            .bind(task_id)
            .execute(&pool)
            .await;
    } else {
        let _ = sqlx::query(
            "UPDATE orch_task_runs SET state = 'failed', exit_code = $1, finished_at = now() WHERE id = $2"
        )
        .bind(exit_code)
        .bind(handle.run_id)
        .execute(&pool)
        .await;

        // Read last 500 chars of stderr for the event
        let stderr_tail = read_tail(&format!("")).await.unwrap_or_default();

        let _ = sink.emit(OrchestratorEvent::RunFailed {
            exit_code: Some(exit_code),
            stderr_tail,
        }, ctx).await;

        let _ = sqlx::query("UPDATE orch_tasks SET state = 'failed', updated_at = now() WHERE id = $1")
            .bind(task_id)
            .execute(&pool)
            .await;
    }
}

// === DB helper functions ===

async fn claim_next_task(pool: &PgPool, instance_id: Uuid) -> Result<Option<Task>> {
    let task = sqlx::query_as::<_, Task>(
        r#"UPDATE orch_tasks SET state = 'scheduled', updated_at = now()
           WHERE id = (
               SELECT id FROM orch_tasks
               WHERE instance_id = $1 AND state = 'pending'
               ORDER BY ordinal
               LIMIT 1
               FOR UPDATE SKIP LOCKED
           )
           RETURNING *"#,
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    Ok(task)
}

async fn count_active_runs(pool: &PgPool, instance_id: Uuid) -> Result<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM orch_task_runs WHERE instance_id = $1 AND state IN ('spawning', 'running')"
    )
    .bind(instance_id)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

async fn get_budget(pool: &PgPool, instance_id: Uuid) -> Result<Option<Budget>> {
    let budget = sqlx::query_as::<_, Budget>(
        r#"SELECT b.* FROM orch_budgets b
           JOIN orch_instances i ON i.project_id = b.project_id
           WHERE i.id = $1"#,
    )
    .bind(instance_id)
    .fetch_optional(pool)
    .await?;

    Ok(budget)
}

async fn get_cycle(pool: &PgPool, cycle_id: Uuid) -> Result<crate::model::Cycle> {
    Ok(sqlx::query_as("SELECT * FROM orch_cycles WHERE id = $1")
        .bind(cycle_id)
        .fetch_one(pool)
        .await?)
}

async fn get_project_for_instance(pool: &PgPool, instance_id: Uuid) -> Result<crate::model::Project> {
    Ok(sqlx::query_as(
        "SELECT p.* FROM orch_projects p JOIN orch_instances i ON i.project_id = p.id WHERE i.id = $1"
    )
    .bind(instance_id)
    .fetch_one(pool)
    .await?)
}

async fn next_run_number(pool: &PgPool, task_id: Uuid) -> Result<i32> {
    let (max,): (Option<i32>,) = sqlx::query_as(
        "SELECT MAX(run_number) FROM orch_task_runs WHERE task_id = $1"
    )
    .bind(task_id)
    .fetch_one(pool)
    .await?;

    Ok(max.unwrap_or(0) + 1)
}

async fn create_run(
    pool: &PgPool,
    task_id: Uuid,
    instance_id: Uuid,
    run_number: i32,
    branch: &str,
    worktree_path: &str,
    log_stdout: &str,
    log_stderr: &str,
) -> Result<TaskRun> {
    Ok(sqlx::query_as(
        r#"INSERT INTO orch_task_runs
           (task_id, instance_id, run_number, branch, worktree_path, log_stdout, log_stderr)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(task_id)
    .bind(instance_id)
    .bind(run_number)
    .bind(branch)
    .bind(worktree_path)
    .bind(log_stdout)
    .bind(log_stderr)
    .fetch_one(pool)
    .await?)
}

async fn update_task_state(pool: &PgPool, task_id: Uuid, state: &str) -> Result<()> {
    sqlx::query("UPDATE orch_tasks SET state = $1, updated_at = now() WHERE id = $2")
        .bind(state)
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn update_run_state(pool: &PgPool, run_id: Uuid, state: &str) -> Result<()> {
    sqlx::query("UPDATE orch_task_runs SET state = $1 WHERE id = $2")
        .bind(state)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn update_run_pid(pool: &PgPool, run_id: Uuid, pid: i32) -> Result<()> {
    sqlx::query("UPDATE orch_task_runs SET pid = $1 WHERE id = $2")
        .bind(pid)
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn revert_task_to_pending(pool: &PgPool, task_id: Uuid) -> Result<()> {
    sqlx::query("UPDATE orch_tasks SET state = 'pending', updated_at = now() WHERE id = $1")
        .bind(task_id)
        .execute(pool)
        .await?;
    Ok(())
}

fn build_worker_prompt(
    task: &Task,
    cycle: &crate::model::Cycle,
    project: &crate::model::Project,
) -> String {
    let guardrails = cycle.guardrails.as_ref()
        .map(|g| format!("\n\nGuardrails:\n{}", serde_json::to_string_pretty(g).unwrap_or_default()))
        .unwrap_or_default();

    let acceptance = serde_json::to_string_pretty(&task.acceptance).unwrap_or_default();

    format!(
        "You are working on project '{}' (repo: {}).\n\n\
         Task: {}\n\n\
         Description:\n{}\n\n\
         Acceptance Criteria:\n{}\n\
         {}\n\n\
         Instructions:\n\
         - Work only in the current worktree directory\n\
         - Commit your changes when done\n\
         - Run tests before committing\n\
         - Do not modify files outside the scope of this task",
        project.name, project.repo_path,
        task.title, task.description, acceptance, guardrails
    )
}

async fn read_tail(path: &str) -> Result<String> {
    let content = tokio::fs::read_to_string(path).await?;
    let tail: String = content.chars().rev().take(500).collect::<String>().chars().rev().collect();
    Ok(tail)
}
```

**Step 2: Verify compiles**

Run: `cargo check -p openclaw-orchestrator`
Expected: no errors

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/src/scheduler.rs crates/openclaw-orchestrator/src/lib.rs
git commit -m "feat(orch): add scheduler with SKIP LOCKED concurrency"
```

---

### Task 10: Verifier (pipeline runner)

**Files:**
- Create: `crates/openclaw-orchestrator/src/verifier.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parse_check_config_cargo() {
        let json = serde_json::json!({ "type": "cargo_check" });
        let check = parse_check(&json).unwrap();
        assert_eq!(check.name(), "cargo_check");
    }

    #[test]
    fn parse_check_config_custom() {
        let json = serde_json::json!({ "type": "custom", "cmd": "echo", "args": ["hello"] });
        let check = parse_check(&json).unwrap();
        assert_eq!(check.name(), "custom: echo");
    }

    #[test]
    fn custom_check_passes_on_success() {
        let dir = TempDir::new().unwrap();
        let check = CustomCheck { cmd: "true".to_string(), args: vec![] };
        let ctx = VerifyContext { worktree: dir.path().to_path_buf() };
        let result = check.run(&ctx).unwrap();
        assert!(result.passed);
    }

    #[test]
    fn custom_check_fails_on_error() {
        let dir = TempDir::new().unwrap();
        let check = CustomCheck { cmd: "false".to_string(), args: vec![] };
        let ctx = VerifyContext { worktree: dir.path().to_path_buf() };
        let result = check.run(&ctx).unwrap();
        assert!(!result.passed);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p openclaw-orchestrator -- verifier`
Expected: FAIL

**Step 3: Implement verifier**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

pub struct VerifyContext {
    pub worktree: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub passed: bool,
    pub output: String,
    pub duration_ms: u64,
}

pub trait VerifyCheck: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult>;
}

/// Run all checks in sequence. Returns results for each.
pub fn run_pipeline(checks: &[Box<dyn VerifyCheck>], ctx: &VerifyContext) -> Vec<CheckResult> {
    checks.iter().map(|check| {
        match check.run(ctx) {
            Ok(result) => result,
            Err(e) => CheckResult {
                name: check.name().to_string(),
                passed: false,
                output: format!("check error: {}", e),
                duration_ms: 0,
            },
        }
    }).collect()
}

/// Parse a check from project config JSON.
pub fn parse_check(json: &serde_json::Value) -> Result<Box<dyn VerifyCheck>> {
    let check_type = json.get("type").and_then(|v| v.as_str()).context("missing check type")?;
    match check_type {
        "cargo_check" => Ok(Box::new(CargoCheck)),
        "cargo_test" => Ok(Box::new(CargoTest)),
        "cargo_fmt" => Ok(Box::new(CargoFmt)),
        "cargo_clippy" => Ok(Box::new(CargoClippy)),
        "custom" => {
            let cmd = json.get("cmd").and_then(|v| v.as_str()).context("missing cmd")?.to_string();
            let args: Vec<String> = json.get("args")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            Ok(Box::new(CustomCheck { cmd, args }))
        }
        _ => anyhow::bail!("unknown check type: {}", check_type),
    }
}

/// Parse all checks from project config.
pub fn parse_checks(config: &serde_json::Value) -> Vec<Box<dyn VerifyCheck>> {
    config.get("verify")
        .and_then(|v| v.get("checks"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|j| parse_check(j).ok()).collect())
        .unwrap_or_default()
}

// === Built-in checks ===

fn run_command(ctx: &VerifyContext, name: &str, cmd: &str, args: &[&str]) -> CheckResult {
    let start = Instant::now();
    let output = Command::new(cmd)
        .args(args)
        .current_dir(&ctx.worktree)
        .output();

    let duration_ms = start.elapsed().as_millis() as u64;

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            CheckResult {
                name: name.to_string(),
                passed: out.status.success(),
                output: format!("{}{}", stdout, stderr),
                duration_ms,
            }
        }
        Err(e) => CheckResult {
            name: name.to_string(),
            passed: false,
            output: format!("failed to execute: {}", e),
            duration_ms,
        },
    }
}

pub struct CargoCheck;
impl VerifyCheck for CargoCheck {
    fn name(&self) -> &str { "cargo_check" }
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult> {
        Ok(run_command(ctx, self.name(), "cargo", &["check"]))
    }
}

pub struct CargoTest;
impl VerifyCheck for CargoTest {
    fn name(&self) -> &str { "cargo_test" }
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult> {
        Ok(run_command(ctx, self.name(), "cargo", &["test"]))
    }
}

pub struct CargoFmt;
impl VerifyCheck for CargoFmt {
    fn name(&self) -> &str { "cargo_fmt" }
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult> {
        Ok(run_command(ctx, self.name(), "cargo", &["fmt", "--check"]))
    }
}

pub struct CargoClippy;
impl VerifyCheck for CargoClippy {
    fn name(&self) -> &str { "cargo_clippy" }
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult> {
        Ok(run_command(ctx, self.name(), "cargo", &["clippy", "--", "-D", "warnings"]))
    }
}

pub struct CustomCheck {
    pub cmd: String,
    pub args: Vec<String>,
}

impl VerifyCheck for CustomCheck {
    fn name(&self) -> &str { Box::leak(format!("custom: {}", self.cmd).into_boxed_str()) }
    fn run(&self, ctx: &VerifyContext) -> Result<CheckResult> {
        let args_ref: Vec<&str> = self.args.iter().map(|s| s.as_str()).collect();
        Ok(run_command(ctx, self.name(), &self.cmd, &args_ref))
    }
}
```

**Step 4: Run tests**

Run: `cargo test -p openclaw-orchestrator -- verifier`
Expected: all pass

**Step 5: Commit**

```bash
git add crates/openclaw-orchestrator/src/verifier.rs crates/openclaw-orchestrator/src/lib.rs
git commit -m "feat(orch): add verification pipeline runner"
```

---

### Task 11: Planner (invoke Claude to generate structured plan)

**Files:**
- Create: `crates/openclaw-orchestrator/src/planner.rs`
- Modify: `crates/openclaw-orchestrator/src/lib.rs`

**Step 1: Implement planner**

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

/// The structured plan that Claude generates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradePlan {
    pub phases: Vec<Phase>,
    pub guardrails: Guardrails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    pub tasks: Vec<PlannedTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTask {
    pub title: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub risk_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guardrails {
    pub do_not_touch: Vec<String>,
    pub coding_standards: Vec<String>,
    pub safety_constraints: Vec<String>,
}

const PLANNER_SYSTEM_PROMPT: &str = r#"You are a software planning assistant. Given a raw upgrade prompt, produce a structured JSON plan.

Output ONLY valid JSON matching this schema:
{
  "phases": [
    {
      "name": "phase name",
      "tasks": [
        {
          "title": "short task title",
          "description": "detailed description of what to do",
          "acceptance_criteria": ["criterion 1", "criterion 2"],
          "risk_notes": "optional risk notes or null"
        }
      ]
    }
  ],
  "guardrails": {
    "do_not_touch": ["file patterns to avoid"],
    "coding_standards": ["standard 1"],
    "safety_constraints": ["constraint 1"]
  }
}

Rules:
- Each task should be independently implementable in a separate git branch
- Tasks within a phase can run in parallel
- Phases run sequentially
- Keep tasks small: each should take one Claude Code session
- Be specific about file paths and what to change
- Include test requirements in acceptance criteria"#;

/// Generate an upgrade plan by invoking Claude Code.
pub fn generate_plan(
    prompt: &str,
    repo_path: &str,
    claude_binary: &str,
) -> Result<UpgradePlan> {
    let full_prompt = format!(
        "{}\n\nProject repository: {}\n\nUpgrade request:\n{}",
        PLANNER_SYSTEM_PROMPT, repo_path, prompt
    );

    let output = Command::new(claude_binary)
        .args(["--print", "--output-format", "json"])
        .arg(&full_prompt)
        .current_dir(repo_path)
        .output()
        .context("failed to run claude for planning")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("planner claude exited with error: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The claude --print --output-format json wraps output in a JSON envelope.
    // We need to extract the result text and parse it as our plan.
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .context("failed to parse claude JSON output")?;

    let result_text = envelope.get("result")
        .and_then(|v| v.as_str())
        .context("no 'result' field in claude output")?;

    // Try to find JSON in the result text (Claude may wrap it in markdown)
    let json_str = extract_json(result_text)?;
    let plan: UpgradePlan = serde_json::from_str(&json_str)
        .context("failed to parse plan JSON")?;

    Ok(plan)
}

/// Extract JSON from text that may contain markdown code fences.
fn extract_json(text: &str) -> Result<String> {
    // Try direct parse first
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        return Ok(text.to_string());
    }

    // Try extracting from ```json ... ``` blocks
    if let Some(start) = text.find("```json") {
        let after_fence = &text[start + 7..];
        if let Some(end) = after_fence.find("```") {
            let json_str = after_fence[..end].trim();
            if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
                return Ok(json_str.to_string());
            }
        }
    }

    // Try extracting from ``` ... ``` blocks
    if let Some(start) = text.find("```") {
        let after_fence = &text[start + 3..];
        if let Some(end) = after_fence.find("```") {
            let json_str = after_fence[..end].trim();
            if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
                return Ok(json_str.to_string());
            }
        }
    }

    // Try finding first { to last }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        let json_str = &text[start..=end];
        if serde_json::from_str::<serde_json::Value>(json_str).is_ok() {
            return Ok(json_str.to_string());
        }
    }

    anyhow::bail!("could not extract JSON from planner output")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_direct() {
        let json = r#"{"phases": [], "guardrails": {"do_not_touch": [], "coding_standards": [], "safety_constraints": []}}"#;
        assert!(extract_json(json).is_ok());
    }

    #[test]
    fn extract_json_from_code_fence() {
        let text = "Here's the plan:\n```json\n{\"phases\": []}\n```\nDone.";
        let result = extract_json(text).unwrap();
        assert_eq!(result, "{\"phases\": []}");
    }

    #[test]
    fn extract_json_from_braces() {
        let text = "The plan is: {\"phases\": []} that's it.";
        let result = extract_json(text).unwrap();
        assert_eq!(result, "{\"phases\": []}");
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p openclaw-orchestrator -- planner`
Expected: all pass

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/src/planner.rs crates/openclaw-orchestrator/src/lib.rs
git commit -m "feat(orch): add planner module for Claude-generated upgrade plans"
```

---

### Task 12: Config module (TOML parsing)

**Files:**
- Create: `crates/openclaw-orchestrator/src/config.rs`
- Modify: `crates/openclaw-orchestrator/Cargo.toml` (add toml dep)

**Step 1: Implement config**

Add `toml = "0.8"` to orchestrator dependencies.

```rust
use serde::{Deserialize, Serialize};
use std::path::Path;
use anyhow::{Context, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    pub orchestrator: OrchSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchSection {
    pub instance_name: String,
    pub bind: String,
    pub data_dir: String,
    pub auth_token: String,
    pub project_name: String,
    pub repo_path: String,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub security: Security,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_workers: u32,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    pub daily_cost_cap_usd: Option<f32>,
    #[serde(default = "default_claude_binary")]
    pub claude_binary: String,
    #[serde(default = "default_claude_max_turns")]
    pub claude_max_turns: u32,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            max_concurrent_workers: default_max_concurrent(),
            max_retries: default_max_retries(),
            daily_cost_cap_usd: None,
            claude_binary: default_claude_binary(),
            claude_max_turns: default_claude_max_turns(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Security {
    #[serde(default)]
    pub allow_lan_bind: bool,
    #[serde(default = "default_redact_patterns")]
    pub redact_env_patterns: Vec<String>,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            allow_lan_bind: false,
            redact_env_patterns: default_redact_patterns(),
        }
    }
}

fn default_max_concurrent() -> u32 { 3 }
fn default_max_retries() -> u32 { 3 }
fn default_claude_binary() -> String { "claude".to_string() }
fn default_claude_max_turns() -> u32 { 50 }
fn default_redact_patterns() -> Vec<String> {
    vec!["KEY".into(), "SECRET".into(), "TOKEN".into(), "PASSWORD".into()]
}

pub fn load(path: &Path) -> Result<OrchestratorConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read config: {}", path.display()))?;
    let config: OrchestratorConfig = toml::from_str(&content)
        .with_context(|| format!("parse config: {}", path.display()))?;

    // Validate bind address security
    if !config.orchestrator.security.allow_lan_bind
        && !config.orchestrator.bind.starts_with("127.0.0.1")
        && !config.orchestrator.bind.starts_with("localhost")
    {
        anyhow::bail!(
            "bind address '{}' is not localhost but allow_lan_bind is false",
            config.orchestrator.bind
        );
    }

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[orchestrator]
instance_name = "test"
bind = "127.0.0.1:3130"
data_dir = "/tmp/orch"
auth_token = "test-token"
project_name = "test-project"
repo_path = "/tmp/repo"
"#;
        let config: OrchestratorConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.orchestrator.instance_name, "test");
        assert_eq!(config.orchestrator.defaults.max_concurrent_workers, 3);
        assert!(!config.orchestrator.security.allow_lan_bind);
    }

    #[test]
    fn reject_lan_bind_without_flag() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, r#"
[orchestrator]
instance_name = "test"
bind = "0.0.0.0:3130"
data_dir = "/tmp/orch"
auth_token = "test-token"
project_name = "test-project"
repo_path = "/tmp/repo"
"#).unwrap();
        assert!(load(&path).is_err());
    }
}
```

**Step 2: Run tests**

Run: `cargo test -p openclaw-orchestrator -- config`
Expected: all pass

**Step 3: Commit**

```bash
git add crates/openclaw-orchestrator/
git commit -m "feat(orch): add TOML config with security validation"
```

---

## Phase 3: UI Binary

### Task 13: Axum server skeleton with health endpoint

**Files:**
- Modify: `crates/openclaw-ui/src/main.rs`
- Modify: `crates/openclaw-ui/Cargo.toml`

**Step 1: Update dependencies**

Add to `openclaw-ui/Cargo.toml`:
```toml
[dependencies]
openclaw-orchestrator = { path = "../openclaw-orchestrator" }
openclaw-db = { path = "../openclaw-db" }
axum = "0.8"
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
uuid = { workspace = true }
tower-http = { version = "0.6", features = ["cors"] }
clap = { workspace = true }
```

**Step 2: Implement main.rs with config loading and health endpoint**

```rust
use axum::{routing::get, Json, Router};
use clap::Parser;
use openclaw_orchestrator::config;
use std::net::SocketAddr;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "openclaw-orch")]
struct Cli {
    /// Path to config TOML file
    #[arg(short, long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    let config = config::load(&cli.config)?;

    info!(
        instance = %config.orchestrator.instance_name,
        bind = %config.orchestrator.bind,
        "starting openclaw-orch"
    );

    // Initialize DB
    openclaw_db::init().await?;

    let app = Router::new()
        .route("/health", get(health));

    let addr: SocketAddr = config.orchestrator.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("listening on {}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "openclaw-orch",
    }))
}
```

**Step 3: Verify compiles**

Run: `cargo check -p openclaw-ui`
Expected: no errors

**Step 4: Commit**

```bash
git add crates/openclaw-ui/
git commit -m "feat(ui): Axum server skeleton with health endpoint"
```

---

### Task 14: REST API routes (projects, cycles, tasks, runs, merge)

**Files:**
- Create: `crates/openclaw-ui/src/api/mod.rs`
- Create: `crates/openclaw-ui/src/api/projects.rs`
- Create: `crates/openclaw-ui/src/api/cycles.rs`
- Create: `crates/openclaw-ui/src/api/tasks.rs`
- Create: `crates/openclaw-ui/src/api/events.rs`
- Modify: `crates/openclaw-ui/src/main.rs` (mount routes)

**Step 1: Implement API routes**

Each file follows the same pattern: Axum handler functions that call into `openclaw-orchestrator` DB functions. The routes are:

```
GET    /api/projects           — list projects
POST   /api/projects           — create project
GET    /api/cycles             — list cycles (query: ?instance_id=)
POST   /api/cycles             — create cycle (submit prompt)
GET    /api/cycles/:id         — get cycle detail
POST   /api/cycles/:id/approve — approve a plan
GET    /api/tasks              — list tasks (query: ?cycle_id=)
GET    /api/tasks/:id          — task detail with runs
GET    /api/runs/:id/logs      — stream run logs (SSE)
POST   /api/merge/:cycle_id    — initiate merge
GET    /api/events             — events (query: ?instance_id=&since_seq=)
GET    /api/budgets/:project_id — get budget
```

(Implementation follows standard Axum patterns: `State<AppState>` with PgPool + EventSink, `Json<T>` for request/response. Each handler is 10-30 lines.)

**Step 2: Mount routes in main.rs**

```rust
let app = Router::new()
    .route("/health", get(health))
    .nest("/api", api::router(app_state));
```

**Step 3: Verify compiles**

Run: `cargo check -p openclaw-ui`
Expected: no errors

**Step 4: Commit**

```bash
git add crates/openclaw-ui/src/
git commit -m "feat(ui): add REST API routes for projects, cycles, tasks, events"
```

---

### Task 15: WebSocket handler (event streaming)

**Files:**
- Create: `crates/openclaw-ui/src/ws.rs`
- Modify: `crates/openclaw-ui/Cargo.toml` (add axum ws feature)
- Modify: `crates/openclaw-ui/src/main.rs` (mount ws route)

**Step 1: Implement WebSocket handler**

Add to Cargo.toml: `axum = { version = "0.8", features = ["ws"] }`

```rust
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use openclaw_orchestrator::events::{EventSink, OrchestratorEvent};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Deserialize)]
struct Subscribe {
    instance_id: Uuid,
    since_seq: Option<i64>,
}

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    // Wait for subscribe message
    let sub: Subscribe = match socket.recv().await {
        Some(Ok(Message::Text(text))) => {
            match serde_json::from_str(&text) {
                Ok(s) => s,
                Err(_) => return,
            }
        }
        _ => return,
    };

    // Replay missed events
    let since = sub.since_seq.unwrap_or(0);
    if let Ok(events) = state.event_sink.replay(since).await {
        for event in events {
            let json = serde_json::to_string(&event).unwrap_or_default();
            if socket.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
    }

    // Subscribe to live events
    let mut rx = state.event_sink.subscribe();
    loop {
        match rx.recv().await {
            Ok((seq, event)) => {
                let msg = serde_json::json!({
                    "seq": seq,
                    "event": event,
                });
                let json = serde_json::to_string(&msg).unwrap_or_default();
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("ws client lagged by {} events", n);
            }
            Err(_) => break,
        }
    }
}
```

**Step 2: Mount in main.rs**

Add: `.route("/ws", get(ws::handler))`

**Step 3: Verify compiles**

Run: `cargo check -p openclaw-ui`
Expected: no errors

**Step 4: Commit**

```bash
git add crates/openclaw-ui/src/ws.rs crates/openclaw-ui/src/main.rs crates/openclaw-ui/Cargo.toml
git commit -m "feat(ui): add WebSocket handler with event replay"
```

---

### Task 16: Embedded TypeScript frontend (vanilla TS + esbuild)

**Files:**
- Create: `crates/openclaw-ui/ui-src/index.html`
- Create: `crates/openclaw-ui/ui-src/app.ts`
- Create: `crates/openclaw-ui/ui-src/store.ts`
- Create: `crates/openclaw-ui/ui-src/views/projects.ts`
- Create: `crates/openclaw-ui/ui-src/views/cycle.ts`
- Create: `crates/openclaw-ui/ui-src/views/worker.ts`
- Create: `crates/openclaw-ui/ui-src/types.ts`
- Create: `crates/openclaw-ui/ui-src/style.css`
- Create: `crates/openclaw-ui/src/frontend.rs`
- Create: `crates/openclaw-ui/build.sh`

**Step 1: Create build script**

`build.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/ui-src"
npx esbuild app.ts --bundle --outfile=../src/static/app.js --minify --sourcemap
cp index.html ../src/static/index.html
cp style.css ../src/static/style.css
```

**Step 2: Create minimal frontend files**

- `index.html`: Shell with `<div id="app">`, links to `style.css` and `app.js`
- `app.ts`: Router (hash-based), WS connection manager, event dispatcher to store
- `store.ts`: Event replay → derived state. Holds events array, computes project/cycle/task/worker state
- `types.ts`: TypeScript interfaces matching Rust models (Project, Cycle, Task, TaskRun, Event)
- `views/projects.ts`: Render project list, create project form
- `views/cycle.ts`: Render cycle detail with plan JSON, phase/task tree, approve button
- `views/worker.ts`: Live log display (appends WorkerOutput chunks to a `<pre>` element)
- `style.css`: Minimal functional styling (grid layout, status badges, monospace for logs)

**Step 3: Create frontend.rs (serve embedded files)**

```rust
pub fn index_html() -> &'static str { include_str!("static/index.html") }
pub fn app_js() -> &'static str { include_str!("static/app.js") }
pub fn style_css() -> &'static str { include_str!("static/style.css") }
```

**Step 4: Add static routes to main.rs**

```rust
.route("/", get(|| async { Html(frontend::index_html()) }))
.route("/app.js", get(|| async { ([("content-type", "application/javascript")], frontend::app_js()) }))
.route("/style.css", get(|| async { ([("content-type", "text/css")], frontend::style_css()) }))
```

**Step 5: Build frontend and verify**

Run: `cd crates/openclaw-ui && bash build.sh && cargo check -p openclaw-ui`
Expected: no errors

**Step 6: Commit**

```bash
git add crates/openclaw-ui/ui-src/ crates/openclaw-ui/src/frontend.rs crates/openclaw-ui/build.sh
git commit -m "feat(ui): add embedded TypeScript frontend with event-driven store"
```

---

## Phase 4: Integration & Deployment

### Task 17: Wire up main.rs as full orchestrator binary

**Files:**
- Modify: `crates/openclaw-ui/src/main.rs`

**Step 1: Complete main.rs with full startup sequence**

The main binary must:
1. Parse CLI args (config path)
2. Load TOML config
3. Init Postgres (openclaw-db)
4. Run migration (002_orchestrator.sql)
5. Register instance (ensure_project + register)
6. Create PgEventStore
7. Reattach orphaned workers
8. Start heartbeat background task
9. Start scheduler background task
10. Start Axum HTTP server
11. Handle graceful shutdown (SIGINT/SIGTERM)

This wires together all the components from Phase 1-3.

**Step 2: Verify it starts**

Create a test config file, start the binary, hit `/health`, then Ctrl+C:

```bash
cargo build -p openclaw-ui
./target/debug/openclaw-orch --config test-config.toml
# In another terminal:
curl http://localhost:3130/health
```

Expected: `{"status":"ok","service":"openclaw-orch"}`

**Step 3: Commit**

```bash
git add crates/openclaw-ui/src/main.rs
git commit -m "feat(ui): wire up full orchestrator startup sequence"
```

---

### Task 18: Auth middleware

**Files:**
- Create: `crates/openclaw-ui/src/auth.rs`
- Modify: `crates/openclaw-ui/src/main.rs`

**Step 1: Implement constant-time token auth**

```rust
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::http::StatusCode;

pub async fn require_auth(
    req: Request,
    next: Next,
) -> Response {
    // Extract token from state and compare
    let expected = req.extensions().get::<AuthToken>();
    let provided = req.headers().get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match (expected, provided) {
        (Some(expected), Some(provided)) => {
            if constant_time_eq(expected.0.as_bytes(), provided.as_bytes()) {
                next.run(req).await
            } else {
                StatusCode::UNAUTHORIZED.into_response()
            }
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone)]
pub struct AuthToken(pub String);
```

**Step 2: Apply to API routes in main.rs**

Health endpoint is public; API/WS routes require auth.

**Step 3: Commit**

```bash
git add crates/openclaw-ui/src/auth.rs crates/openclaw-ui/src/main.rs
git commit -m "feat(ui): add constant-time token auth middleware"
```

---

### Task 19: Prometheus metrics endpoint

**Files:**
- Create: `crates/openclaw-ui/src/metrics.rs`
- Modify: `crates/openclaw-ui/src/main.rs`

**Step 1: Implement /metrics**

Expose counters for: active workers, total events, cycles by state, tasks by state, budget usage.
Query from Postgres on each scrape (simple, no in-memory accumulation needed for MVP).

**Step 2: Commit**

```bash
git add crates/openclaw-ui/src/metrics.rs
git commit -m "feat(ui): add Prometheus metrics endpoint"
```

---

### Task 20: Nix flake + systemd service

**Files:**
- Create: `flake.nix` (in openclaw-rs root)
- Create: `nix/orchestrator-service.nix`

**Step 1: Create Nix flake**

Minimal flake with:
- `packages.openclaw-orch` — build the binary
- `devShells.default` — dev environment with Rust, sqlx-cli, esbuild, nodejs
- NixOS module for systemd service

**Step 2: Create systemd service template**

```nix
# nix/orchestrator-service.nix
{ config, lib, pkgs, ... }:
{
  systemd.user.services.openclaw-orch = {
    description = "OpenClaw SDLC Orchestrator";
    after = [ "network.target" "postgresql.service" ];
    wantedBy = [ "default.target" ];
    serviceConfig = {
      ExecStart = "${pkgs.openclaw-orch}/bin/openclaw-orch --config %h/.config/openclaw-orch/config.toml";
      Restart = "on-failure";
      RestartSec = 5;
      Environment = [
        "DATABASE_URL=postgresql://openclaw:openclaw@127.0.0.1/openclaw"
        "RUST_LOG=info"
      ];
    };
  };
}
```

**Step 3: Commit**

```bash
git add flake.nix nix/
git commit -m "feat: add Nix flake and systemd service for orchestrator"
```

---

### Task 21: End-to-end smoke test

**Files:**
- Create: `tests/e2e_smoke.sh`

**Step 1: Write smoke test script**

```bash
#!/usr/bin/env bash
set -euo pipefail

BASE="http://localhost:3130"
TOKEN="test-token"
AUTH="Authorization: Bearer $TOKEN"

echo "=== E2E Smoke Test ==="

# 1. Health check
echo -n "Health... "
curl -sf "$BASE/health" | jq -e '.status == "ok"'
echo "OK"

# 2. Create project
echo -n "Create project... "
PROJECT_ID=$(curl -sf -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"name":"test-project","repo_path":"/tmp/test-repo"}' \
  "$BASE/api/projects" | jq -r '.id')
echo "OK ($PROJECT_ID)"

# 3. Create cycle
echo -n "Create cycle... "
CYCLE_ID=$(curl -sf -H "$AUTH" -H "Content-Type: application/json" \
  -d '{"prompt":"Add a README.md file"}' \
  "$BASE/api/cycles?project_id=$PROJECT_ID" | jq -r '.id')
echo "OK ($CYCLE_ID)"

# 4. Check events
echo -n "Events... "
curl -sf -H "$AUTH" "$BASE/api/events?instance_id=$INSTANCE_ID&since_seq=0" | jq -e 'length > 0'
echo "OK"

echo "=== All checks passed ==="
```

**Step 2: Run smoke test**

Run: `bash tests/e2e_smoke.sh`
Expected: All checks passed

**Step 3: Commit**

```bash
git add tests/e2e_smoke.sh
git commit -m "test: add end-to-end smoke test script"
```

---

## Summary of all commits

| # | Commit | What |
|---|--------|------|
| 1 | `feat(orch): scaffold orchestrator and UI crates` | Crate skeletons, workspace wiring |
| 2 | `feat(orch): add orchestrator database migration` | orch_* tables, triggers, indexes |
| 3 | `feat(orch): add domain model types` | Rust structs matching DB schema |
| 4 | `feat(orch): add state machines with validated transitions` | CycleState, TaskState, RunState + tests |
| 5 | `feat(orch): add event system with PgEventStore` | EventSink trait, monotonic seq, broadcast |
| 6 | `feat(orch): add git provider trait with CLI implementation` | GitProvider, worktree, merge + tests |
| 7 | `feat(orch): add instance registration and heartbeat` | Instance lifecycle |
| 8 | `feat(orch): add worker manager with spawn, log tailing, and reattach` | Worker lifecycle |
| 9 | `feat(orch): add scheduler with SKIP LOCKED concurrency` | Task claiming, budget enforcement |
| 10 | `feat(orch): add verification pipeline runner` | VerifyCheck trait, built-in checks + tests |
| 11 | `feat(orch): add planner module` | Claude-generated plan, JSON extraction + tests |
| 12 | `feat(orch): add TOML config with security validation` | Config loading, LAN bind safety + tests |
| 13 | `feat(ui): Axum server skeleton with health endpoint` | Binary entrypoint |
| 14 | `feat(ui): add REST API routes` | Projects, cycles, tasks, events, merge |
| 15 | `feat(ui): add WebSocket handler with event replay` | WS subscription, seq-based replay |
| 16 | `feat(ui): add embedded TypeScript frontend` | Vanilla TS, esbuild, event-driven store |
| 17 | `feat(ui): wire up full orchestrator startup sequence` | Complete main.rs |
| 18 | `feat(ui): add constant-time token auth middleware` | Auth for API/WS |
| 19 | `feat(ui): add Prometheus metrics endpoint` | /metrics |
| 20 | `feat: add Nix flake and systemd service` | NixOS deployment |
| 21 | `test: add end-to-end smoke test script` | Smoke test |

---

## How to verify locally (after all tasks)

```bash
# 1. Build
cargo build -p openclaw-ui

# 2. Apply migration
psql -U openclaw -d openclaw -f migrations/002_orchestrator.sql

# 3. Create config
cat > /tmp/orch-test.toml <<'EOF'
[orchestrator]
instance_name = "dev"
bind = "127.0.0.1:3130"
data_dir = "/tmp/openclaw-orch"
auth_token = "test-token"
project_name = "test"
repo_path = "/tmp/test-repo"
EOF

# 4. Create test repo
mkdir -p /tmp/test-repo && cd /tmp/test-repo && git init && echo "# test" > README.md && git add . && git commit -m "init"

# 5. Run
DATABASE_URL="postgresql://openclaw:openclaw@127.0.0.1/openclaw" \
  ./target/debug/openclaw-orch --config /tmp/orch-test.toml

# 6. Open UI
# Visit http://localhost:3130 in browser

# 7. Test API
curl -H "Authorization: Bearer test-token" http://localhost:3130/api/projects

# 8. Run smoke test
bash tests/e2e_smoke.sh
```
