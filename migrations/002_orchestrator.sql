-- ============================================================
-- SDLC ORCHESTRATOR TABLES
-- Migration: 002_orchestrator.sql
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
-- Composite unique on (project_id, name) for same-project isolation
CREATE TABLE IF NOT EXISTS orch_instances (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id      UUID NOT NULL REFERENCES orch_projects(id),
    name            TEXT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'provisioning'
                    CHECK (state IN ('provisioning', 'active', 'blocked', 'suspended', 'provisioning_failed')),
    block_reason    TEXT,
    pid             INT,
    bind_addr       TEXT,
    data_dir        TEXT NOT NULL,
    token_hash      TEXT NOT NULL,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_heartbeat  TIMESTAMPTZ NOT NULL DEFAULT now(),
    config          JSONB NOT NULL DEFAULT '{}',
    UNIQUE(project_id, name)
);

-- Cycles: one per raw prompt/upgrade request
-- Composite FK: (instance_id) references orch_instances(id)
CREATE TABLE IF NOT EXISTS orch_cycles (
    id          UUID NOT NULL DEFAULT gen_random_uuid(),
    instance_id UUID NOT NULL REFERENCES orch_instances(id),
    state       TEXT NOT NULL DEFAULT 'created'
                CHECK (state IN ('created', 'planning', 'plan_ready', 'approved', 'running',
                                 'blocked', 'completing', 'completed', 'failed', 'cancelled')),
    prompt      TEXT NOT NULL,
    plan        JSONB,
    block_reason TEXT,
    failure_reason TEXT,
    cancel_reason TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (instance_id, id)
);

-- Tasks: work units with acceptance criteria
-- Composite FK to cycles for same-instance integrity
CREATE TABLE IF NOT EXISTS orch_tasks (
    id              UUID NOT NULL DEFAULT gen_random_uuid(),
    cycle_id        UUID NOT NULL,
    instance_id     UUID NOT NULL,
    task_key        TEXT NOT NULL,
    phase           TEXT NOT NULL,
    ordinal         INT NOT NULL,
    state           TEXT NOT NULL DEFAULT 'scheduled'
                    CHECK (state IN ('scheduled', 'active', 'verifying', 'passed',
                                     'failed', 'cancelled', 'skipped')),
    active_run_id   UUID,
    current_attempt INT NOT NULL DEFAULT 0,
    title           TEXT NOT NULL,
    description     TEXT NOT NULL,
    acceptance      JSONB NOT NULL,
    max_retries     INT NOT NULL DEFAULT 3,
    failure_reason  TEXT,
    cancel_reason   TEXT,
    skip_reason     TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id),
    UNIQUE (instance_id, id),
    UNIQUE (instance_id, cycle_id, task_key),
    FOREIGN KEY (instance_id, cycle_id)
        REFERENCES orch_cycles(instance_id, id)
);

-- Runs: each attempt is a separate row
-- worker_session_id is domain identity, pid is adapter-only
-- NO 'starting' state (per canonical E5)
CREATE TABLE IF NOT EXISTS orch_runs (
    id                  UUID NOT NULL DEFAULT gen_random_uuid(),
    task_id             UUID NOT NULL,
    instance_id         UUID NOT NULL,
    run_number          INT NOT NULL,
    state               TEXT NOT NULL DEFAULT 'claimed'
                        CHECK (state IN ('claimed', 'running', 'completed', 'failed',
                                         'timed_out', 'cancelled', 'abandoned')),
    worker_session_id   UUID NOT NULL,
    pid                 INT,                    -- adapter-only, nullable
    branch              TEXT,
    worktree_path       TEXT,
    prompt_sent         TEXT,
    output_json         JSONB,
    exit_code           INT,
    cost_cents          BIGINT NOT NULL DEFAULT 0,  -- integer cents, NEVER floats (E4)
    failure_category    TEXT,
    cancel_reason       TEXT,
    abandon_reason      TEXT,
    lease_until         TIMESTAMPTZ,
    log_stdout          TEXT,
    log_stderr          TEXT,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at         TIMESTAMPTZ,
    PRIMARY KEY (id),
    UNIQUE (instance_id, id),
    UNIQUE (task_id, run_number),
    FOREIGN KEY (instance_id, task_id)
        REFERENCES orch_tasks(instance_id, id)
);

-- Artifacts: verification evidence
-- Tombstone support: deleted_at nullable. When deleted, content set to '{}' but row preserved
CREATE TABLE IF NOT EXISTS orch_artifacts (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    run_id      UUID NOT NULL,
    instance_id UUID NOT NULL,
    kind        TEXT NOT NULL,
    content     JSONB NOT NULL,
    passed      BOOLEAN NOT NULL,
    deleted_at  TIMESTAMPTZ,            -- tombstone: set when artifact data purged
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (instance_id, run_id)
        REFERENCES orch_runs(instance_id, id)
);

