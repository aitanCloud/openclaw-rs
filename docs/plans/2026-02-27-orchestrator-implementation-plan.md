# OpenClaw SDLC Orchestrator — Comprehensive Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

> **Supersedes:** `2026-02-27-sdlc-orchestrator-impl.md` (pre-review version)
> **Architecture source:** `docs/ARCHITECTURE.md` (37 GPT review cycles, 9558 lines, 31 sections)
> **Date:** 2026-02-27
> **Status:** Ready for implementation

**Goal:** Build a local-first AI SDLC Orchestrator that manages parallel Claude Code workers across git worktrees, with verification gates, event-sourced state, and a web UI control plane.

**Architecture:** Two new crates in the `openclaw-rs` workspace. `openclaw-orchestrator` is a pure domain+app library (no HTTP) with event-sourced state, SKIP LOCKED scheduling, worker management, and git CLI wrappers. `openclaw-ui` is a thin Axum binary serving REST API, WebSocket events, and an embedded vanilla TypeScript frontend. Workers are `claude --print --output-format json` subprocesses with log files tailed server-side into the event bus.

**Tech Stack:** Rust 2021, sqlx 0.8 (Postgres), Axum 0.8, tokio 1, tokio-tungstenite, esbuild, vanilla TypeScript. Existing workspace deps reused.

## What We're Building

A **control plane for AI-driven software development**. The orchestrator manages multiple projects, each running upgrade cycles where Claude Code workers execute tasks in parallel — implementing features, fixing bugs, writing tests — in isolated git worktrees. The orchestrator plans work, schedules it, spawns workers, verifies their output, and merges clean code back.

**North-star**: "The orchestrator is a deterministic state machine whose only source of truth is an append-only event log, and every side effect is idempotent and recoverable."

### Workers Are Claude Code Instances

Each worker is a `claude --print --output-format json` subprocess:
- Spawned in its own **git worktree** (filesystem isolation)
- stdout/stderr redirected to **log files** (survives restarts)
- Reports **proof-of-life** → heartbeats extend lease
- Output is **verified** (cargo check, cargo test, cargo clippy, custom checks)
- Verified work gets **merged** back to the target branch
- Budget tracked per run (token cost in integer cents)

**Target scale**: 5 projects × 6 workers each, no human handholding.

## Architecture Summary

### Crate Structure

Two new crates added to the existing `openclaw-rs` workspace:

```
openclaw-rs workspace
├── crates/
│   ├── openclaw-core/           (existing — config, paths)
│   ├── openclaw-agent/          (existing — LLM runtime, tools)
│   ├── openclaw-gateway/        (existing — Telegram/Discord, add orch commands)
│   ├── openclaw-db/             (existing — Postgres pool, add orch_* migrations)
│   ├── openclaw-cli/            (existing — add orch subcommands)
│   ├── openclaw-mcp/            (existing)
│   ├── openclaw-orchestrator/   (NEW — pure domain + app library, no HTTP)
│   └── openclaw-ui/             (NEW — Axum binary: REST + WS + embedded TS frontend)
```

**Key constraint**: `openclaw-orchestrator` is a pure library. No HTTP types, no Axum. It exports domain types, state machines, event store, scheduler, worker manager, verifier, git provider, and budget engine. `openclaw-ui` is a thin adapter that wires HTTP/WS routes to orchestrator functions.

### Event-Sourced Spine

Every state change emits an event to `orch_events` (append-only, trigger-protected). Projection tables (`orch_cycles`, `orch_tasks`, `orch_runs`, etc.) are derived from events and can be rebuilt. 38 state-changing + 9 informational event types. See Architecture Doc Section 31 for the full catalog.

### Five State Machines

