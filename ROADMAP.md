# OpenClaw Rust Port — Roadmap

**Version:** 0.14.0
**Last updated:** 2026-02-18
**Maintainer:** Cascade + Shawaz

---

## Legend

| Status | Meaning |
|--------|---------|
| ✅ | Shipped |
| 🚧 | In progress |
| 📋 | Planned |
| 💡 | Idea / low priority |

---

## v0.1.0 — Foundation (shipped)

- ✅ Agent runtime with tool-call loop (max 20 rounds)
- ✅ OpenAI-compatible LLM provider (Ollama, Moonshot, DeepSeek, Anthropic)
- ✅ Fallback chain with circuit breaker (>3 failures = skip)
- ✅ Tool system: `exec`, `read_file`, `write_file`, `list_dir`
- ✅ Workspace context loader (system prompt, project files)
- ✅ Session persistence (SQLite)
- ✅ CLI with streaming (SSE → stdout)
- ✅ Config from `openclaw-manual.json`

## v0.2.0 — Telegram Gateway (shipped)

- ✅ Telegram bot gateway (`@rustedCoreBot`)
- ✅ Long-polling with access control (user ID allowlist)
- ✅ Real-time streaming: SSE → mpsc channel → editMessageText in-place
  - Edit throttle: 80 chars / 400ms minimum between edits
  - Tool execution indicators: ⚙️ Running → ✅ Done
  - Multi-round indicators: 🔄 Round N
- ✅ Model name in stats footer (`2094ms · 1 round(s) · 0 tool(s) · 721 tokens · moonshot/kimi-k2.5`)
- ✅ Fallback provider tracks last successful model via RwLock
- ✅ Commands: `/start`, `/help`, `/new`, `/status`, `/model`, `/sessions`
- ✅ Docker deployment with compose (config RO, workspace RW, sessions volume)
- ✅ Health endpoint on `:3100`

## v0.3.0 — Agent Capabilities (shipped)

- ✅ **Web search tool** — DuckDuckGo HTML search, returns titles/URLs/snippets (no API key needed)
- ✅ **Web fetch tool** — fetch URLs with HTML-to-text extraction, 128KB limit, 20s timeout
- ✅ **Parallel tool execution** — `futures::join_all` runs concurrent tool calls when LLM requests multiple
- ✅ **Session history injection** — loads up to 40 recent messages from SQLite into LLM context for conversation memory

## v0.4.0 — Security & Reliability (shipped)

- ✅ **Sandbox policies** — `SandboxPolicy` struct with command blocklist (30+ dangerous patterns), path allowlist for read/write, timeout clamping
- ✅ **Timeout enforcement** — exec tool respects `sandbox.clamp_timeout()`, max 60s default
- ✅ **Rate limiting** — sliding window (10 msgs/60s per user), in-memory tracker, Telegram feedback on limit hit
- ✅ **Concurrency control** — semaphore (5 concurrent tasks), busy message when full
- ✅ **Graceful shutdown** — SIGINT handler drains active tasks (30s timeout), clean exit

## v0.5.0 — Practical Tool Upgrades (shipped)

- ✅ **List dir tool** — dedicated `list_dir` with recursive mode (3 levels), sorted entries, size display, 500 entry cap
- ✅ **Patch tool** — surgical `patch` for find-and-replace edits, uniqueness enforcement, path safety
- ✅ **Per-turn timeout** — 120s tokio::timeout wrapping entire agent turn in Telegram gateway
- ✅ **7 built-in tools** — exec, read, write, list_dir, patch, web_search, web_fetch

## v0.6.0 — Plugin System & Config (shipped)

- ✅ **Script plugin system** — load shell-based tools from `.openclaw/plugins/*.json` manifests
  - JSON manifest: name, description, parameters, command, optional timeout
  - Receives tool args as JSON on stdin, sandbox-enforced
  - Auto-discovered at each agent turn from workspace
- ✅ **Config-driven sandbox** — rate limit, concurrency, exec timeout, blocked commands all configurable via `agent.sandbox` in gateway config
- ✅ **Enhanced /status endpoint** — returns uptime, tool list, tool count, version, agent name

## v0.7.0 — Reliability & Polish (shipped)

- ✅ **Telegram Markdown rendering** — final response rendered with Markdown parse mode, automatic fallback to plain text on parse errors
- ✅ **LLM retry with backoff** — exponential backoff (1s/2s/4s) for transient errors (429, 502, 503, 504), up to 3 retries per provider
- ✅ **Refactored LLM response processing** — extracted `process_chat_response` helper for cleaner code reuse
- ✅ **Stats footer styling** — italic formatting for the stats line in Telegram responses

## v0.8.0 — Search & Discovery Tools (shipped)

- ✅ **Grep tool** — regex search across files using rg (ripgrep) with fallback to grep, smart case, glob filtering, context lines
- ✅ **Find tool** — glob-based file finder using fd with fallback to find, type filtering, max depth
- ✅ **9 built-in tools** — exec, read, write, list_dir, patch, grep, find, web_search, web_fetch

## v0.9.0 — Context Intelligence (shipped)

- ✅ **Token-aware context pruning** — estimates tokens (~4 chars/token), walks history backwards keeping messages within 12K token budget, replaces hard 40-message cap
- ✅ **`/export` command** — dumps current session as formatted markdown to Telegram, with role icons and chunked delivery
- ✅ **6 Telegram commands** — /help, /new, /status, /model, /sessions, /export

