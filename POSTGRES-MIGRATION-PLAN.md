# OpenClaw-RS → PostgreSQL Migration Plan

## Goal

Replace SQLite session storage with PostgreSQL and add context tables that the
Windsurf MCP Postgres server can query during coding sessions, giving Cascade
arbitrary read access to runtime state, deployment history, config snapshots,
and agent activity.

---

## Current State

| Component | Storage | Location |
|---|---|---|
| Sessions + Messages | SQLite (rusqlite) | `~/.openclaw/agents/{name}/sessions.db` |
| Metrics | In-memory atomics | Lost on restart |
| LLM call log | In-memory VecDeque (256 cap) | Lost on restart |
| Config | JSON file | `~/.openclaw/openclaw-manual.json` |
| Cron history | JSON file | `~/.openclaw/cron/jobs.json` |

**Problems:**
- Metrics and LLM logs are lost on every restart
- SQLite DB is per-agent, per-machine — no cross-machine visibility
- Windsurf has zero runtime visibility into the gateway
- Config changes are not tracked (watchdog copies known-good.json but no history)

---

## Architecture

```
┌──────────────────────┐          ┌─────────────────────┐
│  openclaw-gateway    │  writes  │                     │  reads (MCP)
│  openclaw-agent      │ ───────► │  PostgreSQL :5432   │ ◄──────────── Windsurf
│  openclaw-cli        │          │  DB: openclaw       │
└──────────────────────┘          └─────────────────────┘
                                         │
                                    Docker volume
                                  (persistent data)
```

Postgres runs in Docker on the **nixos** desktop (192.168.50.3).
The gateway on **g731** connects over LAN.
Windsurf MCP connects to localhost:5432.

---

## Phase 1: Infrastructure (Docker + Schema)

### 1a. Docker Compose for Postgres

File: `docker-compose.postgres.yml`

```yaml
version: "3.8"
services:
  postgres:
    image: postgres:17-alpine
    container_name: openclaw-postgres
    restart: unless-stopped
    ports:
      - "127.0.0.1:5432:5432"       # localhost only
      - "192.168.50.3:5432:5432"     # LAN for g731
    environment:
      POSTGRES_DB: openclaw
      POSTGRES_USER: openclaw
      POSTGRES_PASSWORD: openclaw-local
    volumes:
      - openclaw-pgdata:/var/lib/postgresql/data
      - ./migrations/init.sql:/docker-entrypoint-initdb.d/init.sql
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U openclaw"]
      interval: 10s
      timeout: 5s
      retries: 5

volumes:
  openclaw-pgdata:
```

### 1b. Schema DDL

File: `migrations/init.sql`