| State Machine | States | Primary Events |
|---------------|--------|----------------|
| InstanceState | Provisioning, Active, Blocked, Suspended, ProvisioningFailed | InstanceCreated → InstanceProvisioned |
| CycleState | Created, Planning, PlanReady, Approved, Running, Blocked, Completing, Completed, Failed, Cancelled | CycleCreated → PlanApproved → CycleRunningStarted → CycleCompleted |
| TaskState | Scheduled, Active, Verifying, Passed, Failed, Cancelled, Skipped | TaskScheduled → RunClaimed → TaskPassed |
| RunState | Claimed, Running, Completed, Failed, TimedOut, Cancelled, Abandoned | RunClaimed → RunStarted → RunCompleted |
| MergeState | Pending, Attempted, Succeeded, Conflicted, FailedTransient, FailedPermanent, Skipped | MergeAttempted → MergeSucceeded |

### Key Invariants (from 37-cycle review)

1. **Every state change emits an event** — no event = no change (Section 1)
2. **Idempotency everywhere** — stable IDs, INSERT IF NOT EXISTS (Section 5)
3. **Advisory locks** — bigint/bigint from UUID (big-endian), xact variant only (E1)
4. **Worker identity** — `worker_session_id` is domain, PID is adapter-only (E3)
5. **Budget in integer cents** — never floats, never f32/f64 (E4)
6. **Co-emission rules** — PlanApproved + BudgetReserved in same tx, etc. (Section 31.3)
7. **Anti-pairs** — RunClaimed + RunStarted must NOT be co-emitted (Section 31.4)
8. **Projector halts instance, not server** — Blocked(UnknownEvent) (E8)
9. **Reconciler is read-only during maintenance** (E7)
10. **Three error types** — DomainError, InfraError, AppError — never leak across boundaries (E2)
11. **Actor in every state-changing event** — Human | Planner | Worker | System (E12)
12. **occurred_at = server clock** — never worker-supplied (E14)
13. **Token fingerprinting** — sha256(token)[0..24] hex (96 bits) for caching/logging (E10)
14. **Criticality from envelope only** — never inspect payload to classify events (E8)

---

## Phase 0: Foundation (estimated: 1 week)

**Goal**: Compilable workspace with domain types, event store, projector trait, and one golden test passing.

### Task 0.1: Create crate skeletons and wire workspace

**Files to create:**
- `crates/openclaw-orchestrator/Cargo.toml`
- `crates/openclaw-orchestrator/src/lib.rs`
- `crates/openclaw-ui/Cargo.toml`
- `crates/openclaw-ui/src/main.rs`

**Files to modify:**
- `Cargo.toml` (root — add workspace members)

**What to do:**
1. Create `openclaw-orchestrator` crate with dependencies: sqlx 0.8 (postgres, chrono, uuid, json), serde, tokio, anyhow, chrono, uuid, tracing
2. Create `openclaw-ui` crate with dependencies: openclaw-orchestrator, openclaw-db, axum 0.8, tokio, serde, tracing
3. Add both to workspace members in root Cargo.toml
4. `cargo check -p openclaw-orchestrator -p openclaw-ui` must pass

**Acceptance**: Both crates compile. One placeholder test passes.

---

### Task 0.2: Domain types — state enums and event types

**Files to create:**
- `crates/openclaw-orchestrator/src/domain/mod.rs`
- `crates/openclaw-orchestrator/src/domain/states.rs`
- `crates/openclaw-orchestrator/src/domain/events.rs`
- `crates/openclaw-orchestrator/src/domain/errors.rs`
- `crates/openclaw-orchestrator/src/domain/types.rs`
- `crates/openclaw-orchestrator/src/domain/ports.rs`
- `crates/openclaw-orchestrator/src/domain/actor.rs`

**What to implement:**

