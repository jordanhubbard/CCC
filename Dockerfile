# CCC — Claw Command Center
# Multi-stage build: Rust acc-server API binary.
#
# The WASM dashboard static files (dist/) are NOT baked into
# this image — they are bind-mounted at runtime from the repo checkout.
# The dist/ is pre-built and committed; kept current by wasm-build.yml CI.
#
# Build: docker build -t ccc .
# Run:   docker compose up   (see docker-compose.yml)

# ── Stage 1: Rust build ──────────────────────────────────────────────────
FROM rust:1.86-slim AS builder
WORKDIR /build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

# Cache deps first
COPY acc-server/Cargo.toml acc-server/Cargo.lock ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs
RUN cargo build --release --bin acc-server 2>/dev/null || true

# Full source
COPY acc-server/src ./src
RUN touch src/main.rs && cargo build --release --bin acc-server

# ── Stage 2: final image ─────────────────────────────────────────────────
FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    curl ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/acc-server /usr/local/bin/acc-server

# Deploy assets (scripts, templates)
COPY deploy/ ./deploy/
COPY workqueue/ ./workqueue/

# Data directories (overridden by volume mounts in production)
RUN mkdir -p /data/ccc /data/logs

# Non-root user for security
RUN groupadd -r ccc && useradd -r -g ccc -s /bin/false ccc \
 && chown -R ccc:ccc /app /data
USER ccc

# Port: 8789=CCC API (Rust/Axum)
EXPOSE 8789

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -f http://localhost:8789/health || exit 1

CMD ["acc-server"]
