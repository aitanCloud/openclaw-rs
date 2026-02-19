# OpenClaw Rust Port — Roadmap

**Version:** 0.93.0
**Last updated:** 2026-02-19
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

## v0.15.0 — Discord Feature Parity (shipped)

- ✅ **Discord photo/vision support** — download image attachments, base64 encode, send to vision-capable LLMs, auto-detect MIME type, default "What's in this image?" prompt for image-only messages
- ✅ **Discord /export command** — export current session as formatted markdown with role icons, chunked delivery
- ✅ **Discord /cron command** — list all cron jobs with status/schedule/last-run, enable/disable by name (case-insensitive partial match)
- ✅ **Full command parity** — Discord now has all 7 commands matching Telegram: /help, /new, /status, /model, /sessions, /export, /cron
- ✅ **115 tests** — 91 agent + 7 core + 17 gateway

## v0.16.0 — Session Isolation & Voice (shipped)

- ✅ **Per-user session isolation** — session keys now include user ID: `tg:{agent}:{user}:{chat}` and `dc:{agent}:{user}:{channel}`, each user gets their own conversation history even in shared channels
- ✅ **Telegram voice messages** — `send_voice()` method with multipart OGG/Opus file upload, caption support
- ✅ **reqwest multipart** — added multipart feature for file uploads across all crates
- ✅ **115 tests** — 91 agent + 7 core + 17 gateway

## v0.17.0 — Voice Replies (shipped)

- ✅ **`/voice` command** — full pipeline: user text → LLM response → Piper TTS (WAV) → ffmpeg (OGG/Opus) → Telegram voice message, with AItan pronunciation fix, caption support, graceful fallbacks for missing piper/ffmpeg/model
- ✅ **8 Telegram commands** — /help, /new, /status, /model, /sessions, /export, /voice, /cron
- ✅ **115 tests** — 91 agent + 7 core + 17 gateway

## v0.18.0 — Full Voice Parity (shipped)

- ✅ **Discord /voice command** — same pipeline as Telegram: LLM response → Piper TTS → ffmpeg OGG/Opus → Discord file upload with caption
- ✅ **Discord send_file()** — multipart file upload method for Discord channels
- ✅ **8 commands on both channels** — Telegram and Discord now both have: /help, /new, /status, /model, /sessions, /export, /voice, /cron
- ✅ **115 tests** — 91 agent + 7 core + 17 gateway

## v0.19.0 — Status & Migration (shipped)

- ✅ **Enhanced /status endpoint** — rich JSON: version, uptime (human + seconds), agent config, session stats (total/telegram/discord/messages/tokens), channel info (enabled, allowed users), command lists, tool inventory
- ✅ **Session key migration** — auto-migrate old 3-part keys (`prefix:agent:channel`) to new 4-part format (`prefix:agent:0:channel`) on gateway startup, with FK-safe SQLite updates
- ✅ **116 tests** — 92 agent + 7 core + 17 gateway

## v0.20.0 — Metrics & Docker (shipped)

- ✅ **Gateway metrics** — atomic counters for telegram/discord requests, errors, rate limits, concurrency rejections, latency tracking with avg calculation, `/metrics` JSON endpoint
- ✅ **Metrics wired into event loops** — both Telegram and Discord loops record requests, errors, and per-request latency
- ✅ **Status endpoint enhanced** — `/status` now includes live metrics alongside session stats
- ✅ **Dockerfile updated** — builds both `openclaw` and `openclaw-gateway` binaries, includes ffmpeg for voice, cleaned up redundant installs
- ✅ **119 tests** — 92 agent + 7 core + 20 gateway

## v0.21.0 — Resilience & Observability (shipped)

- ✅ **Discord auto-reconnect improved** — always reconnect on clean close (Discord maintenance), exponential backoff with reset after healthy sessions (>30s clean, >60s error), consecutive failure tracking, stop on channel close
- ✅ **Prometheus /metrics endpoint** — text/plain exposition format with HELP/TYPE annotations for all counters and gauges, compatible with Prometheus/Grafana scrapers
- ✅ **/metrics/json endpoint** — JSON format metrics for dashboard consumption
- ✅ **120 tests** — 92 agent + 7 core + 21 gateway

## v0.22.0 — Voice Input & Commands (shipped)

- ✅ **Incoming voice message support** — detect Telegram voice/audio messages, download OGG, convert to 16kHz WAV via ffmpeg, transcribe via whisper-cpp or Python whisper, process transcription as normal text input
- ✅ **`/ping` command** — lightweight latency check on both Telegram and Discord
- ✅ **TgVoice + TgAudio types** — Telegram API types for voice and audio message handling
- ✅ **9 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /cron
- ✅ **120 tests** — 92 agent + 7 core + 21 gateway

## v0.23.0 — Debounce & Cleanup (shipped)

- ✅ **Message debouncer** — `debounce.rs` module collects rapid messages from same user into single batch (1.5s window, max 5 messages), with 4 async tests
- ✅ **Session auto-cleanup** — `prune_old_sessions()` deletes sessions older than 30 days on gateway startup, with FK-safe SQLite deletes
- ✅ **Session maintenance on startup** — migration + pruning run together before cron/polling starts
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.24.0 — Resume & Introspection (shipped)

- ✅ **Discord Gateway Resume** — store `session_id` + `sequence` across reconnects, send Resume (op 6) instead of re-Identify, use `resume_gateway_url` from READY, handle RESUMED event, clear state on non-resumable Invalid Session
- ✅ **`/db` command** — show SQLite session database stats (session count, message count, tokens, DB size, oldest/newest) on both Telegram and Discord
- ✅ **`DbStats` struct** — new public API in `SessionStore` for database introspection
- ✅ **10 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /db, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.25.0 — Reliability & History (shipped)

