-- ============================================================
-- RUNTIME TABLES (replacing SQLite)
-- ============================================================

CREATE TABLE IF NOT EXISTS sessions (
    id              BIGSERIAL PRIMARY KEY,
    session_key     TEXT UNIQUE NOT NULL,
    agent_name      TEXT NOT NULL,
    model           TEXT NOT NULL DEFAULT '',
    channel         TEXT,
    user_id         TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    total_tokens    BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_sessions_agent ON sessions(agent_name, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_channel ON sessions(channel);

CREATE TABLE IF NOT EXISTS messages (
    id                  BIGSERIAL PRIMARY KEY,
    session_id          BIGINT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role                TEXT NOT NULL,
    content             TEXT,
    reasoning_content   TEXT,
    tool_calls_json     JSONB,
    tool_call_id        TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);

-- ============================================================
-- LLM CALL LOG
-- ============================================================

CREATE TABLE IF NOT EXISTS llm_calls (
    id                      UUID PRIMARY KEY,
    session_key             TEXT,
    model                   TEXT NOT NULL,
    provider_attempt        SMALLINT NOT NULL DEFAULT 1,
    messages_count          INT NOT NULL DEFAULT 0,
    request_tokens_est      INT NOT NULL DEFAULT 0,
    streaming               BOOLEAN NOT NULL DEFAULT false,
    response_content        TEXT,
    response_reasoning      TEXT,
    response_tool_calls     INT NOT NULL DEFAULT 0,
    tool_call_names         TEXT[],
    usage_prompt_tokens     INT NOT NULL DEFAULT 0,
    usage_completion_tokens INT NOT NULL DEFAULT 0,
    usage_total_tokens      INT NOT NULL DEFAULT 0,
    latency_ms              INT NOT NULL DEFAULT 0,
    error                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_llm_calls_session ON llm_calls(session_key);
CREATE INDEX IF NOT EXISTS idx_llm_calls_model ON llm_calls(model, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_calls_errors ON llm_calls(created_at DESC) WHERE error IS NOT NULL;

-- ============================================================
-- METRICS SNAPSHOTS
-- ============================================================

CREATE TABLE IF NOT EXISTS metrics_snapshots (
    id                      BIGSERIAL PRIMARY KEY,
    host                    TEXT NOT NULL,
    telegram_requests       BIGINT NOT NULL DEFAULT 0,
    discord_requests        BIGINT NOT NULL DEFAULT 0,
    telegram_errors         BIGINT NOT NULL DEFAULT 0,
    discord_errors          BIGINT NOT NULL DEFAULT 0,
    rate_limited            BIGINT NOT NULL DEFAULT 0,
    concurrency_rejected    BIGINT NOT NULL DEFAULT 0,
    completed_requests      BIGINT NOT NULL DEFAULT 0,
    total_latency_ms        BIGINT NOT NULL DEFAULT 0,
    agent_turns             BIGINT NOT NULL DEFAULT 0,
    tool_calls              BIGINT NOT NULL DEFAULT 0,
    webhook_requests        BIGINT NOT NULL DEFAULT 0,
    tasks_cancelled         BIGINT NOT NULL DEFAULT 0,
    agent_timeouts          BIGINT NOT NULL DEFAULT 0,
    process_rss_bytes       BIGINT NOT NULL DEFAULT 0,
    uptime_seconds          BIGINT NOT NULL DEFAULT 0,
    snapshot_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_metrics_host_time ON metrics_snapshots(host, snapshot_at DESC);

-- ============================================================
-- CRON EXECUTION LOG
-- ============================================================

CREATE TABLE IF NOT EXISTS cron_executions (
    id              BIGSERIAL PRIMARY KEY,
    job_name        TEXT NOT NULL,
    agent_name      TEXT NOT NULL,
    model           TEXT,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    duration_ms     INT,
    status          TEXT NOT NULL DEFAULT 'running',
    error           TEXT,
    tokens_used     INT NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_cron_job ON cron_executions(job_name, started_at DESC);

-- ============================================================
-- WINDSURF CONTEXT TABLES
-- ============================================================

CREATE TABLE IF NOT EXISTS deployments (
    id              BIGSERIAL PRIMARY KEY,
    host            TEXT NOT NULL,
    component       TEXT NOT NULL,
    version         TEXT NOT NULL,
    binary_path     TEXT,
    service_name    TEXT,
    git_commit      TEXT,
    built_at        TIMESTAMPTZ,
    deployed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    status          TEXT NOT NULL DEFAULT 'active',
    notes           TEXT
);

CREATE INDEX IF NOT EXISTS idx_deployments_host ON deployments(host, component, deployed_at DESC);

CREATE TABLE IF NOT EXISTS config_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    config_path     TEXT NOT NULL,
    config_json     JSONB NOT NULL,
    changed_keys    TEXT[],
    change_source   TEXT,
    snapshot_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_config_time ON config_snapshots(snapshot_at DESC);

CREATE TABLE IF NOT EXISTS known_issues (
    id              BIGSERIAL PRIMARY KEY,
    title           TEXT NOT NULL,
    description     TEXT,
    severity        TEXT NOT NULL DEFAULT 'medium',
    status          TEXT NOT NULL DEFAULT 'open',
    component       TEXT,
    workaround      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at     TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_issues_status ON known_issues(status, severity);

CREATE TABLE IF NOT EXISTS task_log (
    id              BIGSERIAL PRIMARY KEY,
    description     TEXT NOT NULL,
    category        TEXT,
    files_changed   TEXT[],
    version         TEXT,
    git_commit      TEXT,
    completed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_task_time ON task_log(completed_at DESC);
CREATE INDEX IF NOT EXISTS idx_task_category ON task_log(category);

CREATE TABLE IF NOT EXISTS project_context (
    key             TEXT PRIMARY KEY,
    value           JSONB NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
