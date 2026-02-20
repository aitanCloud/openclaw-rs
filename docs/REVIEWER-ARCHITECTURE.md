# OpenClaw Reviewer Architecture

**Version:** 0.1.0-draft
**Date:** 2026-02-19
**Status:** Proposal

---

## Problem Statement

Three interconnected problems exist in AI-driven development loops:

1. **The Trust Gap** — An AI worker cannot also be its own quality gate. The builder is optimistic by nature; it believes it solved the problem. A structurally separate adversarial reviewer is required.

2. **The Specification Problem** — The orchestrator loop is only as good as the task definition it receives. Vague tasks ("make the login page better") produce infinite loops or premature approval. The reviewer can only judge against criteria it has.

3. **The "Good Enough" Problem** — Quality is not a boolean. Code can pass tests but be unmaintainable. It can be clean but not match the spec. It can match the spec but have edge cases. "Good enough" must be decomposed into independently scorable dimensions.

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────┐
│                   ORCHESTRATOR                       │
│  Reads structured task spec                          │
│  Manages worker ↔ reviewer loop                      │
│  Enforces max iterations + human escalation          │
│  Writes loop state to Postgres                       │
└──────────┬──────────────────────┬────────────────────┘
           │                      │
           ▼                      ▼
┌──────────────────┐   ┌──────────────────────────────┐
│     WORKER       │   │         REVIEWER              │
│  (subagent)      │   │  (hardened subagent)           │
│                  │   │                                │
│  - Codes changes │   │  - Reads diff + test results   │
│  - Runs tests    │   │  - Scores each dimension       │
│  - Uses tools    │   │  - Outputs structured verdict  │
│  - Cannot review │   │  - Cannot modify code           │
│    itself        │   │  - Has NO tools except read     │
└──────────────────┘   └──────────────────────────────┘
           │                      │
           └──────────┬───────────┘
                      ▼
              ┌───────────────┐
              │   POSTGRES    │
              │  (openclaw-db)│
              │               │
              │  task_specs   │
              │  review_runs  │
              │  verdicts     │
              │  llm_calls    │
              └───────────────┘
```

---

## Structured Task Spec

The root cause of bad review loops is underspecified tasks. Every task entering the orchestrator must have explicit, machine-evaluable acceptance criteria.

### Schema

```rust
/// A structured task specification for the orchestrator loop.
pub struct TaskSpec {
    /// Human-readable task title
    pub title: String,

    /// Detailed description of what needs to be done
    pub description: String,

    /// Functional requirements — each must be independently testable
    pub functional: Vec<Criterion>,

    /// Quality constraints — code style, performance, safety
    pub quality: Vec<Criterion>,

    /// Done-criteria — the final checklist before approval
    pub done_when: Vec<Criterion>,

    /// Maximum orchestrator iterations before human escalation
    pub max_iterations: u32,

    /// Files expected to be touched (optional, for scope enforcement)
    pub expected_files: Vec<String>,
}

/// A single acceptance criterion
pub struct Criterion {
    /// Short label (e.g. "rate-limit-429")
    pub id: String,

    /// Human-readable description
    pub description: String,

    /// How to verify this criterion
    pub verification: Verification,

    /// Is this criterion required or advisory?
    pub required: bool,
}

/// How a criterion is verified
pub enum Verification {
    /// Run a command; exit code 0 = pass
    Command { command: String },

    /// AI reviewer evaluates against this description
    AiReview { prompt: String },

    /// Check that a specific string/pattern exists in a file
    FileContains { path: String, pattern: String },

    /// Manual — requires human confirmation
    Manual { instructions: String },
}
```

### Example

```toml
[task]
title = "Add rate limiting to /api/chat"
description = "Implement per-IP rate limiting on the chat endpoint"
max_iterations = 5
expected_files = ["crates/openclaw-gateway/src/main.rs"]

[[functional]]
id = "returns-429"
description = "Returns HTTP 429 after 10 requests/minute per IP"
required = true
verification = { type = "ai_review", prompt = "Verify the code returns 429 status after 10 req/min per IP" }

[[functional]]
id = "retry-after-header"
description = "429 response includes Retry-After header"
required = true
verification = { type = "ai_review", prompt = "Verify Retry-After header is set on 429 responses" }

