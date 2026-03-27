# claw-watchdog sidecar — runs alongside OpenClaw in a container stack
# Usage: add to your docker-compose.yml as a sidecar service (see README)
# Or build standalone: docker build -f claw-watchdog.Dockerfile -t claw-watchdog .

FROM alpine:3.19

RUN apk add --no-cache bash curl python3 docker-cli procps

COPY deploy/claw-watchdog.sh /usr/local/bin/claw-watchdog.sh
RUN chmod +x /usr/local/bin/claw-watchdog.sh

# Default config — override via environment in docker-compose
ENV MODE=docker \
    CHECK_INTERVAL=30 \
    HANG_THRESHOLD=120 \
    HEARTBEAT_STALE=600 \
    MAX_RESTARTS=5 \
    RESTART_WINDOW=3600 \
    LOG_FILE=/var/log/watchdog.log

ENTRYPOINT ["bash", "/usr/local/bin/claw-watchdog.sh"]
