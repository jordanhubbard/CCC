# AgentBus Receiver

Receives AgentBus push messages from the hub and makes them available to the local agent runtime.

AgentBus is **not** a plugin into a runtime — it is a standalone SSE subscriber daemon managed by systemd (Linux) or launchd (macOS). It connects outbound to the hub's SSE stream and appends received messages to a local log.

---

## How It Works

1. `ccc-bus-listener` connects to `$CCC_URL/bus/stream` (SSE)
2. Incoming messages are validated with `AGENTBUS_TOKEN` (HMAC-SHA256)
3. Messages are appended to `~/.ccc/logs/bus.jsonl`
4. The hermes-agent runtime reads new entries on its next poll cycle

No inbound port is required. The agent reaches out to the hub; the hub does not reach in.

---

## Setup

The listener is installed and started by `deploy/setup-node.sh` / `deploy/acc-init.sh`. To install manually:

### Linux (systemd)

```bash
sudo cp ~/.ccc/workspace/deploy/systemd/ccc-bus-listener.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now ccc-bus-listener
```

### macOS (launchd)

```bash
cp ~/.ccc/workspace/deploy/launchd/com.ccc.bus-listener.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.ccc.bus-listener.plist
```

### Environment

The service reads these variables from `~/.ccc/.env`:

| Variable | Purpose |
|----------|---------|
| `CCC_URL` | Hub base URL (SSE stream at `$CCC_URL/bus/stream`) |
| `CCC_AGENT_TOKEN` | Bearer token for authenticating to the hub |
| `AGENTBUS_TOKEN` | Shared HMAC-SHA256 secret for payload validation |
| `AGENT_NAME` | This agent's name (used when filtering directed messages) |

---

## Verifying

```bash
# Check service status
systemctl status ccc-bus-listener      # Linux
launchctl list | grep bus-listener     # macOS

# Watch incoming messages
tail -f ~/.ccc/logs/bus.jsonl | jq .

# Send a test message from the hub
curl -X POST $CCC_URL/api/bus/send \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"to":"all","subject":"ping","body":"hello"}'
```

---

## Remote Exec

The related `ccc-exec-listen` service handles `ccc.exec` messages from AgentBus — signed payloads that run sandboxed code and POST results back to the hub. It is managed separately by `ccc-exec-listen.service` / `com.ccc.exec-listen.plist`.

See [../SPEC.md](../SPEC.md) for the full AgentBus protocol.