**states.rs** — All 5 state enums with validated transitions (Architecture Section 31.1):
```rust
pub enum InstanceState { Provisioning, Active, Blocked(BlockReason), Suspended, ProvisioningFailed(String) }
pub enum CycleState { Created, Planning, PlanReady, Approved, Running, Blocked(BlockReason), Completing, Completed, Failed(String), Cancelled(String) }
pub enum TaskState { Scheduled, Active { active_run_id: Uuid, attempt: u32 }, Verifying { run_id: Uuid }, Passed { evidence_run_id: Uuid }, Failed { reason: String, total_attempts: u32 }, Cancelled(String), Skipped(String) }
pub enum RunState { Claimed { resource_claim_id: Uuid }, Running { worker_session_id: Uuid, lease_until: DateTime<Utc> }, Completed { exit_code: i32 }, Failed(FailureCategory), TimedOut, Cancelled(String), Abandoned(String) }
pub enum MergeState { Pending, Attempted { source_ref: String, target_ref: String }, Succeeded { merge_ref: String }, Conflicted { conflict_count: u32 }, FailedTransient { category: FailureCategory, next_attempt_after: DateTime<Utc> }, FailedPermanent { category: FailureCategory }, Skipped(String) }
```

Each enum gets a `transition(trigger) -> Result<Self, DomainError>` method enforcing the exact transitions from Section 31.1.

**events.rs** — Event envelope + all 47 event types from Section 31.2/31.5:
```rust
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

pub enum EventCriticality { StateChanging, Informational }

pub struct EventKindEntry {
    pub event_type: &'static str,
    pub current_version: u16,
    pub criticality: EventCriticality,
}

pub const EVENT_KINDS: &[EventKindEntry] = &[
    // All 38 state-changing + 9 informational from Section 31.5
];
```

**errors.rs** — Three error types per E2:
```rust
pub enum DomainError { InvalidTransition { from: String, trigger: String }, ... }
// InfraError in infra module, AppError composes both
```

**types.rs** — Budget types per E4:
```rust
pub type BudgetCents = i64;  // integer cents, never floats
pub type BasisPoints = u16;  // 10000 = 100%
```

**actor.rs** — Actor enum per E12:
```rust
pub enum Actor { Human { actor_id: String }, Planner, Worker { run_id: Uuid }, System }
```

**ports.rs** — Port traits (in domain crate per Section 26):
```rust
pub trait EventStore: Send + Sync {
    async fn emit(&self, event: EventEnvelope) -> Result<i64, DomainError>;
    async fn replay(&self, instance_id: Uuid, since_seq: i64) -> Result<Vec<EventEnvelope>, DomainError>;
}

pub trait ProjectionReader: Send + Sync { ... }
```

**Unit tests:**
- Every state enum: test all valid transitions succeed
- Every state enum: test invalid transitions return DomainError
- EVENT_KINDS: assert count of StateChanging == 38, Informational == 9

**Acceptance**: `cargo test -p openclaw-orchestrator` passes all state transition tests.

---

### Task 0.3: Database migration — orch_* tables

**Files to create:**
- `migrations/002_orchestrator.sql`

**What to implement:**

Full schema from Architecture Section 21, incorporating all review fixes:
- `orch_projects` — repo identity, config
- `orch_instances` — process-bound runtime, composite FK
- `orch_cycles` — per-prompt, composite FK (instance_id, id)
- `orch_tasks` — work units, composite FK to cycles, partial unique for idempotency
- `orch_runs` — attempt tracking, worker_session_id (not PID as domain), lease_until
- `orch_artifacts` — verification evidence, tombstone support (deleted_at nullable)
- `orch_events` — append-only, seq per instance, idempotency_key partial unique index
- `orch_budget_ledger` — integer cents (BudgetCents), per-instance
- `orch_server_capacity` — projection table for scheduler (Section 23)
- Append-only triggers (deny UPDATE/DELETE on orch_events, orch_artifacts)
- Advisory lock helper comment (big-endian i64/i64 from UUID)
- All indexes from Section 21

Key schema decisions from review:
- Composite FKs: `FOREIGN KEY (instance_id, cycle_id) REFERENCES orch_cycles(instance_id, id)` — ensures same-instance integrity
- `orch_events.idempotency_key` partial unique index: `WHERE idempotency_key IS NOT NULL`
- `cost_usd REAL` → `cost_cents BIGINT` (integer cents per E4)
- `orch_runs.worker_session_id UUID` (domain identity), `pid INT` (adapter-only, nullable)
- No `'starting'` state in CHECK constraints (per E5)