```sql
-- ============================================================
-- RUNTIME TABLES (replacing SQLite)
-- ============================================================

CREATE TABLE IF NOT EXISTS sessions (
    id              BIGSERIAL PRIMARY KEY,
    session_key     TEXT UNIQUE NOT NULL,
    agent_name      TEXT NOT NULL,
    model           TEXT NOT NULL DEFAULT '',
    channel         TEXT,                          -- 'telegram', 'discord', 'cli', 'cron'
    user_id         TEXT,                          -- platform user ID
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    total_tokens    BIGINT NOT NULL DEFAULT 0
);

CREATE INDEX idx_sessions_agent ON sessions(agent_name, updated_at DESC);
CREATE INDEX idx_sessions_channel ON sessions(channel);

CREATE TABLE IF NOT EXISTS messages (
    id                  BIGSERIAL PRIMARY KEY,
    session_id          BIGINT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    role                TEXT NOT NULL,              -- 'system', 'user', 'assistant', 'tool'
    content             TEXT,
    reasoning_content   TEXT,
    tool_calls_json     JSONB,                     -- native JSON, queryable
    tool_call_id        TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_messages_session ON messages(session_id, id);

-- ============================================================
-- LLM CALL LOG (currently in-memory, lost on restart)
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
    tool_call_names         TEXT[],                -- Postgres array
    usage_prompt_tokens     INT NOT NULL DEFAULT 0,
    usage_completion_tokens INT NOT NULL DEFAULT 0,
    usage_total_tokens      INT NOT NULL DEFAULT 0,
    latency_ms              INT NOT NULL DEFAULT 0,
    error                   TEXT,
    created_at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_llm_calls_session ON llm_calls(session_key);
CREATE INDEX idx_llm_calls_model ON llm_calls(model, created_at DESC);
CREATE INDEX idx_llm_calls_errors ON llm_calls(created_at DESC) WHERE error IS NOT NULL;

-- ============================================================
-- METRICS SNAPSHOTS (currently in-memory atomics, lost on restart)
-- ============================================================

CREATE TABLE IF NOT EXISTS metrics_snapshots (
    id                      BIGSERIAL PRIMARY KEY,
    host                    TEXT NOT NULL,          -- 'nixos', 'g731'
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

CREATE INDEX idx_metrics_host_time ON metrics_snapshots(host, snapshot_at DESC);

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
    status          TEXT NOT NULL DEFAULT 'running',  -- 'running', 'success', 'error'
    error           TEXT,
    tokens_used     INT NOT NULL DEFAULT 0
);

CREATE INDEX idx_cron_job ON cron_executions(job_name, started_at DESC);

-- ============================================================
-- WINDSURF CONTEXT TABLES
-- ============================================================

-- Current state of each deployment target
CREATE TABLE IF NOT EXISTS deployments (
    id              BIGSERIAL PRIMARY KEY,
    host            TEXT NOT NULL,                  -- 'nixos', 'g731'
    component       TEXT NOT NULL,                  -- 'gateway', 'cli', 'node-gateway'
    version         TEXT NOT NULL,
    binary_path     TEXT,
    service_name    TEXT,                           -- systemd unit name
    git_commit      TEXT,
    built_at        TIMESTAMPTZ,
    deployed_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    status          TEXT NOT NULL DEFAULT 'active', -- 'active', 'stopped', 'failed'
    notes           TEXT
);

CREATE INDEX idx_deployments_host ON deployments(host, component, deployed_at DESC);

-- Config change history (JSON snapshots with diff tracking)
CREATE TABLE IF NOT EXISTS config_snapshots (
    id              BIGSERIAL PRIMARY KEY,
    config_path     TEXT NOT NULL,
    config_json     JSONB NOT NULL,
    changed_keys    TEXT[],                         -- which top-level keys changed
    change_source   TEXT,                           -- 'manual', 'watchdog', 'windsurf', 'cli'
    snapshot_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_config_time ON config_snapshots(snapshot_at DESC);

-- Known issues, bugs, workarounds (Windsurf can query these for context)
CREATE TABLE IF NOT EXISTS known_issues (
    id              BIGSERIAL PRIMARY KEY,
    title           TEXT NOT NULL,
    description     TEXT,
    severity        TEXT NOT NULL DEFAULT 'medium', -- 'low', 'medium', 'high', 'critical'
    status          TEXT NOT NULL DEFAULT 'open',   -- 'open', 'resolved', 'wontfix'
    component       TEXT,                           -- 'gateway', 'agent', 'cli', 'core', 'infra'
    workaround      TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolved_at     TIMESTAMPTZ
);

CREATE INDEX idx_issues_status ON known_issues(status, severity);

-- Task log (what was done, when, by whom — for Windsurf session continuity)
CREATE TABLE IF NOT EXISTS task_log (
    id              BIGSERIAL PRIMARY KEY,
    description     TEXT NOT NULL,
    category        TEXT,                           -- 'feature', 'bugfix', 'refactor', 'deploy', 'config'
    files_changed   TEXT[],
    version         TEXT,
    git_commit      TEXT,
    completed_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_task_time ON task_log(completed_at DESC);
CREATE INDEX idx_task_category ON task_log(category);

-- Project-level key-value store (arbitrary context for Windsurf)
CREATE TABLE IF NOT EXISTS project_context (
    key             TEXT PRIMARY KEY,
    value           JSONB NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

---

## Phase 2: Crate Changes

### 2a. New crate: `openclaw-db`

A thin shared database layer used by all other crates.

```
crates/openclaw-db/
├── Cargo.toml
└── src/
    ├── lib.rs          # Pool init, connection helpers
    ├── sessions.rs     # SessionStore (Postgres version)
    ├── llm_log.rs      # LLM call persistence
    ├── metrics.rs      # Metrics snapshot writer
    ├── context.rs      # Windsurf context tables (deployments, issues, tasks, kv)
    └── migrations.rs   # Embedded migrations via sqlx::migrate!
```

**Dependencies:**
```toml
[dependencies]
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "chrono", "uuid", "json"] }
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
chrono = { workspace = true }
uuid = { workspace = true }
tracing = { workspace = true }
```

### 2b. Connection pool

```rust
// crates/openclaw-db/src/lib.rs
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub async fn connect(database_url: &str) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
```

**Connection string:** `DATABASE_URL=postgres://openclaw:openclaw-local@127.0.0.1:5432/openclaw`
(g731 uses `@192.168.50.3:5432/openclaw`)

### 2c. Dependency graph change

