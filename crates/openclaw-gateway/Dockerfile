FROM rust:1.86-bookworm AS builder

WORKDIR /build
COPY . .

RUN cargo build --release --bin openclaw-gateway

# ── Runtime ──
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/openclaw-gateway /usr/local/bin/openclaw-gateway

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -sf http://localhost:3100/health || exit 1

RUN apt-get update && apt-get install -y --no-install-recommends curl && rm -rf /var/lib/apt/lists/*

EXPOSE 3100

ENV RUST_LOG=openclaw_gateway=info
ENV HTTP_PORT=3100

ENTRYPOINT ["openclaw-gateway"]
