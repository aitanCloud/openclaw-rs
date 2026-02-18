# openclaw-rs

Fast Rust CLI replacement for [OpenClaw](https://github.com/openclaw/openclaw).

## Why?

The Node.js CLI takes ~4 seconds for `openclaw --version` due to cold start + process respawn. This Rust port targets <10ms.

## What's ported

- `openclaw --version` — instant version check
- `openclaw sessions list` — list sessions across agents
- `openclaw skills list` — list installed skills
- `openclaw config show` — show configuration summary
- `openclaw cron list` — list cron jobs

## Build

```bash
cargo build --release
./target/release/openclaw --version
```

## Docker Testing

```bash
docker build -t openclaw-rs-test .
docker run --rm openclaw-rs-test
```

## Architecture

```
crates/
├── openclaw-core/    # Shared types, config parsing, paths
└── openclaw-cli/     # CLI binary (clap)
```

Reads existing OpenClaw config files from `~/.openclaw/` — fully compatible with the Node.js version.