**Acceptance**: Migration applies cleanly to test Postgres. All constraints enforced.

---

### Task 0.4: Event store implementation (infra layer)

**Files to create:**
- `crates/openclaw-orchestrator/src/infra/mod.rs`
- `crates/openclaw-orchestrator/src/infra/event_store.rs`
- `crates/openclaw-orchestrator/src/infra/errors.rs`

**What to implement:**

`PgEventStore` implementing domain `EventStore` trait:
- `emit(event) -> seq`: INSERT into orch_events, return seq. Idempotency via partial unique index (duplicate idempotency_key = return existing seq, not error)
- `replay(instance_id, since_seq) -> Vec<EventEnvelope>`: SELECT ordered by seq
- Advisory lock helper: `advisory_lock_key(uuid: Uuid) -> (i64, i64)` with big-endian byte order per E1
- `subscribe() -> broadcast::Receiver<EventEnvelope>`: In-process notification via tokio::broadcast

`InfraError` type: DB errors, IO errors, git errors, spawn errors. Does NOT implement Serialize.

**Integration tests** (require test Postgres):
- Emit event → replay returns it
- Emit with duplicate idempotency_key → returns existing seq (not error)
- Replay with since_seq filters correctly
- Advisory lock key is deterministic for same UUID

**Acceptance**: Integration tests pass against test Postgres.

---

### Task 0.5: First projector and golden test

**Files to create:**
- `crates/openclaw-orchestrator/src/app/mod.rs`
- `crates/openclaw-orchestrator/src/app/projector.rs`
- `crates/openclaw-orchestrator/src/app/errors.rs`
- `crates/openclaw-orchestrator/src/app/instance_projector.rs`

**What to implement:**

Projector trait:
```rust
pub trait Projector: Send + Sync {
    async fn handle(&self, event: &EventEnvelope, tx: &mut Transaction<'_, Postgres>) -> Result<(), AppError>;
}
```

Instance projector: handles `InstanceCreated` → INSERT into orch_instances projection.

`AppError`: composes DomainError | InfraError | ConcurrencyConflict.

Golden test pattern (Section 27):
```rust
#[test]
fn golden_instance_created() {
    let event = fixture("InstanceCreated", 1); // loads from test fixtures
    let projector = InstanceProjector::new();
    // Apply event to empty DB, verify projection matches expected state
}
```

CI assertion: count of StateChanging event kinds with projector handlers == count of golden fixtures.

**Acceptance**: Golden test passes. CI assertion compiles (starts as compile_error! for unimplemented handlers).

---

### Task 0.6: Event streaming (LISTEN/NOTIFY)

**Files to create:**
- `crates/openclaw-orchestrator/src/infra/event_stream.rs`

**What to implement:**

Postgres LISTEN/NOTIFY for new events:
- On event emit: `NOTIFY orch_events_channel, '{instance_id}:{seq}'`
- Subscriber: LISTEN on channel, parse notification, replay if gap detected
- `broadcast::Sender<EventEnvelope>` for in-process distribution
- Reconnect-with-backfill: on reconnect, replay from last known seq

**Acceptance**: Two concurrent subscribers both receive events in order. Gap detection works after simulated disconnect.

---

## Phase 1: Core Lifecycle (estimated: 2 weeks)

**Goal**: Full cycle lifecycle — create → plan → approve → schedule → claim → complete → pass. All through events. No workers yet (simulated completion).

### Task 1.1: Cycle + Plan projectors and domain logic

**What to implement:**
- CycleState transitions enforced in domain
- Events: CycleCreated, PlanRequested, PlanGenerated, PlanApproved, CycleRunningStarted, CycleCompleting, CycleCompleted, CycleFailed, CycleCancelled, CycleBlocked, CycleUnblocked
- Cycle projector: updates orch_cycles on each event
- Golden tests for every cycle state-changing event
- Co-emission: PlanApproved + PlanBudgetReserved in same tx (Section 31.3 rule 1)

