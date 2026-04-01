# RCC — Rocky Command Center
# Multi-stage build: RCC API + SquirrelChat (Node.js/Express) in one image.
#
# The WASM dashboard static files (rcc/dashboard/dist/) are NOT baked into
# this image — they are bind-mounted at runtime from the repo checkout.
# The dist/ is pre-built and committed; kept current by wasm-build.yml CI.
#
# Build: docker build -t rcc .
# Run:   docker compose up   (see docker-compose.yml)

# ── Stage 1: deps ──────────────────────────────────────────────────────────
FROM node:22-slim AS deps
WORKDIR /app

# RCC deps
COPY rcc/package.json rcc/package-lock.json* ./rcc/
RUN cd rcc && npm ci --omit=dev

# SquirrelChat deps
COPY squirrelchat/package.json squirrelchat/package-lock.json* ./squirrelchat/
RUN cd squirrelchat && npm ci --omit=dev

# Root-level deps (shared utils)
COPY package.json package-lock.json* ./
RUN npm ci --omit=dev 2>/dev/null || true

# ── Stage 2: final image ───────────────────────────────────────────────────
FROM node:22-slim
WORKDIR /app

# Runtime deps only
RUN apt-get update && apt-get install -y --no-install-recommends \
    curl \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Copy source
COPY --from=deps /app/node_modules ./node_modules
COPY --from=deps /app/rcc/node_modules ./rcc/node_modules
COPY --from=deps /app/squirrelchat/node_modules ./squirrelchat/node_modules

# RCC API
COPY rcc/ ./rcc/

# SquirrelChat backend
COPY squirrelchat/server.mjs ./squirrelchat/server.mjs
COPY squirrelchat/public/ ./squirrelchat/public/

# Deploy assets (scripts, templates)
COPY deploy/ ./deploy/
COPY onboarding/ ./onboarding/
COPY workqueue/ ./workqueue/

# Data directories (overridden by volume mounts in production)
RUN mkdir -p /data/rcc /data/squirrelchat /data/logs

# Non-root user for security
RUN groupadd -r rcc && useradd -r -g rcc -s /bin/false rcc \
 && chown -R rcc:rcc /app /data
USER rcc

# Ports
EXPOSE 8789   # RCC API
EXPOSE 8790   # SquirrelChat

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -f http://localhost:8789/health || exit 1

# Default: start RCC API. SquirrelChat runs as a separate compose service.
CMD ["node", "rcc/api/index.mjs"]
