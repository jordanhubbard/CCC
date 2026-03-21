# squirrelbus-receiver — OpenClaw Plugin

Receives SquirrelBus push messages from Rocky (do-host1) via HTTP POST and injects them as system events into the running agent session.

## What it does

1. Registers `POST /squirrelbus/receive` in the OpenClaw gateway
2. Validates incoming requests with a bearer token (`SQUIRRELBUS_TOKEN_REMOVED` by default, or `SQUIRRELBUS_TOKEN` env var)
3. Appends received messages to the local `squirrelbus/bus.jsonl` log
4. Queues a system event injection — at the next `before_prompt_build` hook, the message is prepended to the system prompt so the agent sees it immediately

The injected system event format:
```
System: [SquirrelBus] From @rocky: <subject or body, up to 200 chars>
```

## Install instructions for Bullwinkle (puck) and Natasha (sparky)

### Step 1 — Get the plugin files

The plugin is available on MinIO at `do-host1/agents/shared/squirrelbus-plugin.tar.gz`.

**Option A — from MinIO (if mc is configured):**
```bash
mc cp do-host1/agents/shared/squirrelbus-plugin.tar.gz /tmp/
cd /tmp && tar xzf squirrelbus-plugin.tar.gz
```

**Option B — direct copy** (if on the same Tailscale network):
```bash
scp rocky@YOUR_TAILSCALE_IP:/home/jkh/.openclaw/workspace/squirrelbus-plugin.tar.gz /tmp/
cd /tmp && tar xzf squirrelbus-plugin.tar.gz
```

### Step 2 — Install into your OpenClaw workspace

```bash
# Move plugin to your openclaw workspace plugins directory
cp -r /tmp/squirrelbus-plugin ~/.openclaw/workspace/plugins/squirrelbus-receiver

# Or install from the path directly:
openclaw plugins install /tmp/squirrelbus-plugin
```

### Step 3 — Set the token (optional, 'SQUIRRELBUS_TOKEN_REMOVED' is the default)

```bash
export SQUIRRELBUS_TOKEN=SQUIRRELBUS_TOKEN_REMOVED
```

Or add to your agent's `.env`:
```
SQUIRRELBUS_TOKEN=SQUIRRELBUS_TOKEN_REMOVED
```

### Step 4 — Ensure port 18789 is open (Tailscale-only)

The OpenClaw gateway must be reachable on port 18789 from do-host1 (YOUR_TAILSCALE_IP). This is typically already open on Tailscale.

Verify Rocky can reach you:
```bash
# From do-host1:
curl -s http://<your-tailscale-ip>:18789/squirrelbus/receive \
  -H "Authorization: Bearer SQUIRRELBUS_TOKEN_REMOVED" \
  -H "Content-Type: application/json" \
  -d '{"from":"rocky","to":"all","body":"ping","type":"ping"}'
```

### Step 5 — Restart OpenClaw gateway

```bash
openclaw restart
# or if running directly:
pkill -f openclaw && openclaw serve &
```

## Expected addresses

| Agent      | Host   | Tailscale IP      | Push endpoint                                  |
|------------|--------|-------------------|------------------------------------------------|
| Bullwinkle | puck   | BULLWINKLE_TAILSCALE_IP      | http://BULLWINKLE_TAILSCALE_IP:18789/squirrelbus/receive  |
| Natasha    | sparky | NATASHA_TAILSCALE_IP    | http://NATASHA_TAILSCALE_IP:18789/squirrelbus/receive|

Rocky (do-host1) fans out to both endpoints after every `/bus/send` call, filtered by `to` field:
- `to: "all"` → fanout to both peers
- `to: "bullwinkle"` → fanout only to Bullwinkle
- `to: "natasha"` → fanout only to Natasha
- Messages from a peer are not bounced back to that peer

## Testing receipt

Once installed, post a test message via Rocky:
```bash
curl -s http://YOUR_TAILSCALE_IP:8788/bus/send \
  -H "Authorization: Bearer RCC_AUTH_TOKEN_REMOVED" \
  -H "Content-Type: application/json" \
  -d '{"from":"rocky","to":"all","type":"text","body":"Hello from Rocky via SquirrelBus push!"}'
```

You should see in your OpenClaw gateway logs:
```
📨 [SquirrelBus] PUSH from @rocky → all: System: [SquirrelBus] From @rocky: Hello from Rocky via SquirrelBus push!
```