### Task 1.2: Task + Run projectors and domain logic

**What to implement:**
- TaskState + RunState transitions enforced in domain
- Events: TaskScheduled, TaskRetryScheduled, TaskPassed, TaskFailed, TaskCancelled, TaskSkipped, RunClaimed, RunStarted, RunCompleted, RunFailed, RunTimedOut, RunCancelled, RunAbandoned
- Task projector: updates orch_tasks
- Run projector: updates orch_runs
- Anti-pair enforcement: RunClaimed and RunStarted are never in same tx (Section 31.4)
- Golden tests for every task/run state-changing event

### Task 1.3: Budget engine

**What to implement:**
- Budget domain types: BudgetCents (i64), BasisPoints (u16) — no floats anywhere
- Events: PlanBudgetReserved, RunBudgetReserved, RunCostObserved, RunBudgetSettled, CycleBudgetSettled, BudgetWarningIssued, GlobalBudgetCapHit
- Budget projector: updates orch_budget_ledger
- Co-emission enforcement for all budget pairs (Section 31.3 rules 1-8)
- Per-instance scope (E15), optional global cap
- Unit tests: budget math is integer-only, settlement releases correct amounts

### Task 1.4: Scheduler

**What to implement:**
- Claim tasks via `SELECT ... FOR UPDATE SKIP LOCKED` (Architecture Section 4)
- Concurrency enforcement: per-instance max + global cap via orch_server_capacity projection
- Two-phase tick: claim phase → spawn phase (Section 23)
- CycleRunningStarted emitted on first claim per cycle (separate event, not merged with TaskScheduled)
- Advisory locks: bigint/bigint from UUID (E1)
- Integration test: concurrent schedulers don't double-claim

### Task 1.5: End-to-end lifecycle integration test

**What to implement:**
- Test that creates instance → project → cycle → plan → approve → schedule tasks → claim runs → simulate completion → tasks pass → cycle completes
- All through events, verified by replaying and checking final projections
- Budget reserved at approval, consumed during run, settled at completion
- No HTTP, no workers — pure domain + app + infra

**Acceptance**: Full lifecycle test passes. Event replay reproduces identical projection state.

---

## Phase 2: Workers & Verification (estimated: 1.5 weeks)

**Goal**: Real Claude Code workers executing tasks in git worktrees.

### Task 2.1: Worker spawn (infra)

**What to implement:**
- Spawn `claude --print --output-format json` as subprocess
- stdout/stderr redirected to files: `{data_dir}/{instance_id}/{run_id}/stdout.log`
- PID tracking (adapter-level only, stored in orch_runs.pid)
- worker_session_id (domain-level, UUID) set at claim time
- RunStarted event on proof-of-life (worker reports in)
- Heartbeat mechanism: worker sends periodic signal, lease_until extended
- Server-side log tailer: reads new bytes from stdout.log, emits WorkerOutput events

### Task 2.2: Worker reattach on restart

**What to implement:**
- On startup: scan orch_runs for state = 'running'
- For each: verify PID alive AND session_token_hash matches (per E3)
- PID alive + token match → reattach (resume log tailing)
- PID alive + token mismatch → RunAbandoned (different process, original is dead)
- PID dead → RunAbandoned → maybe retry
- Actor: System (reconciler-only, per Section 31.7)

### Task 2.3: Verification pipeline

**What to implement:**
- VerifyCheck trait: `name() -> &str`, `run(ctx) -> CheckResult`
- Built-in checks: cargo_check, cargo_test, cargo_fmt, cargo_clippy, custom command
- Configured per-project via orch_projects.config JSON
- Artifact storage: orch_artifacts table with tombstone support (deleted_at, per Section 24)
- TaskPassed requires all artifacts have passed=true
- Events: VerifyStarted (informational), VerifyResult (informational), TaskPassed/TaskFailed (state-changing)