- ✅ **Graceful shutdown enhanced** — handle SIGTERM (systemd) alongside SIGINT, drain active tasks up to 30s timeout
- ✅ **`/history` command** — show last 5 messages from current session on both Telegram and Discord, with role icons and content preview
- ✅ **Heartbeat timeout detection** — detect missed Discord Gateway ACKs after 45s, force reconnect with resume state preserved
- ✅ **`find_latest_session()`** — new SessionStore method for prefix-based session lookup (supports UUID-suffixed keys)
- ✅ **11 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /history, /db, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.26.0 — Session Management (shipped)

- ✅ **`/clear` command** — delete current session and all its messages from SQLite, on both Telegram and Discord
- ✅ **`delete_session()`** — new SessionStore method for FK-safe session deletion
- ✅ **12 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /history, /clear, /db, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.27.0 — Observability & Safety (shipped)

- ✅ **Startup banner** — log version box, config summary, command count, DB stats (sessions, messages, size) on boot
- ✅ **`/version` command** — show build version, uptime, agent name on both Telegram and Discord
- ✅ **WebSocket error classification** — fatal errors (4004 auth invalid, 4010–4014) stop reconnecting; transient closes save resume state
- ✅ **`BOOT_TIME` static** — `LazyLock<Instant>` for uptime tracking across commands
- ✅ **13 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /history, /clear, /db, /version, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.28.0 — Live Metrics (shipped)

- ✅ **`/stats` command** — show gateway metrics (Telegram/Discord requests, errors, rate limits, avg latency, uptime) on both channels
- ✅ **Global metrics accessor** — `OnceLock`-based `metrics::global()` for handler access without threading `Arc` through signatures
- ✅ **14 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /history, /clear, /db, /version, /stats, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.29.0 — Rich UI & Identity (shipped)

- ✅ **Discord embed support** — `send_embed()` method with colored sidebar and fields; /stats (blurple), /version (Rust orange), /whoami (green) use rich embeds
- ✅ **`/whoami` command** — show user ID, username, session key, authorization status on both channels (embed on Discord)
- ✅ **15 commands on both channels** — /help, /new, /status, /model, /sessions, /export, /voice, /ping, /history, /clear, /db, /version, /stats, /whoami, /cron
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.30.0 — Polish & UX (shipped)

- ✅ **Discord embed for `/db`** — rich embed with orange sidebar and 6 inline fields (sessions, messages, tokens, size, oldest, newest)
- ✅ **Configurable `/history N`** — show last N messages (default 5, max 20) on both Telegram and Discord
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.31.0 — Resilience & Logging (shipped)

- ✅ **Message retry with backoff** — Discord `send_reply` retries once on 5xx or network error with 1s delay before giving up
- ✅ **Gateway connection logging** — log connect events with RESUME/IDENTIFY status, session duration on disconnect
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.32.0 — Retry & Gateway Metrics (shipped)

- ✅ **Telegram send_message retry** — retry once on 5xx or network error with 1s delay (parity with Discord send_reply)
- ✅ **Gateway WS metrics** — `gateway_connects`, `gateway_disconnects`, `gateway_resumes` counters in Prometheus text + JSON
- ✅ **125 tests** — 93 agent + 7 core + 25 gateway

## v0.33.0 — Test Coverage (shipped)

- ✅ **`test_delete_session`** — verify session + messages removed, other sessions unaffected, non-existent returns 0
- ✅ **`test_find_latest_session`** — verify prefix-based lookup returns newest match, exact key match, non-match returns None
- ✅ **`test_gateway_ws_metrics`** — verify connect/disconnect/resume counters in both Prometheus and JSON output
- ✅ **128 tests** — 95 agent + 7 core + 26 gateway (+3 new)

## v0.34.0 — Task Cancellation (shipped)

- ✅ **Task registry** — global `CancellationToken` per chat with auto-cancel when new message arrives (prevents ghost typing)
- ✅ **`/cancel` and `/stop` commands** — kill the running agent task for the current chat on both Telegram and Discord
- ✅ **Cancellation wired into agent runtime** — checked between rounds and during LLM streaming via `tokio::select!`
- ✅ **Auto-cancel on new message** — if a task is already running for a chat, it is cancelled before starting a new one
- ✅ **120s timeout guard** — agent turns auto-abort after 120s even without user cancellation
- ✅ **17 commands on both channels** — added /cancel and /stop (aliases)
- ✅ **133 tests** — 95 agent + 7 core + 31 gateway (+5 new task_registry tests)

## v0.35.0 — Cancellation Polish (shipped)

- ✅ **Discord `/cancel` embed** — red embed with active task count on successful cancellation
- ✅ **Active tasks in `/status`** — show running task count on both Telegram and Discord
- ✅ **Cancellation metrics** — `tasks_cancelled` counter in Prometheus text + JSON, recorded on every /cancel or /stop
- ✅ **133 tests** — 95 agent + 7 core + 31 gateway

## v0.36.0 — Rich Embeds & Metrics (shipped)

- ✅ **Discord `/status` embed** — green embed with 6 inline fields (model, fallback, active tasks, messages, tokens, sessions)
- ✅ **Cancelled count in `/stats`** — `tasks_cancelled` shown in Discord /stats embed alongside other metrics
- ✅ **133 tests** — 95 agent + 7 core + 31 gateway

## v0.37.0 — HTTP & Test Coverage (shipped)

- ✅ **`/health` JSON upgrade** — returns `{"status":"ok","active_tasks":N}` instead of plain text
- ✅ **`/status` active tasks** — `active_tasks` field added to HTTP status JSON
- ✅ **Fixed stale command lists** — `/status` HTTP endpoint now lists all 17 commands (was 8)
- ✅ **`test_tasks_cancelled_metric`** — verifies `tasks_cancelled` counter in Prometheus + JSON output
- ✅ **134 tests** — 95 agent + 7 core + 32 gateway (+1 new)