## v0.10.0 — Cron & Efficiency (shipped)

- ✅ **Cron job executor** — background task checks jobs.json every 30s, parses 5-field cron expressions and `every` schedules, fires agent turns, delivers results to Telegram, updates job state
- ✅ **Tool output truncation** — caps tool output at 32K chars before sending to LLM, preserves 75% head + 25% tail with truncation marker
- ✅ **Cron expression parser** — supports *, N, N-M, */N, N,M,... with timezone support (US timezones + common IANA)
- ✅ **77 tests** — 58 agent + 7 core + 12 gateway

## v0.11.0 — Vision, Cron Control & Caching (shipped)

- ✅ **Telegram photo/vision support** — receive photos, download largest size, base64 encode, send as multimodal content to vision-capable LLMs (OpenAI vision format)
- ✅ **Custom Message serialization** — `content` field serializes as array of content parts when images present, plain string otherwise
- ✅ **`/cron` command** — list all cron jobs with status/schedule/last-run, enable/disable by name (case-insensitive partial match)
- ✅ **System prompt caching** — in-memory cache with file mtime checking, avoids re-reading SOUL.md/TOOLS.md/etc every turn
- ✅ **7 Telegram commands** — /help, /new, /status, /model, /sessions, /export, /cron
- ✅ **77 tests** — 58 agent + 7 core + 12 gateway

## v0.12.0 — Agent Tools Expansion (shipped)

- ✅ **`process` tool** — background exec sessions with start/poll/kill/list actions, async-safe mutex handling, sandbox-enforced command blocklist
- ✅ **`image` tool** — standalone vision analysis via LLM, supports URLs and local files, auto-detects provider from env (OPENCLAW_VISION_*, OPENAI_*, OPENROUTER_*, etc.)
- ✅ **`cron` tool** — LLM-callable cron management: list/enable/disable/add/remove jobs, writes directly to jobs.json
- ✅ **Persistent typing indicator** — Telegram "typing..." stays active throughout entire agent turn (4s refresh via CancellationToken)
- ✅ **Streaming token usage** — `stream_options.include_usage` + fallback estimation when API returns 0
- ✅ **Markdown fix** — escape underscores in model names for Telegram stats footer
- ✅ **12 built-in tools** — exec, read, write, list_dir, patch, grep, find, web_search, web_fetch, process, image, cron
- ✅ **90 tests** — 71 agent + 7 core + 12 gateway

## v0.13.0 — Advanced Tools & Sessions (shipped)

- ✅ **`browser` tool** — headless Chromium browser: navigate (fetch page as text with HTML stripping), screenshot (PNG capture), evaluate (JS execution), auto-discovers chromium/chrome/brave
- ✅ **`tts` tool** — text-to-speech via Piper TTS subprocess, configurable model/speaker/speed, auto-generates WAV files, 30s timeout, AItan pronunciation hint
- ✅ **`sessions` tool** — LLM-callable session management: list (with stats, current marker), history (with timestamps, truncation), send (inject messages with role control), partial session key matching
- ✅ **15 built-in tools** — exec, read, write, list_dir, patch, grep, find, web_search, web_fetch, process, image, cron, sessions, tts, browser
- ✅ **110 tests** — 91 agent + 7 core + 12 gateway

## v0.14.0 — Multi-Channel Gateway (shipped)

- ✅ **Discord integration** — full Discord bot via WebSocket Gateway API: real-time message streaming, reply threading, typing indicators, auto-reconnect with exponential backoff, bot mention stripping
- ✅ **Discord commands** — /help, /new, /status, /model, /sessions (both `/` and `!` prefix)
- ✅ **Discord message handler** — parallel to Telegram handler with streaming edits, tool status indicators, stats footer, 2000-char chunking
- ✅ **Shared concurrency control** — rate limiter and semaphore shared across Telegram + Discord channels
- ✅ **Config expansion** — optional `discord` section in gateway config with bot_token and allowed_user_ids
- ✅ **115 tests** — 91 agent + 7 core + 17 gateway

## v0.15.0 — Daemon & Polish

- 📋 **Unix socket daemon mode** — long-running agent process
- 📋 **Slack integration**
- 📋 **Discord photo/vision support** — download attachments, send to vision LLM
- 📋 **Discord /export and /cron commands**
- 💡 **WhatsApp integration**

---

## Architecture Notes

```
openclaw-rs/
├── crates/
│   ├── openclaw-core/       # Config, paths, shared types
│   ├── openclaw-agent/      # LLM providers, tools, runtime, sessions
│   ├── openclaw-cli/        # Terminal interface with streaming
│   └── openclaw-gateway/    # Telegram + Discord bots, HTTP health, message handlers
├── Dockerfile
├── docker-compose.gateway.yml
└── ROADMAP.md               # This file
```

### Streaming Pipeline

```
LLM SSE stream
  → stream_completion() parses chunks
  → StreamEvent variants (ContentDelta, ToolExec, ToolResult, RoundStart, Done)
  → mpsc::unbounded_channel
  → Telegram handler accumulates text
  → editMessageText every 80 chars / 400ms
  → Final edit with stats footer
```

### Fallback Chain

```
ollama/llama3.2:1b → ollama/qwen2.5-coder:14b → moonshot/kimi-k2.5
  → deepseek-reasoner → deepseek-chat → anthropic/claude-opus-4-6
```

Circuit breaker: >3 consecutive failures = provider skipped.
Last successful model tracked in `RwLock<String>` for stats display.