### Task 2.4: Git operations (infra)

**What to implement:**
- GitProvider trait (Section 13 of architecture):
  - `create_worktree(repo, branch) -> PathBuf`
  - `remove_worktree(path)`
  - `merge(repo, source, target) -> MergeResult`
  - `diff_stat(repo, base, head) -> DiffStat`
  - `current_sha(repo) -> String`
  - `status_porcelain(repo) -> Vec<FileStatus>`
- GitCli implementation: shells out to git binary
- Worktrees at `{data_dir}/worktrees/{run_id}/`
- Unit tests with mock GitProvider for CI

---

## Phase 3: Merge & Completion (estimated: 1.5 weeks)

**Goal**: Verified work merges back to target branch. Full cycle completes autonomously.

### Task 3.1: Merge workflow

**What to implement:**
- Merge state machine from Section 31.1 (MergeState)
- Events: MergeAttempted, MergeSucceeded, MergeConflicted, MergeFailed, MergeSkipped
- In-flight merge guard: at most one merge Attempted per cycle at a time
- Transient failure retry with exponential backoff
- Permanent failure blocks cycle (requires human resolution)
- Co-emission: MergeSucceeded(last task) + CycleCompleted in same tx (Section 31.3 rule 9)

### Task 3.2: Reconciler

**What to implement:**
- Three operational loops per Section 28:
  1. Scheduler tick (claims, spawns)
  2. Reconciler tick (detects anomalies, stale claims)
  3. Janitor tick (cleanup — Phase 5)
- Safe-conditions gate for auto catch-up (Section 28)
- Stale claim detection: bounded lock timeout (Section 22)
- Read-only during maintenance (E7): detect anomalies, log diagnostics, but NO state-changing events
- On maintenance exit: scheduler tick picks up anomalies

### Task 3.3: Planner integration

**What to implement:**
- Planner module: takes cycle prompt → generates structured JSON plan via Claude Code
- Plan format: phases → tasks with title, description, acceptance criteria
- PlanRequested → spawns planner → PlanGenerated (with plan JSON)
- User reviews plan in UI → PlanApproved
- Actor: Planner for plan generation, Human for approval

---

## Phase 4: HTTP & WebSocket (estimated: 1.5 weeks)

**Goal**: Web control plane for managing the orchestrator.

### Task 4.1: HTTP API (Axum)

**What to implement:**
- REST endpoints in openclaw-ui crate:
  - `POST/GET /api/v1/projects` — CRUD
  - `POST/GET /api/v1/instances` — lifecycle
  - `POST/GET /api/v1/cycles` — create cycle, list, detail
  - `POST /api/v1/cycles/:id/approve` — approve plan
  - `GET /api/v1/tasks` — list by cycle
  - `GET /api/v1/runs/:id` — run detail
  - `GET /api/v1/runs/:id/logs` — streaming log tail
  - `POST /api/v1/cycles/:id/merge` — trigger merge
  - `GET/PUT /api/v1/budgets` — budget config
  - `GET /api/v1/events?since_seq=N` — polling fallback
- Auth: token in Authorization header, argon2id hash comparison, constant-time
- Token fingerprinting: sha256(token)[0..24] hex for caching/logging (E10)
- API idempotency: DB-backed api_idempotency table (E13)
- Cursor-based pagination: (created_at, id) for entities, (instance_id, seq) for events (Section 29)
- Error mapping: AppError → HTTP status codes (bounded strings)

### Task 4.2: WebSocket event streaming

**What to implement:**
- Endpoint: `/api/v1/instances/:id/events/ws` (per-instance, per E6)
- First-message auth: `{ "type": "auth", "token": "..." }` — close if not received within 5s
- NO tokens in URL query parameters (E6)
- Backfill: client sends since_seq → server replays gap + switches to live
- Message types: event (full EventEnvelope), backfill_complete, heartbeat, error
- No snapshots via WS — snapshots come from HTTP GET endpoints (E6)