```
Before:                          After:
openclaw-gateway                 openclaw-gateway
  └── openclaw-agent               ├── openclaw-agent
        └── openclaw-core          │     └── openclaw-core
                                   └── openclaw-db
                                         └── sqlx

openclaw-cli                     openclaw-cli
  ├── openclaw-agent               ├── openclaw-agent
  └── openclaw-core                ├── openclaw-core
                                   └── openclaw-db
```

---

## Phase 3: Session Migration (SQLite → Postgres)

### Strategy: Dual-write, then cutover

1. **Add `openclaw-db` as optional dependency** to `openclaw-agent`
2. **Feature flag:** `postgres` (default off)
   - When off: existing SQLite code unchanged
   - When on: writes go to both SQLite and Postgres
3. **Migration script** (`openclaw migrate-sessions`):
   - Reads all sessions + messages from SQLite
   - Inserts into Postgres with original timestamps preserved
   - Reports count and any conflicts
4. **Cutover:** flip feature flag to `postgres`-only, remove SQLite code
5. **Cleanup:** remove `rusqlite` dependency

### API compatibility

The `SessionStore` public API stays identical:
```rust
pub struct SessionStore { pool: PgPool }

impl SessionStore {
    pub async fn open(pool: &PgPool) -> Result<Self>;  // was sync, now async
    pub async fn create_session(...) -> Result<()>;
    pub async fn append_message(...) -> Result<()>;
    pub async fn load_messages(...) -> Result<Vec<SessionMessage>>;
    pub async fn load_llm_messages(...) -> Result<Vec<Message>>;
    pub async fn list_sessions(...) -> Result<Vec<SessionInfo>>;
    // ... etc
}
```

**Breaking change:** all methods become `async`. Callers in `handler.rs`,
`discord_handler.rs`, and `runtime.rs` already run in async context, so this
is straightforward.

---

## Phase 4: LLM Log Persistence

Currently `llm_log.rs` uses an in-memory `VecDeque<LlmLogEntry>` capped at 256.

### Change:
- Keep the in-memory buffer for fast `/logs` endpoint access
- **Also** write each entry to `llm_calls` table via `openclaw-db`
- The `/logs/:id` endpoint can fall back to Postgres for entries that aged out of memory

### Windsurf benefit:
```sql
-- "What models were used today and how did they perform?"
SELECT model, count(*), avg(latency_ms), count(*) FILTER (WHERE error IS NOT NULL) as errors
FROM llm_calls WHERE created_at > now() - interval '1 day'
GROUP BY model ORDER BY count(*) DESC;

-- "Show me the last 5 errors"
SELECT model, error, latency_ms, created_at
FROM llm_calls WHERE error IS NOT NULL
ORDER BY created_at DESC LIMIT 5;
```

---

## Phase 5: Metrics Persistence

### Change:
- Every 60 seconds, snapshot current atomic counters to `metrics_snapshots`
- Background task in gateway: `tokio::spawn(metrics_snapshot_loop(pool, interval))`
- Counters continue to work in-memory for real-time `/metrics` endpoint

### Windsurf benefit:
```sql
-- "How many requests did the bot handle this week?"
SELECT host, sum(completed_requests) FROM metrics_snapshots
WHERE snapshot_at > now() - interval '7 days' GROUP BY host;

-- "Error rate trend over last 24h"
SELECT date_trunc('hour', snapshot_at) as hour,
       avg(telegram_errors + discord_errors)::numeric / nullif(avg(completed_requests), 0) * 100 as error_pct
FROM metrics_snapshots WHERE snapshot_at > now() - interval '1 day'
GROUP BY hour ORDER BY hour;
```

---

## Phase 6: Context Tables + Write Hooks

### Deployments
- `openclaw-cli` writes a deployment record after each `cargo build --release` + service restart
- New CLI command: `openclaw deploy log` — records current version/host/commit

### Config snapshots
- Watchdog `poststart.sh` calls `openclaw config snapshot` after successful start
- Stores full JSON + diff of changed keys vs previous snapshot

### Known issues
- CLI command: `openclaw issues add "title" --severity high --component gateway`
- CLI command: `openclaw issues resolve 42`
- Windsurf can query these during debugging sessions

### Task log
- CLI command: `openclaw task log "description" --category feature --version v1.16.0`
- Or: gateway auto-logs after each version bump commit

### Project context (KV store)
- Arbitrary key-value pairs for Windsurf context
- `openclaw context set "current_focus" '{"task": "postgres migration", "phase": 3}'`
- `openclaw context get "current_focus"`

---

## Phase 7: Windsurf MCP Wiring

### MCP server config update