## v0.38.0 — Stats Parity (shipped)

- ✅ **Telegram `/stats` parity** — added cancelled count and active tasks to Telegram /stats (was Discord-only)
- ✅ **Discord `/stats` active tasks** — added Active Tasks field to Discord /stats embed
- ✅ **Full Telegram↔Discord parity** — both channels now show identical stats: requests, errors, rate limited, completed, cancelled, active tasks, avg latency
- ✅ **134 tests** — 95 agent + 7 core + 32 gateway

## v0.39.0 — Discord Embeds & Cancellation Test (shipped)

- ✅ **Discord `/clear` embed** — green embed with deleted message count and session key
- ✅ **Discord `/sessions` embed** — blurple embed with session list, total/messages/tokens summary fields
- ✅ **`test_cancellation_aborts_streaming_turn`** — integration test using SlowMockProvider: verifies CancellationToken aborts agent turn within 50ms instead of waiting 10s
- ✅ **135 tests** — 96 agent + 7 core + 32 gateway (+1 new)

## v0.40.0 — Model Embeds & Timeout Metrics (shipped)

- ✅ **Discord `/model` embed** — orange embed with fallback chain description and provider/mode/circuit-breaker fields
- ✅ **`agent_timeouts` metric** — tracks 120s timeout hits separately from user cancellations, in Prometheus + JSON
- ✅ **Timeout recording** — wired into both Telegram and Discord handlers on 120s timeout
- ✅ **`test_agent_timeouts_metric`** — verifies counter in Prometheus + JSON output
- ✅ **136 tests** — 96 agent + 7 core + 33 gateway (+1 new)

## v0.41.0 — Config Tests & New Session Embed (shipped)

- ✅ **Config parsing tests** — 3 new tests: minimal config, full config with Discord, sandbox config (JSON deserialization + defaults)
- ✅ **Discord `/new` embed** — green embed with previous message count and agent name
- ✅ **139 tests** — 96 agent + 7 core + 36 gateway (+3 new config tests)

## v0.42.0 — Help Embed & Health Uptime (shipped)

- ✅ **Discord `/help` embed** — blurple embed with categorized command groups (Session, Info, Monitoring, Control)
- ✅ **`/health` uptime** — added `uptime_seconds` to /health JSON endpoint
- ✅ **Discord `/stats` timeouts** — added Timeouts field to /stats embed for full parity with Telegram
- ✅ **139 tests** — 96 agent + 7 core + 36 gateway

## v0.43.0 — Stats & Health Polish (shipped)

- ✅ **Telegram `/stats` timeouts** — added Timeouts line for full parity with Discord /stats embed
- ✅ **`/health` version** — added `version` field to /health JSON endpoint
- ✅ **139 tests** — 96 agent + 7 core + 36 gateway

## v0.44.0 — Error Rate & Prometheus Tests (shipped)

- ✅ **Error rate %** — `error_rate_pct()` method on GatewayMetrics, shown in /stats on both Telegram and Discord
- ✅ **`test_error_rate_pct`** — verifies error rate calculation (0 requests = 0%, 10 req / 2 err = 20%)
- ✅ **`test_prometheus_format_headers`** — verifies all HELP/TYPE headers present in Prometheus output
- ✅ **141 tests** — 96 agent + 7 core + 38 gateway (+2 new)

## v0.45.0 — Error Rate Observability (shipped)

- ✅ **`error_rate_pct` in JSON** — added to `to_json()` output, automatically included in /status and /metrics/json HTTP endpoints
- ✅ **`error_rate_pct` Prometheus gauge** — `openclaw_gateway_error_rate_pct` gauge in /metrics output
- ✅ **`test_error_rate_in_json_and_prometheus`** — verifies 25% error rate appears correctly in both formats
- ✅ **142 tests** — 96 agent + 7 core + 39 gateway (+1 new)

## v0.46.0 — Streaming Tests & Ping Embed (shipped)

- ✅ **Streaming SSE tests** — 6 new tests: StreamChunk content/reasoning/tool_call/usage deserialization, PartialToolCall default, StreamEvent variant coverage
- ✅ **Discord `/ping` embed** — color-coded embed: green <100ms, yellow <500ms, red ≥500ms
- ✅ **148 tests** — 102 agent + 7 core + 39 gateway (+6 new streaming tests)

## v0.47.0 — Subagent System (shipped)

- ✅ **`subagent.rs` module** — `run_subagent_turn()` spawns isolated agent turns with fresh message history, same LLM provider, minimal context
- ✅ **`delegate` tool** — 16th built-in tool; allows agent to spawn a subagent for focused subtasks (code review, summarization, research)
- ✅ **Recursion prevention** — `ToolRegistry::without_tool()` strips `delegate` from subagent tool set, preventing infinite delegation loops
- ✅ **Provider reuse** — subagent uses same fallback chain as parent, falls back to env vars if config unavailable
- ✅ **3 new tests** — `test_delegate_tool_definition`, `test_subagent_session_key_format`, `test_without_tool_removes_delegate`
- ✅ **151 tests** — 105 agent + 7 core + 39 gateway (+3 new)
- ✅ **16 tools** — exec, read, write, list_dir, patch, grep, find, web_search, web_fetch, process, image, cron, sessions, tts, browser, **delegate**

## v0.48.0 — Memory Tool & Circuit Breaker Tests (shipped)