[[quality]]
id = "no-unwrap"
description = "No new unwrap() calls in changed code"
required = true
verification = { type = "command", command = "git diff HEAD~1 | grep '+.*\\.unwrap()' && exit 1 || exit 0" }

[[done_when]]
id = "tests-pass"
description = "cargo test passes"
required = true
verification = { type = "command", command = "nix-shell -p gcc --run 'cargo test --release'" }

[[done_when]]
id = "no-warnings"
description = "No new compiler warnings"
required = false
verification = { type = "command", command = "cargo build --release 2>&1 | grep -c 'warning' | xargs test 0 -eq" }
```

---

## The Reviewer Subagent

The reviewer is a specialized subagent with a hardened, review-only system prompt. It is structurally prevented from modifying code.

### Design Principles

1. **Separation of concerns** — The reviewer CANNOT write code. It has no write tools. It can only read files, read diffs, and read test output.

2. **Adversarial posture** — The reviewer's system prompt instructs it to find problems, not confirm success. It is biased toward rejection.

3. **Structured output** — The reviewer does not produce prose. It produces a machine-parseable verdict with per-criterion scores and specific failure reasons with file/line references.

4. **Deterministic threshold** — Each criterion has a pass/fail. The orchestrator checks: all required criteria pass → task complete. Any required criterion fails → loop continues.

### Reviewer System Prompt (Template)

```
You are a code reviewer. Your job is to find problems.

You will receive:
1. A task specification with acceptance criteria
2. A git diff of the changes made
3. Test output (stdout/stderr, exit code)
4. The current state of affected files

For each criterion in the task spec, output a verdict:

{
  "criterion_id": "returns-429",
  "pass": false,
  "confidence": 0.85,
  "reason": "The rate limiter checks req/second not req/minute. See line 47 of main.rs: `Duration::from_secs(1)` should be `Duration::from_secs(60)`.",
  "file_refs": ["crates/openclaw-gateway/src/main.rs:47"]
}

Rules:
- You MUST evaluate every criterion. Do not skip any.
- If you cannot determine pass/fail, set confidence < 0.5 and explain why.
- Be specific. Reference exact file paths and line numbers.
- Do not suggest fixes. Only identify problems.
- Do not be lenient. If something is ambiguous, fail it.
```

### Verdict Schema

```rust
pub struct ReviewVerdict {
    /// Which review iteration this is
    pub iteration: u32,

    /// Overall pass/fail (all required criteria met)
    pub approved: bool,

    /// Per-criterion results
    pub criteria: Vec<CriterionVerdict>,

    /// Reviewer model used
    pub model: String,

    /// Time taken for review
    pub latency_ms: u64,
}

pub struct CriterionVerdict {
    pub criterion_id: String,
    pub pass: bool,
    pub confidence: f32,
    pub reason: String,
    pub file_refs: Vec<String>,
}
```

---

## The Orchestrator Loop

### Flow

```
1. Orchestrator receives TaskSpec
2. Orchestrator spawns Worker subagent with task description + full criteria
3. Worker codes, runs tests, produces changes
4. Orchestrator collects:
   - git diff of changes
   - test output (from Command-type criteria)
   - current file contents
5. Orchestrator spawns Reviewer subagent with:
   - TaskSpec criteria
   - git diff
   - test results
   - file contents
6. Reviewer produces ReviewVerdict
7. If verdict.approved → done, write success to DB
8. If !verdict.approved AND iteration < max_iterations:
   - Feed failing CriterionVerdicts back to Worker as context
   - Go to step 3
9. If iteration >= max_iterations → escalate to human
   - Write escalation to DB with full context
   - Notify via Telegram/Discord
```

### Orchestrator State (Postgres)

```sql
-- Task specifications
CREATE TABLE task_specs (
    id          BIGSERIAL PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT NOT NULL,
    spec_json   JSONB NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);