### Task 4.3: Frontend (vanilla TypeScript)

**What to implement:**
- Vanilla TS with memo/dispose discipline (Section 25)
- Component model with viewModel layer
- esbuild bundled → `include_str!()` embedded in binary
- Views:
  - Project list + create
  - Cycle detail: plan viewer, task tree, approve button
  - Worker view: live log stream, status
  - Merge view: diff display, conflict resolution
  - Budget view: cost tracking, caps
- WS connection manager: reconnect with since_seq, backfill handling
- Client-side event replay: events[] → derived view state (store.ts)

---

## Phase 5: Operations (estimated: 1 week)

**Goal**: Production-grade operational features.

### Task 5.1: Janitor

**What to implement:**
- Per-run row locking for cleanup (Section 24 — no race with live operations)
- State-dependent worktree retention: keep if run active, clean if terminal
- Tombstoned artifact cleanup: purge data but keep metadata row (Section 24)
- Disk guard: check available space before operations (Section 23)
- Scheduled via janitor tick (separate from scheduler/reconciler)

### Task 5.2: Maintenance mode

**What to implement:**
- `InstanceState::Blocked(Maintenance)` — admin API to enter/exit
- Checked at app entrypoints: scheduler, merge, planner, budget, worker spawn (E7)
- Reconciler: read-only during maintenance — logs diagnostics, no state-changing events (E7 Option A)
- Active workers: allowed to finish (not killed)
- Event streaming: continues for observability
- Projection rebuild procedure: available as admin command

### Task 5.3: Gateway integration (Telegram/Discord commands)

**What to implement:**
- Add commands to existing openclaw-gateway:
  - `/projects` — list projects
  - `/cycle <project> <prompt>` — start upgrade cycle
  - `/status [project]` — cycle/worker status
  - `/approve <cycle_id>` — approve plan
  - `/workers` — list active workers
- Commands call openclaw-orchestrator library functions directly (no HTTP hop)

### Task 5.4: CLI admin commands

**What to implement:**
- Add subcommands to openclaw-cli:
  - `openclaw orch instance create/list/status`
  - `openclaw orch cycle create/list/approve/cancel`
  - `openclaw orch reconcile` — manual reconciliation
  - `openclaw orch skip-event <event_id>` — skip blocked event
  - `openclaw orch resume <instance_id>` — resume blocked instance
  - `openclaw orch maintenance enter/exit <instance_id>`
  - `openclaw orch rebuild-projections <instance_id>`

---

## Cross-Cutting Concerns (enforce throughout all phases)

### Error Handling (E2)
- `DomainError` in domain module — pure state violations, serializable
- `InfraError` in infra module — DB, IO, git, spawn — NOT serializable
- `AppError` in app module — composes both + ConcurrencyConflict
- HTTP handlers map AppError → status codes. Never expose DomainError/InfraError directly.

### Event Discipline (Section 31)
- Every state change emits an event. No mutation without event.
- Co-emission rules enforced in app layer (Section 31.3)
- Anti-pairs documented and tested (Section 31.4)
- Actor included in every state-changing event payload (E12)
- occurred_at = server clock at emit time, never worker-supplied (E14)
- Criticality from envelope (event_type, event_version) only, never payload (E8)

### Idempotency (E13)
- DB uniqueness (orch_events.idempotency_key) = source of truth
- API idempotency cache = optional optimization, never required for correctness
- Every externally-triggered operation safely repeatable

### Budget Integrity (E4, E15)
- Integer cents only. No floats. No f32. No f64.
- Per-instance scope. Optional global cap.
- Co-emission with lifecycle events (Section 31.3)

### Testing Strategy
- **Domain unit tests**: State transition validation, budget math
- **Golden tests**: Every state-changing event has a fixture + projector test (Section 27)
- **Integration tests**: Event store round-trip, scheduler concurrency, lifecycle end-to-end
- **CI gate**: count(StateChanging event kinds by type+version) == count(projector handlers) == count(golden fixtures)