- ✅ **`memory` tool** — 17th built-in tool; persistent key-value notes per agent in JSON file (set/get/list/delete)
- ✅ **Subagent cancellation** — `run_subagent_turn()` now accepts optional CancellationToken, propagated from parent
- ✅ **Circuit breaker test** — `test_circuit_breaker_threshold` verifies >3 failures opens circuit, reset on success
- ✅ **Last successful tracking test** — `test_last_successful_tracking` verifies initial provider is tracked
- ✅ **Memory tool tests** — `test_memory_tool_definition`, `test_memory_set_get_list_delete` (full CRUD), `test_load_memory_missing_file`
- ✅ **156 tests** — 110 agent + 7 core + 39 gateway (+5 new)
- ✅ **17 tools** — added `memory` (persistent notes)

## v0.49.0 — Agent Turn & Tool Call Metrics (shipped)

- ✅ **`agent_turns` counter** — tracks total agent turns completed across both channels
- ✅ **`tool_calls` counter** — tracks total tool calls made by agents (accumulated per turn)
- ✅ **`record_agent_turn(tool_calls)`** — single method increments both counters, wired into Telegram and Discord handlers
- ✅ **Prometheus + JSON** — `openclaw_gateway_agent_turns_total` and `openclaw_gateway_tool_calls_total` counters
- ✅ **`/stats` parity** — Agent Turns and Tool Calls shown on both Telegram and Discord /stats
- ✅ **`test_agent_turns_and_tool_calls_metric`** — verifies counters in Prometheus + JSON output
- ✅ **157 tests** — 110 agent + 7 core + 40 gateway (+1 new)

## v0.50.0 — Skills Integration (shipped)

- ✅ **Skills in system prompt** — workspace `skills/` directory is scanned at load time; skill names and descriptions are injected into the agent's system prompt as `<!-- SKILLS.md -->`
- ✅ **Skills count in `/health`** — `/health` JSON endpoint now includes `skills` count for monitoring
- ✅ **`test_skills_injected_into_system_prompt`** — verifies skills are scanned and appear in the assembled system prompt
- ✅ **158 tests** — 111 agent + 7 core + 40 gateway (+1 new)
- ✅ **Skills scanner** — leverages existing `openclaw_core::skills::list_skills()` with SKILL.md frontmatter parsing

## v0.51.0 — Doctor Command (shipped)

- ✅ **`/doctor` command** — runs 8 health checks: workspace, config, sessions DB, skills, LLM provider, metrics, active tasks, uptime
- ✅ **Discord `/doctor` embed** — green (all clear) or red (issues found) with per-check ✅/❌ status fields
- ✅ **Telegram `/doctor`** — text-based report with status icons
- ✅ **`doctor.rs` module** — `run_checks()` async function returns `Vec<(name, passed, detail)>`
- ✅ **2 new tests** — `test_doctor_returns_checks` (verifies 8 check names), `test_doctor_skills_always_ok`
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway (+2 new)
- ✅ **13/17 Discord embeds** — added `/doctor`

## v0.52.0 — Doctor HTTP & More Embeds (shipped)

- ✅ **`/doctor` HTTP endpoint** — JSON health check endpoint at `:3100/doctor` returning `{status, passed, total, checks[]}` for monitoring/safe-restart integration
- ✅ **Discord `/history` embed** — role-labeled fields (👤 User / 🤖 Assistant / 🔧 Tool) with message count and previews
- ✅ **Discord `/cron` embed** — orange embed with per-job fields showing schedule, last run, status, duration
- ✅ **15/17 Discord embeds** — added `/history` and `/cron`
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway
- ✅ **HTTP endpoints** — /health, /status, /metrics, /metrics/json, **/doctor**

## v0.53.0 — Graceful Shutdown & Session Stats (shipped)

- ✅ **SIGTERM handler** — replaced no-op `pending()` with real `tokio::signal::unix::SignalKind::terminate()` listener for proper systemd shutdown
- ✅ **Graceful drain** — on SIGINT/SIGTERM, waits up to 30s for active tasks to complete before exiting
- ✅ **Session count in `/stats`** — both Telegram and Discord /stats now show total session count from SQLite DB
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway

## v0.54.0 — /tools Command & 19 Commands (shipped)

- ✅ **`/tools` command** — lists all 17 built-in agent tools on both Telegram (bullet list) and Discord (embed with dot-separated tool names)
- ✅ **`/help` updated** — added `/tools` and `/doctor` to help text on both channels
- ✅ **19 commands** — added `/tools` and `/doctor` (previously unlisted), updated startup banner, /version, /help
- ✅ **16/19 Discord embeds** — added `/tools` embed
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway

## v0.55.0 — /skills Command & 20 Commands (shipped)

- ✅ **`/skills` command** — lists available workspace skills with descriptions on both Telegram (bullet list) and Discord (purple embed with per-skill fields)
- ✅ **20 commands** — added `/skills`, updated startup banner, /version, /help on both channels
- ✅ **18/20 Discord embeds** — added `/skills` embed (remaining: /export file upload, /voice TTS audio)
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway

## v0.56.0 — Provider Visibility (shipped)

- ✅ **Model info in `/doctor`** — LLM Provider check now shows configured model count and labels (e.g. `3 model(s): ollama/qwen2.5:14b, moonshot/kimi-k2.5, ...`)
- ✅ **`providers` in `/status` JSON** — `/status` endpoint now includes `providers` array with all fallback chain model labels
- ✅ **Command lists updated** — `/status` JSON now lists all 20 commands including `/tools`, `/skills`, `/doctor`
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway

## v0.57.0 — Webhook Endpoint (shipped)

- ✅ **`POST /webhook`** — HTTP endpoint for external agent turn triggers with bearer token auth
- ✅ **Webhook auth** — `WebhookConfig { token }` in gateway config, validates `Authorization: Bearer <token>` header
- ✅ **Full agent turn** — webhook runs complete agent turn with tools, returns JSON `{reply, session_key, model, tool_calls, rounds, elapsed_ms}`
- ✅ **`webhook_requests` metric** — new counter in Prometheus and JSON metrics output
- ✅ **6 HTTP endpoints** — /health, /status, /metrics, /metrics/json, /doctor, **/webhook**
- ✅ **160 tests** — 111 agent + 7 core + 42 gateway