-- Review loop runs
CREATE TABLE review_runs (
    id            BIGSERIAL PRIMARY KEY,
    task_spec_id  BIGINT NOT NULL REFERENCES task_specs(id),
    iteration     INT NOT NULL,
    worker_model  TEXT,
    reviewer_model TEXT,
    worker_response TEXT,
    diff_text     TEXT,
    test_output   TEXT,
    verdict_json  JSONB,
    approved      BOOLEAN NOT NULL DEFAULT false,
    escalated     BOOLEAN NOT NULL DEFAULT false,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

---

## Integration with Existing Codebase

### What Already Exists

| Component | Location | Role in This Architecture |
|---|---|---|
| `subagent.rs` | `openclaw-agent/src/subagent.rs` | Worker and Reviewer are both subagents. Reviewer variant strips all tools except `read`. |
| `runtime.rs` | `openclaw-agent/src/runtime.rs` | `run_agent_turn` / `run_agent_turn_streaming` already handle the LLM loop with tool calls. The orchestrator wraps this in an outer loop. |
| `sessions.rs` | `openclaw-agent/src/sessions.rs` | Session persistence for both worker and reviewer turns. Each iteration gets its own session key. |
| `llm_log.rs` | `openclaw-agent/src/llm_log.rs` | Already dual-writes to Postgres. Every LLM call in the loop is automatically logged. |
| `openclaw-db` | `crates/openclaw-db/` | Postgres integration with `llm_log`, `metrics`, and `context` modules. New `task_specs` and `review_runs` tables extend this. |
| `ToolRegistry` | `openclaw-agent/src/tools.rs` | `ToolRegistry::without_tool()` already exists — used to strip `delegate` from subagents. Same pattern strips write tools from reviewer. |

### What Needs to Be Built

| Component | Crate | Description |
|---|---|---|
| `TaskSpec` types | `openclaw-core` | Struct definitions, TOML/JSON parsing, validation |
| `ReviewVerdict` types | `openclaw-core` | Verdict schema, serialization |
| `reviewer.rs` | `openclaw-agent` | Reviewer subagent with hardened prompt, read-only tool set, structured output parsing |
| `orchestrator.rs` | `openclaw-agent` | Outer loop: worker → test → review → feedback → repeat |
| `task_specs` table | `openclaw-db` | Postgres schema + write functions |
| `review_runs` table | `openclaw-db` | Postgres schema + write functions |
| CLI commands | `openclaw-cli` | `openclaw task run <spec.toml>`, `openclaw task status <id>`, `openclaw task list` |
| Bot commands | `openclaw-gateway` | `/task` command to submit specs via Telegram/Discord |

### Reviewer Tool Set

The reviewer subagent gets a restricted `ToolRegistry`:

```rust
fn reviewer_tools() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    // Read-only tools only
    tools.register(ReadTool);
    tools.register(GrepTool);
    tools.register(FindTool);
    tools.register(ListDirTool);
    // Explicitly NO: exec, write, patch, delegate, process, browser
    tools
}
```

---

## Cross-Machine Coordination via Postgres

With the shared Postgres instance accessible from both Windsurf desktops:

1. **Cascade on Machine A** submits a `TaskSpec` → OpenClaw runs the loop → results land in Postgres
2. **Cascade on Machine B** queries `review_runs` → sees exactly what happened, which criteria failed, what the reviewer said
3. **Either machine** can pick up escalated tasks and continue work with full context
4. **OpenClaw gateway** (running on either machine) can notify via Telegram/Discord when a task completes or escalates

The `project_context` KV store in `openclaw-db/src/context.rs` can also be used to share arbitrary state between machines — architecture decisions, current priorities, known blockers.

---

## Implementation Order

1. **Phase 1: Types** — `TaskSpec`, `Criterion`, `Verification`, `ReviewVerdict` in `openclaw-core`
2. **Phase 2: Reviewer** — Hardened reviewer subagent in `openclaw-agent/src/reviewer.rs`
3. **Phase 3: Orchestrator** — Outer loop in `openclaw-agent/src/orchestrator.rs`
4. **Phase 4: Postgres** — `task_specs` + `review_runs` tables in `openclaw-db`
5. **Phase 5: CLI** — `openclaw task run/status/list` commands
6. **Phase 6: Bot integration** — `/task` command in gateway handlers

---

## Open Questions

- **Reviewer model selection**: Should the reviewer use a different model than the worker? A stronger model reviewing weaker worker output may catch more issues, but costs more.
- **Parallel criteria evaluation**: Should Command-type criteria run in parallel with AI review, or sequentially?
- **Spec generation**: Should there be a "planner" subagent that takes a vague task and produces a structured TaskSpec before the loop begins?
- **Diff scope**: Should the reviewer see the full file or only the diff? Full file gives more context but costs more tokens.
- **Confidence thresholds**: Should low-confidence verdicts (< 0.5) trigger automatic human escalation even if the criterion "passes"?