### Security
- Default bind: 127.0.0.1 only
- Token auth: constant-time comparison, argon2id storage
- Token fingerprint: sha256[0..24] hex for caching/logging (E10)
- First-message WS auth, no tokens in URLs (E6)
- Secrets redaction: env vars matching patterns stripped from logs
- Append-only event log (DB trigger enforced)
- CSP headers on frontend responses

---

## Architecture Document Reference

The canonical architecture is in `/home/shawaz/openclaw-architecture.md` (~9500 lines, 31 sections). Key sections for implementers:

| Section | Topic | Key Decisions |
|---------|-------|---------------|
| 1 | Event System | Crate layering, emit/replay, subscribe |
| 4 | Scheduler | SKIP LOCKED, concurrency caps |
| 6 | Worker Manager | Spawn, log tailing, reattach |
| 14 | Worker Identity | session_id domain, PID adapter |
| 16 | Budget | Integer cents, co-emission |
| 18 | Security | Token storage, auth, CSP |
| 21 | Database Schema | Full DDL, composite FKs, indexes |
| 22 | Error Handling | Three-enum split, crash recovery |
| 23 | Performance | orch_server_capacity, watermark cache |
| 24 | Data Retention | Tombstones, worktree cleanup |
| 25 | Frontend | Vanilla TS, memo/dispose, WS wake-up |
| 26 | Crate Structure | 6-crate dep diagram, ports in domain |
| 27 | Event Versioning | EVENT_KINDS, criticality, golden tests |
| 28 | Reconciliation | Three loops, safe-conditions |
| 29 | API Versioning | Cursor pagination, rate limits |
| 30 | Consistency Errata | E1-E15 canonical decisions |
| 31 | State & Event Catalog | Full state machines, mapping table, co-emission |

---

## Existing Code to Reuse

| Component | Source | Reuse |
|-----------|--------|-------|
| Postgres pool | openclaw-db | Shared pool, add orch_* migrations |
| Config parsing | openclaw-core | Load orchestrator TOML config |
| LLM provider | openclaw-agent | Planner uses Claude via existing LLM stack |
| Tool system | openclaw-agent | Workers use claude CLI directly (subprocess) |
| Telegram/Discord | openclaw-gateway | Add /projects, /cycle, /approve commands |
| CLI framework | openclaw-cli | Add orch subcommands |

---

## Decision Log (from 37-cycle review)

| # | Decision | Rationale | Architecture Section |
|---|----------|-----------|---------------------|
| E1 | Advisory locks: i64/i64 big-endian from UUID | Full 128 bits, no collision, deterministic | 30 |
| E2 | Three error types: Domain/Infra/App | No leakage across boundaries | 30 |
| E3 | Worker identity: worker_session_id (domain), PID (adapter) | Domain doesn't know about OS processes | 30 |
| E4 | Budget: integer cents, basis points, never floats | Precision, testability | 30 |
| E5 | RunState: no "starting" — Claimed → Running | Clean state machine | 30 |
| E6 | WS: first-message auth, no URL tokens, no snapshots | Security, simplicity | 30 |
| E7 | Maintenance: reconciler read-only (Option A) | Simplest to reason about | 30 |
| E8 | Unknown events: halt instance, not server | Isolation, recoverability | 30 |
| E9 | Event envelope: 11 fields including correlation/causation | Audit, replay, ordering | 30 |
| E10 | Token fingerprint: sha256[0..24] hex (96 bits) | Collision-safe, compact | 30 |
| E11 | Projection ownership: app = policy, infra = mechanics | Clean dependency direction | 30 |
| E12 | Actor in events: Human/Planner/Worker/System | Audit trails | 30 |
| E13 | Idempotency: DB uniqueness = truth, API cache = optimization | Correctness without cache | 30 |
| E14 | Time: occurred_at = server clock, never worker | Consistency, replay safety | 30 |
| E15 | Budget scope: per-instance, optional global cap | Isolation, simplicity | 30 |