## v0.58.0 — Webhook Stats & Test Coverage (shipped)

- ✅ **Webhook stats in `/stats`** — webhook_requests count shown on both Telegram and Discord /stats
- ✅ **Webhook config tests** — 2 new tests: parse webhook config, verify webhook is optional
- ✅ **Webhook metric test** — verifies webhook_requests counter in both Prometheus and JSON output
- ✅ **163 tests** — 111 agent + 7 core + 45 gateway (was 42)

## v0.59.0 — /config Command & Enhanced Doctor (shipped)

- ✅ **`/config` command** — shows sanitized gateway config (no tokens) on both Telegram (text) and Discord (gray embed with fields)
- ✅ **Doctor: 11 checks** — added Cron Jobs (count + enabled), Disk Usage (workspace + sessions DB with human-readable sizes), Webhook status
- ✅ **21 commands** — added `/config`, updated startup banner, /version, /help on both channels
- ✅ **19/21 Discord embeds** — added `/config` embed (remaining: /export file upload, /voice TTS audio)
- ✅ **163 tests** — 111 agent + 7 core + 45 gateway

## v0.60.0 — Polish & Consistency (shipped)

- ✅ **`/status` JSON fix** — command lists now include all 21 commands (was missing `/config`)
- ✅ **`human_uptime()` formatter** — consistent uptime display with days support (e.g. `2d 5h 30m`) used in /health endpoint
- ✅ **Enhanced `/health`** — now includes `uptime` (human-readable), `commands: 21` fields
- ✅ **163 tests** — 111 agent + 7 core + 45 gateway

## v0.61.0 — /runtime Command & Webhook Tracing (shipped)

- ✅ **`/runtime` command** — shows build profile, PID, uptime, OS, arch on both Telegram (text) and Discord (green embed)
- ✅ **Webhook `request_id`** — every successful webhook response now includes a UUID `request_id` for tracing
- ✅ **22 commands** — added `/runtime`, updated startup banner, /version, /help on both channels
- ✅ **20/22 Discord embeds** — added `/runtime` embed (remaining: /export file upload, /voice TTS audio)
- ✅ **163 tests** — 111 agent + 7 core + 45 gateway

## v0.62.0 — Consistent Uptime & Test Coverage (shipped)

- ✅ **Consistent `human_uptime()`** — all uptime displays (/version, /stats, /health, /runtime) now use the same formatter with days support
- ✅ **`/status` + `/health` fix** — command lists and count updated to 22 (was missing `/runtime`)
- ✅ **5 new uptime tests** — seconds, minutes, hours, days, multi-days edge cases
- ✅ **168 tests** — 111 agent + 7 core + 50 gateway (was 45)

## v0.63.0 — Build Timestamp & Test Coverage (shipped)

- ✅ **Compile-time build timestamp** — `BUILD_TIMESTAMP` set via `build.rs`, shown in `/runtime` (both channels), `/health`, and `/status` JSON
- ✅ **`webhook_configured`** — `/status` JSON now includes `webhook_configured: true/false`
- ✅ **5 new doctor tests** — `human_bytes` (bytes, KB, MB, GB) and `dir_size_bytes` (nonexistent path)
- ✅ **173 tests** — 111 agent + 7 core + 55 gateway (was 50)

## v0.64.0 — Enhanced /db & /health (shipped)

- ✅ **Enhanced `/db`** — added avg messages per session on both Telegram and Discord
- ✅ **`sessions` in `/health`** — /health endpoint now includes session_count for monitoring
- ✅ **173 tests** — 111 agent + 7 core + 55 gateway

## v0.65.0 — Process Memory Monitoring (shipped)

- ✅ **`process_rss_bytes()`** — reads RSS from `/proc/self/statm` (Linux), human-readable via `human_bytes_pub()`
- ✅ **Memory in `/runtime`** — shows process RSS on both Telegram (text) and Discord (embed)
- ✅ **Memory in `/health`** — `memory_rss_bytes` and `memory_rss` fields for monitoring/alerting
- ✅ **173 tests** — 111 agent + 7 core + 55 gateway

## v0.66.0 — RSS Prometheus Gauge & Metrics Tests (shipped)

- ✅ **`process_rss_bytes` Prometheus gauge** — RSS exposed as `openclaw_gateway_process_rss_bytes` for Grafana/alerting
- ✅ **RSS in JSON metrics** — `process_rss_bytes` field in `/metrics/json` output
- ✅ **2 new metrics tests** — verify RSS presence in both Prometheus and JSON output
- ✅ **175 tests** — 111 agent + 7 core + 57 gateway (was 55)

## v0.67.0 — Prometheus Gauges & Memory Doctor (shipped)

- ✅ **`uptime_seconds` Prometheus gauge** — `openclaw_gateway_uptime_seconds` for dashboard uptime tracking
- ✅ **`sessions_total` Prometheus gauge** — `openclaw_gateway_sessions_total` for session count monitoring
- ✅ **Memory doctor check** — 12th check: shows RSS, warns (fails) if RSS > 512 MB
- ✅ **Uptime uses `human_uptime()`** — doctor uptime check now uses consistent formatter with days support
- ✅ **175 tests** — 111 agent + 7 core + 57 gateway

## v0.68.0 — Metrics Parity & Comprehensive Tests (shipped)