-- Events: append-only log with strict per-instance ordering
-- This is the SINGLE SOURCE OF TRUTH for all state changes
CREATE TABLE IF NOT EXISTS orch_events (
    id              BIGSERIAL PRIMARY KEY,
    event_id        UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    instance_id     UUID NOT NULL REFERENCES orch_instances(id),
    seq             BIGINT NOT NULL,
    event_type      TEXT NOT NULL,
    event_version   SMALLINT NOT NULL DEFAULT 1,
    payload         JSONB NOT NULL DEFAULT '{}',
    idempotency_key TEXT,
    correlation_id  UUID,
    causation_id    UUID,
    occurred_at     TIMESTAMPTZ NOT NULL,
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(instance_id, seq)
);

-- Partial unique index for idempotency key enforcement
CREATE UNIQUE INDEX IF NOT EXISTS ux_events_instance_idem
    ON orch_events(instance_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

-- Budget ledger: integer cents (E4), per-instance (E15)
CREATE TABLE IF NOT EXISTS orch_budget_ledger (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id     UUID NOT NULL REFERENCES orch_instances(id),
    cycle_id        UUID,
    run_id          UUID,
    event_type      TEXT NOT NULL,      -- PlanBudgetReserved, RunBudgetReserved, etc.
    amount_cents    BIGINT NOT NULL,    -- positive = reservation/spend, negative = release
    balance_after   BIGINT NOT NULL,    -- running balance after this entry
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Server capacity: projection table for scheduler (Section 23)
-- Avoids COUNT(*) hot path for global cap checking
CREATE TABLE IF NOT EXISTS orch_server_capacity (
    instance_id     UUID NOT NULL REFERENCES orch_instances(id),
    active_runs     INT NOT NULL DEFAULT 0,
    max_concurrent  INT NOT NULL DEFAULT 3,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (instance_id)
);

-- API idempotency cache (E13) -- DB-backed for correctness
CREATE TABLE IF NOT EXISTS orch_api_idempotency (
    token_fingerprint   TEXT NOT NULL,
    idempotency_key     TEXT NOT NULL,
    response_status     INT NOT NULL,
    response_body       JSONB NOT NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at          TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (token_fingerprint, idempotency_key)
);

-- ============================================================
-- APPEND-ONLY ENFORCEMENT
-- ============================================================

CREATE OR REPLACE FUNCTION deny_mutation() RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'table % is append-only: % denied', TG_TABLE_NAME, TG_OP;
END;
$$ LANGUAGE plpgsql;

-- Drop triggers first so migration is re-runnable
DROP TRIGGER IF EXISTS orch_events_no_update ON orch_events;
CREATE TRIGGER orch_events_no_update
    BEFORE UPDATE OR DELETE ON orch_events
    FOR EACH ROW EXECUTE FUNCTION deny_mutation();

DROP TRIGGER IF EXISTS orch_artifacts_no_delete ON orch_artifacts;
CREATE TRIGGER orch_artifacts_no_delete
    BEFORE DELETE ON orch_artifacts
    FOR EACH ROW EXECUTE FUNCTION deny_mutation();

-- ============================================================
-- INDEXES
-- ============================================================

-- Events: primary query patterns
CREATE INDEX IF NOT EXISTS idx_events_instance_seq ON orch_events(instance_id, seq);
CREATE INDEX IF NOT EXISTS idx_events_instance_type ON orch_events(instance_id, event_type);
CREATE INDEX IF NOT EXISTS idx_events_correlation ON orch_events(correlation_id) WHERE correlation_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_events_causation ON orch_events(causation_id) WHERE causation_id IS NOT NULL;

-- Cycles
CREATE INDEX IF NOT EXISTS idx_cycles_instance_state ON orch_cycles(instance_id, state);

-- Tasks
CREATE INDEX IF NOT EXISTS idx_tasks_cycle_state ON orch_tasks(cycle_id, state);
CREATE INDEX IF NOT EXISTS idx_tasks_instance_state ON orch_tasks(instance_id, state);

-- Runs
CREATE INDEX IF NOT EXISTS idx_runs_task ON orch_runs(task_id, run_number);
CREATE INDEX IF NOT EXISTS idx_runs_instance_state ON orch_runs(instance_id, state);
CREATE INDEX IF NOT EXISTS idx_runs_worker_session ON orch_runs(worker_session_id);

-- Budget
CREATE INDEX IF NOT EXISTS idx_budget_instance ON orch_budget_ledger(instance_id, created_at);
CREATE INDEX IF NOT EXISTS idx_budget_cycle ON orch_budget_ledger(cycle_id) WHERE cycle_id IS NOT NULL;

-- Instances
CREATE INDEX IF NOT EXISTS idx_instances_heartbeat ON orch_instances(last_heartbeat);
CREATE INDEX IF NOT EXISTS idx_instances_project ON orch_instances(project_id);

-- API idempotency: cleanup expired entries
CREATE INDEX IF NOT EXISTS idx_api_idem_expires ON orch_api_idempotency(expires_at);

-- ============================================================
-- ADVISORY LOCK CONVENTION (COMMENT ONLY)
-- ============================================================
-- Advisory locks use pg_advisory_xact_lock(hi, lo) where:
--   hi = i64 from UUID bytes[0..8] (big-endian)
--   lo = i64 from UUID bytes[8..16] (big-endian)
-- ALWAYS use xact variant (auto-released on COMMIT/ROLLBACK)
-- ALWAYS big-endian byte order for both halves
-- See E1 in architecture doc for canonical Rust implementation
