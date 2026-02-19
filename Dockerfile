# Stage 1: Build all Rust binaries
FROM rust:1.83-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY crates/ crates/

RUN cargo build --release --bin openclaw --bin openclaw-gateway

# Stage 2: Test in a clean Debian environment with Node.js (simulates coexistence)
FROM debian:bookworm-slim AS test

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl ffmpeg && \
    rm -rf /var/lib/apt/lists/*

# Install Node.js 22 for TS gateway coexistence testing
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
    apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/*

# Copy the Rust binaries
COPY --from=builder /build/target/release/openclaw /usr/local/bin/openclaw
COPY --from=builder /build/target/release/openclaw-gateway /usr/local/bin/openclaw-gateway

# Create a mock ~/.openclaw structure for testing
RUN mkdir -p /root/.openclaw/cron \
             /root/.openclaw/agents/main/sessions \
             /root/.openclaw/agents/main/agent \
             /root/.openclaw/workspace/skills/web-search \
             /root/.openclaw/workspace/skills/github-ops \
             /root/.openclaw/workspace/skills/system-monitor

# Mock openclaw-manual.json
RUN echo '{ \
  "meta": {"name": "aitan"}, \
  "env": {}, \
  "browser": {}, \
  "models": {}, \
  "agents": {"defaults": {"timeoutSeconds": 120, "maxConcurrent": 2}}, \
  "tools": {}, \
  "commands": {}, \
  "channels": {}, \
  "gateway": {"mode": "local", "bind": "127.0.0.1", "port": 3001, "auth": "token"}, \
  "plugins": {}, \
  "session": {}, \
  "messages": {} \
}' > /root/.openclaw/openclaw-manual.json

# Mock cron/jobs.json
RUN echo '{ \
  "version": 1, \
  "jobs": [ \
    { \
      "id": "test-health", \
      "name": "System health check", \
      "enabled": true, \
      "schedule": {"kind": "cron", "expr": "30 6 * * *", "tz": "America/New_York"}, \
      "payload": {"kind": "agentTurn", "message": "Quick health check"}, \
      "delivery": {"mode": "announce", "channel": "last"}, \
      "state": {"lastStatus": "ok", "lastDurationMs": 28554, "consecutiveErrors": 0} \
    }, \
    { \
      "id": "test-security", \
      "name": "Security threat intel", \
      "enabled": true, \
      "schedule": {"kind": "cron", "expr": "0 6 * * *", "tz": "America/New_York"}, \
      "payload": {"kind": "agentTurn", "message": "Check CVEs", "model": "openai-compatible/deepseek-chat"}, \
      "delivery": {"mode": "announce", "channel": "last"}, \
      "state": {"lastStatus": "ok", "lastDurationMs": 103615, "consecutiveErrors": 0} \
    } \
  ] \
}' > /root/.openclaw/cron/jobs.json

# Mock sessions
RUN echo '{ \
  "sessions": [ \
    {"key": "abc123", "label": "Dashboard fixes", "messageCount": 42, "model": "ollama/llama3.2:1b"}, \
    {"key": "def456", "label": "Cron optimization", "messageCount": 15, "model": "moonshot/kimi-k2.5"} \
  ] \
}' > /root/.openclaw/agents/main/sessions/sessions.json

# Mock agent config
RUN echo '{"provider": "ollama", "model": "llama3.2:1b"}' > /root/.openclaw/agents/main/agent/config.json

# Mock skills
RUN echo '---\ndescription: Search the web using Brave\n---\nUse browser tool to search.' > /root/.openclaw/workspace/skills/web-search/SKILL.md
RUN echo '---\ndescription: Manage GitHub repositories and issues\n---\nUse gh CLI.' > /root/.openclaw/workspace/skills/github-ops/SKILL.md
RUN echo '---\ndescription: Monitor system health and resources\n---\nUse exec tool.' > /root/.openclaw/workspace/skills/system-monitor/SKILL.md

# Copy test script
COPY docker-test.sh /test.sh
RUN chmod +x /test.sh

CMD ["/test.sh"]