- ✅ **JSON metrics parity** — `uptime_seconds` and `sessions_total` now in JSON metrics (was only in Prometheus)
- ✅ **Comprehensive Prometheus test** — verifies all 20 metric lines are present in output
- ✅ **JSON uptime/sessions test** — verifies new fields in JSON metrics
- ✅ **20 Prometheus metrics** — requests, errors, rate_limited, concurrency_rejected, completed, latency, avg_latency, ws_events (3), cancelled, timeouts, error_rate, turns, tool_calls, webhooks, rss, uptime, sessions
- ✅ **177 tests** — 111 agent + 7 core + 59 gateway (was 57)

## v0.69.0 — Readiness Probe & JSON Parity (shipped)

- ✅ **`GET /ready` endpoint** — Kubernetes-style readiness probe: returns 200 if all 12 doctor checks pass, 503 with failed check names if any fail
- ✅ **JSON `latency_ms_total`** — renamed from `total_latency_ms` for consistency with Prometheus naming
- ✅ **7 HTTP endpoints** — /health, /ready, /status, /metrics, /metrics/json, /doctor, /webhook
- ✅ **177 tests** — 111 agent + 7 core + 59 gateway

## v0.70.0 — Webhook Error Codes & Test Coverage (shipped)

- ✅ **Webhook `error_code` fields** — all 5 webhook error responses now include UPPER_SNAKE_CASE `error_code` for programmatic handling: `WEBHOOK_NOT_CONFIGURED`, `INVALID_TOKEN`, `MISSING_MESSAGE`, `PROVIDER_INIT_FAILED`, `AGENT_TURN_FAILED`
- ✅ **`process_rss_bytes` test** — verifies RSS > 0 on Linux
- ✅ **Webhook error codes test** — validates all 5 error codes are UPPER_SNAKE_CASE
- ✅ **179 tests** — 111 agent + 7 core + 61 gateway (was 59)

## v0.71.0 — Webhook Tracing & /status Endpoints (shipped)

- ✅ **`request_id` on ALL webhook responses** — generated at handler entry, included in all 5 error responses + success for consistent tracing
- ✅ **`http_endpoints` in `/status` JSON** — lists all 7 endpoints with count
- ✅ **Boundary value tests** — `human_bytes` tested at exact KB/MB/GB thresholds
- ✅ **180 tests** — 111 agent + 7 core + 62 gateway (was 61)

## v0.72.0 — Doctor Checks in /health & Prometheus (shipped)

- ✅ **Doctor summary in `/health`** — `doctor_checks_total` and `doctor_checks_passed` fields for quick health assessment
- ✅ **`doctor_checks_total` Prometheus gauge** — 21st metric: static count of doctor checks for alerting
- ✅ **21 Prometheus metrics** — comprehensive test updated to verify all 21
- ✅ **180 tests** — 111 agent + 7 core + 62 gateway

## v0.73.0 — Boot Timestamp & JSON Metrics Parity (shipped)

- ✅ **`boot_time` ISO 8601** — `BOOT_TIMESTAMP` LazyLock stores gateway start time, shown in `/health` and `/status` JSON
- ✅ **`doctor_checks_total` in JSON metrics** — parity with Prometheus gauge
- ✅ **180 tests** — 111 agent + 7 core + 62 gateway

## v0.74.0 — Boot Time in /runtime & Comprehensive Tests (shipped)

- ✅ **`Started` in `/runtime`** — boot_time ISO 8601 shown on both Telegram (text) and Discord (embed)
- ✅ **JSON metrics completeness test** — verifies all 22 JSON metrics fields are present
- ✅ **BOOT_TIMESTAMP format test** — validates ISO 8601 format (YYYY-MM-DDTHH:MM:SSZ)
- ✅ **181 tests** — 111 agent + 7 core + 63 gateway (was 62)

## v0.75.0 — Info Gauge & Response Timing (shipped)

- ✅ **`openclaw_gateway_info` Prometheus gauge** — 22nd metric: `{version="0.75.0"} 1` for Grafana version tracking
- ✅ **`X-Response-Time-Ms` header** — added to `/health` and `/ready` responses for latency monitoring
- ✅ **`human_uptime` edge case tests** — boundary values: 0s, 60s, 3600s, 86400s
- ✅ **22 Prometheus metrics** — comprehensive test updated
- ✅ **182 tests** — 111 agent + 7 core + 64 gateway (was 63)

## v0.76.0 — Response Timing & Command Validation (shipped)

- ✅ **`response_time_ms` in JSON bodies** — `/health` and `/ready` now include self-measured response time in both header and body
- ✅ **Command count validation test** — verifies both channels have exactly 22 commands and lists match
- ✅ **`human_bytes(0)` test** — boundary value for zero bytes
- ✅ **183 tests** — 111 agent + 7 core + 65 gateway (was 64)

## v0.77.0 — /version Endpoint & Error Rate (shipped)

- ✅ **`GET /version` endpoint** — lightweight version check: just version, built, boot_time (no doctor checks)
- ✅ **`error_rate_pct` in `/health`** — current error rate percentage for monitoring
- ✅ **8 HTTP endpoints** — /health, /version, /ready, /status, /metrics, /metrics/json, /doctor, /webhook
- ✅ **HTTP endpoint count test** — verifies 8 endpoints with no duplicates
- ✅ **184 tests** — 111 agent + 7 core + 66 gateway (was 65)

## v0.78.0 — /ping Endpoint & Error Rate Test (shipped)

- ✅ **`GET /ping` endpoint** — minimal plaintext "pong" response for load balancer health checks
- ✅ **`total_requests` in `/health`** — combined telegram + discord request count
- ✅ **`total_requests()` method** — refactored `error_rate_pct()` to use it
- ✅ **Error rate calculation test** — verifies 0% with no requests, 0% with no errors, 20% with 2/10 errors
- ✅ **9 HTTP endpoints** — /health, /version, /ping, /ready, /status, /metrics, /metrics/json, /doctor, /webhook
- ✅ **185 tests** — 111 agent + 7 core + 67 gateway (was 66)