The Windsurf Postgres MCP server needs the connection string:
```json
{
  "postgres": {
    "connectionString": "postgres://openclaw:openclaw-local@127.0.0.1:5432/openclaw"
  }
}
```

### Example Windsurf queries (what I can do after migration):

```sql
-- "What's deployed where?"
SELECT host, component, version, status, deployed_at
FROM deployments WHERE status = 'active'
ORDER BY deployed_at DESC;

-- "What changed in the last config update?"
SELECT changed_keys, change_source, snapshot_at
FROM config_snapshots ORDER BY snapshot_at DESC LIMIT 1;

-- "Any open issues I should know about?"
SELECT title, severity, component, workaround
FROM known_issues WHERE status = 'open' ORDER BY severity DESC;

-- "What was done in the last session?"
SELECT description, category, version, completed_at
FROM task_log ORDER BY completed_at DESC LIMIT 10;

-- "How is the bot performing?"
SELECT model, count(*) as calls, avg(latency_ms)::int as avg_ms,
       count(*) FILTER (WHERE error IS NOT NULL) as errors
FROM llm_calls WHERE created_at > now() - interval '24 hours'
GROUP BY model;

-- "Show me the current project focus"
SELECT value FROM project_context WHERE key = 'current_focus';
```

---

## Implementation Order

| Step | Description | Crates touched | Est. effort |
|------|-------------|----------------|-------------|
| 1 | Docker Compose + init.sql | infra only | 30 min |
| 2 | Create `openclaw-db` crate with pool + migrations | new crate | 1 hour |
| 3 | Context tables: deployments, issues, task_log, kv | openclaw-db, openclaw-cli | 2 hours |
| 4 | LLM log persistence (dual-write) | openclaw-db, openclaw-agent | 1 hour |
| 5 | Metrics snapshot loop | openclaw-db, openclaw-gateway | 1 hour |
| 6 | Session migration (dual-write + migrate script) | openclaw-db, openclaw-agent | 3 hours |
| 7 | Session cutover (remove SQLite) | openclaw-agent | 1 hour |
| 8 | Wire MCP, test queries | config only | 30 min |
| 9 | CLI commands for context tables | openclaw-cli | 2 hours |

**Total estimated: ~12 hours across 2-3 sessions**

---

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Postgres down → gateway can't start | Graceful fallback: log warning, skip DB writes, keep in-memory only |
| g731 can't reach nixos:5432 | Open firewall port 5432 for 192.168.50.0/24 in NixOS config |
| SQLite migration data loss | Dual-write phase ensures both stores have data; migration script is idempotent |
| `sqlx` compile-time query checking | Use `sqlx::query!` with `DATABASE_URL` in `.env` for checked queries, or `sqlx::query()` for runtime-only |
| Binary size increase from sqlx | ~2-3 MB increase; acceptable for the functionality gained |
| Async session API breaking change | All callers already in async context; mechanical `.await` additions |

---

## Environment Variables

```bash
# Gateway service (g731)
DATABASE_URL=postgres://openclaw:openclaw-local@192.168.50.3:5432/openclaw

# Gateway service (nixos, if running locally)
DATABASE_URL=postgres://openclaw:openclaw-local@127.0.0.1:5432/openclaw

# Windsurf MCP
# Already configured, just needs Postgres running on :5432
```

---

## Files to Create/Modify

### New files:
- `docker-compose.postgres.yml`
- `migrations/init.sql`
- `crates/openclaw-db/Cargo.toml`
- `crates/openclaw-db/src/lib.rs`
- `crates/openclaw-db/src/sessions.rs`
- `crates/openclaw-db/src/llm_log.rs`
- `crates/openclaw-db/src/metrics.rs`
- `crates/openclaw-db/src/context.rs`
- `crates/openclaw-db/src/migrations.rs`

### Modified files:
- `Cargo.toml` (workspace members)
- `crates/openclaw-agent/Cargo.toml` (add openclaw-db dep)
- `crates/openclaw-agent/src/sessions.rs` (Postgres backend)
- `crates/openclaw-agent/src/llm_log.rs` (dual-write to Postgres)
- `crates/openclaw-gateway/Cargo.toml` (add openclaw-db dep)
- `crates/openclaw-gateway/src/main.rs` (init pool, pass to handlers)
- `crates/openclaw-gateway/src/metrics.rs` (snapshot loop)
- `crates/openclaw-cli/Cargo.toml` (add openclaw-db dep)
- `crates/openclaw-cli/src/main.rs` (new commands: migrate, deploy, issues, task, context)
- `crates/openclaw-cli/src/commands/` (new command modules)
- `/etc/nixos/configuration.nix` (firewall: open 5432 for LAN)
