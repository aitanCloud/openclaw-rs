# openclaw-rs

Fast Rust rewrite of [OpenClaw](https://github.com/openclaw/openclaw) — an AI agent framework with Telegram bot gateway, 9 built-in tools, streaming responses, and plugin system.

## Features

- **9 built-in tools** — exec, read, write, list_dir, patch, grep, find, web_search, web_fetch
- **Script plugin system** — extend with shell-based tools via `.openclaw/plugins/*.json` manifests
- **Telegram bot gateway** — streaming responses with real-time message editing
- **Fallback LLM chain** — automatic failover across providers (Ollama, DeepSeek, Moonshot, Anthropic, etc.)
- **LLM retry with backoff** — exponential retry for transient errors (429, 502, 503, 504)
- **Sandbox policies** — command blocklist, path allowlist, timeout clamping
- **Rate limiting** — per-user sliding window with configurable limits
- **Graceful shutdown** — SIGINT drains active tasks before exit
- **Token-aware context** — estimates tokens, prunes history to fit context window
- **Session persistence** — SQLite-backed conversation history
- **6 Telegram commands** — /help, /new, /status, /model, /sessions, /export
- **Config-driven** — rate limits, concurrency, sandbox all configurable

## Build

```bash
# Local (NixOS)
nix-shell -p gcc --run "cargo build --release"

# Run tests
nix-shell -p gcc --run "cargo test --release"

# Docker
docker compose -f docker-compose.gateway.yml build
docker compose -f docker-compose.gateway.yml up -d
```

## Architecture

```
crates/
├── openclaw-core/      # Config parsing, cron, sessions, skills, paths
├── openclaw-agent/     # LLM providers, 9 tools, workspace, agent loop, sandbox
│   └── src/
│       ├── llm/        # Provider trait, streaming, fallback chain, retry
│       ├── runtime.rs  # Agent turn loop (parallel tools, token-aware history)
│       ├── sandbox.rs  # SandboxPolicy: blocklist, allowlist, timeout clamping
│       ├── sessions.rs # SQLite session persistence
│       ├── tools/      # exec, read, write, list_dir, patch, grep, find,
│       │               # web_search, web_fetch, script_plugin
│       └── workspace.rs
├── openclaw-cli/       # CLI binary (clap)
└── openclaw-gateway/   # Telegram bot gateway
    └── src/
        ├── handler.rs  # Message handler, streaming edits, 6 commands
        ├── ratelimit.rs
        ├── config.rs   # Gateway config with sandbox section
        ├── telegram.rs # Bot API client (Markdown + plain fallback)
        └── main.rs     # Polling, graceful shutdown, /health, /status
```

## Tests

67 tests (55 agent + 7 core + 5 gateway)

## Version History

See [ROADMAP.md](ROADMAP.md) for full version history (v0.1.0 through v0.9.0).

## Config Compatibility

Reads existing OpenClaw config files from `~/.openclaw/` — fully compatible with the Node.js version.