## v0.79.0 — Error Totals & Latency Test (shipped)

- ✅ **`total_errors()` method** — combined telegram + discord error count, refactored `error_rate_pct()` to use it
- ✅ **`total_errors` in `/health`** — shows combined error count alongside error_rate_pct
- ✅ **`avg_latency_ms` calculation test** — verifies 0 with no completions, 200ms avg with 100+200+300
- ✅ **186 tests** — 111 agent + 7 core + 68 gateway (was 67)

## v0.80.0 — /health Enrichment & Field Validation (shipped)

- ✅ **`avg_latency_ms` and `webhook_requests` in `/health`** — 20 total fields for comprehensive health overview
- ✅ **`webhook_requests()` accessor** — new public method on GatewayMetrics
- ✅ **`/health` field completeness test** — verifies all 20 fields with no duplicates
- ✅ **`human_uptime` multi-day test** — 2d 3h 45m and 7d 0h 0m
- ✅ **188 tests** — 111 agent + 7 core + 70 gateway (was 68)

## v0.81.0 — Provider Info & 13th Doctor Check (shipped)

- ✅ **`provider_count` and `fallback_chain` in `/health`** — 22 total fields, shows configured LLM providers
- ✅ **13th doctor check: HTTP** — reports 9 endpoints and configured port
- ✅ **UUID format validation test** — verifies request_id is valid UUID (36 chars, 4 dashes, parseable)
- ✅ **13 doctor checks** — workspace, config, sessions, skills, LLM, metrics, cron, disk, webhook, memory, tasks, HTTP, uptime
- ✅ **189 tests** — 111 agent + 7 core + 71 gateway (was 70)

## v0.82.0 — /health Counts & Doctor Check Validation (shipped)

- ✅ **`http_endpoint_count` and `tool_count` in `/health`** — 24 total fields for complete system overview
- ✅ **Doctor check names completeness test** — verifies all 13 check names and exact count
- ✅ **`human_bytes(1)` test** — single byte boundary value
- ✅ **189 tests** — 111 agent + 7 core + 71 gateway

## v0.83.0 — Agent Name & /status Validation (shipped)

- ✅ **`agent` field in `/health` and `/version`** — shows configured agent name via `OnceLock` static
- ✅ **`init_agent_name()` / `agent_name()`** — set once at startup, accessible from all HTTP handlers
- ✅ **`/status` field completeness test** — verifies all 20 fields with no duplicates
- ✅ **25 `/health` fields** — comprehensive system overview
- ✅ **190 tests** — 111 agent + 7 core + 72 gateway (was 71)

## v0.84.0 — /metrics/summary & Agent Activity (shipped)

- ✅ **`GET /metrics/summary`** — 10th endpoint: human-readable one-liner of key metrics
- ✅ **`agent_turns` and `tool_calls` in `/health`** — 27 total fields for complete system overview
- ✅ **`agent_turns()` and `tool_calls()` accessors** — new public methods on GatewayMetrics
- ✅ **`/version` field completeness test** — verifies all 4 fields with no duplicates
- ✅ **10 HTTP endpoints** — /health, /version, /ping, /ready, /status, /metrics, /metrics/json, /metrics/summary, /doctor, /webhook
- ✅ **191 tests** — 111 agent + 7 core + 73 gateway (was 72)

## v0.85.0 — Completed Requests & Endpoint Field Tests (shipped)

- ✅ **`completed_requests` in `/health`** — 28 total fields, shows successful agent completions
- ✅ **`completed_requests()` accessor** — new public method on GatewayMetrics
- ✅ **`/ready` field completeness test** — verifies all 5 fields with no duplicates
- ✅ **`/metrics/summary` format test** — verifies all 8 key=value pairs present
- ✅ **193 tests** — 111 agent + 7 core + 75 gateway (was 73)

## v0.86.0 — Prometheus Completions & Rate Limiting (shipped)

- ✅ **`openclaw_gateway_completed_requests_total` Prometheus metric** — 23rd metric: completed agent requests counter
- ✅ **`rate_limited` and `concurrency_rejected` in `/health`** — 30 total fields for complete system overview
- ✅ **`rate_limited()` and `concurrency_rejected()` accessors** — new public methods on GatewayMetrics
- ✅ **`record_agent_turn` test** — verifies turns and tool_calls accumulate correctly
- ✅ **194 tests** — 111 agent + 7 core + 76 gateway (was 75)

## v0.87.0 — Timeouts & Cancellations in /health (shipped)

- ✅ **`agent_timeouts` and `tasks_cancelled` in `/health`** — 32 total fields for complete system overview
- ✅ **`agent_timeouts()` and `tasks_cancelled()` accessors** — new public methods on GatewayMetrics
- ✅ **`record_completion` test** — verifies completed_requests count and avg_latency calculation
- ✅ **195 tests** — 111 agent + 7 core + 77 gateway (was 76)

## v0.88.0 — /health/lite & WebSocket Stats (shipped)

- ✅ **`GET /health/lite`** — 11th endpoint: lightweight health check without doctor checks (fast response)
- ✅ **`gateway_connects`, `gateway_disconnects`, `gateway_resumes` in `/health`** — 35 total fields
- ✅ **`gateway_connects()`, `gateway_disconnects()`, `gateway_resumes()` accessors** — new public methods on GatewayMetrics
- ✅ **`/health/lite` field completeness test** — verifies all 6 fields with no duplicates
- ✅ **11 HTTP endpoints** — /health, /health/lite, /version, /ping, /ready, /status, /metrics, /metrics/json, /metrics/summary, /doctor, /webhook
- ✅ **196 tests** — 111 agent + 7 core + 78 gateway (was 77)

