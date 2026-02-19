# OpenClaw Rust Port — Roadmap

**Version:** 0.24.0
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

## v0.25.0 — Daemon & Polish

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