## v0.89.0 — /doctor/json & Uptime Tests (shipped)

- ✅ **`GET /doctor/json`** — 12th endpoint: structured JSON array of all 13 doctor checks with name/ok/detail
- ✅ **`human_uptime` exact hour test** — 1h 0m and 2h 0m boundary values
- ✅ **`/status` commands structure test** — verifies telegram and discord sub-keys
- ✅ **12 HTTP endpoints** — /health, /health/lite, /version, /ping, /ready, /status, /metrics, /metrics/json, /metrics/summary, /doctor, /doctor/json, /webhook
- ✅ **198 tests** — 111 agent + 7 core + 80 gateway (was 78)

## v0.90.0 — Test Quality Milestone (shipped)

- ✅ **`/doctor/json` field completeness test** — verifies all 4 top-level fields with no duplicates
- ✅ **Doctor check item field test** — verifies each check has name/ok/detail (3 fields)
- ✅ **Prometheus HELP/TYPE line count test** — verifies matching HELP and TYPE lines (≥15 each)
- ✅ **201 tests** — 111 agent + 7 core + 83 gateway (was 80) — **200+ test milestone!**

## v0.91.0 — Disk Usage & 14th Doctor Check (shipped)

- ✅ **`disk_usage_bytes` and `disk_usage` in `/health`** — 37 total fields, shows workspace disk usage
- ✅ **`dir_size_bytes_pub()` public wrapper** — exposes workspace size calculation for HTTP handlers
- ✅ **14th doctor check: LLM Providers** — reports configured provider count, fails if zero
- ✅ **14 doctor checks** — workspace, config, sessions, skills, LLM, metrics, cron, disk, webhook, memory, tasks, LLM providers, HTTP, uptime
- ✅ **201 tests** — 111 agent + 7 core + 83 gateway

## v0.92.0 — Cron & Sessions in /health (shipped)

- ✅ **`cron_jobs_count`, `sessions_db_size_bytes`, `sessions_db_size` in `/health`** — 40 total fields
- ✅ **`human_uptime(0)` test** — verifies zero-second boundary returns "0m 0s"
- ✅ **`process_rss_bytes` validity test** — verifies RSS is a reasonable u64 value
- ✅ **203 tests** — 111 agent + 7 core + 85 gateway (was 83)

## v0.93.0 — OS Info & Recursion Limit Fix (shipped)

- ✅ **`os_name` and `os_arch` in `/health`** — 42 total fields, shows OS and architecture
- ✅ **`#![recursion_limit = "256"]`** — fixes serde_json::json! macro expansion for 42+ field JSON
- ✅ **`human_bytes(1024)` boundary test** — verifies exact 1 KB and 1023 B boundaries
- ✅ **`human_uptime(0)` test** — verifies zero-second boundary returns "0m 0s"
- ✅ **`process_rss_bytes` validity test** — verifies RSS is a reasonable u64 value
- ✅ **205 tests** — 111 agent + 7 core + 87 gateway (was 85)

## v0.94.0 — Daemon & Polish

- 📋 **Unix socket daemon mode** — long-running agent process, CLI connects via socket
- 📋 **Slack integration**
- 📋 **Grafana dashboard template** — JSON dashboard for gateway metrics
- 💡 **WhatsApp integration**

---

## Architecture Notes

```
openclaw-rs/
├── crates/
│   ├── openclaw-core/       # Config, paths, cron, sessions, shared types (7 tests)
│   ├── openclaw-agent/      # LLM providers, 15 tools, runtime, sessions, sandbox (92 tests)
│   ├── openclaw-cli/        # Terminal interface with streaming
│   └── openclaw-gateway/    # Telegram + Discord bots, HTTP endpoints, metrics (21 tests)
│       ├── handler.rs       # Telegram: 8 commands, streaming, photo/vision, /voice TTS
│       ├── discord.rs       # Discord WebSocket Gateway, auto-reconnect, file upload
│       ├── discord_handler.rs # Discord: 8 commands, streaming, photo/vision, /voice TTS
│       ├── metrics.rs       # Atomic counters, Prometheus + JSON format
│       ├── telegram.rs      # Bot API client, voice upload, photo download
│       └── main.rs          # Polling + WS, /health, /status, /metrics, graceful shutdown
├── Dockerfile
├── docker-compose.gateway.yml
└── ROADMAP.md
```

### Streaming Pipeline (Telegram + Discord)

```
LLM SSE stream
  → stream_completion() parses chunks
  → StreamEvent variants (ContentDelta, ToolExec, ToolResult, RoundStart, Done)
  → mpsc::unbounded_channel
  → Handler accumulates text
  → Telegram: editMessageText every 80 chars / 400ms
  → Discord: editMessage every 80 chars / 400ms (2000 char limit)
  → Final edit with stats footer
```

### Voice Pipeline

```
/voice <text>
  → LLM response (agent turn with tools)
  → Piper TTS (WAV, jenny_dioco model)
  → ffmpeg (OGG/Opus, 64k VBR voip)
  → Telegram: sendVoice multipart
  → Discord: file upload with caption
  → Cleanup temp files
```

### HTTP Endpoints

```
GET /health          → "ok" (plain text)
GET /status          → JSON (version, uptime, sessions, channels, metrics, tools, commands)
GET /metrics         → Prometheus text/plain exposition format
GET /metrics/json    → JSON metrics
```

### Fallback Chain

```
ollama/llama3.2:1b → ollama/qwen2.5-coder:14b → moonshot/kimi-k2.5
  → deepseek-reasoner → deepseek-chat → anthropic/claude-opus-4-6
```

Circuit breaker: >3 consecutive failures = provider skipped.
Last successful model tracked in `RwLock<String>` for stats display.
