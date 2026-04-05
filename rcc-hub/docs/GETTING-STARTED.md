# Getting Started with CCC

This guide walks you from zero to a working CCC hub + first agent, step by step.
It assumes you know how to SSH into a server and run commands, but have never set up
an AI agent coordination system before.

**What you'll have at the end:**
- An CCC hub running on a cloud VPS (AWS EC2 or Azure VM)
- A local agent (your Mac or Linux workstation) registered to the hub
- The agent appearing in the dashboard
- A work item submitted via curl and claimed by the agent

---

## Step 1: Spin Up a VPS

### AWS EC2

1. Launch a **t3.micro** (or larger) instance with **Ubuntu 22.04 LTS**
2. In the Security Group, open inbound TCP ports:
   - **22** (SSH)
   - **8789** (CCC API)
   - **8788** (Dashboard)
3. Note the public IP address: `your.server.ip`

### Azure VM

1. Create a **Standard_B1s** (or larger) VM with **Ubuntu 22.04**
2. In the Networking panel, add inbound port rules for **8789** and **8788**
3. Note the public IP address: `your.server.ip`

> **TLS note:** For a production deployment with a real domain, skip steps 3/4 above
> and open ports 80 and 443 instead. See [HTTPS.md](HTTPS.md) after completing this guide.

---

## Step 2: Install the Hub

SSH into your VPS and run the one-liner:

```bash
curl -fsSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/rcc-hub/install-hub.sh | bash
```

The installer will:
1. Install Node.js 20+ (if not already present)
2. Clone rcc-hub to `/opt/rcc-hub`
3. Run an interactive setup wizard
4. Install a systemd service that auto-starts on boot

### Setup Wizard

The wizard asks for a few things:

```
RCC_PORT (default: 8789):
  → Press Enter to accept the default

CCC_AUTH_TOKENS:
  → Paste a UUID you generated: node -e "console.log(require('crypto').randomUUID())"
  → This is the token your agents will use

RCC_ADMIN_TOKEN:
  → Generate a separate UUID for admin operations

RCC_PUBLIC_URL:
  → Enter: http://your.server.ip:8789
```

### Verify the Hub is Running

```bash
curl http://your.server.ip:8789/health
```

Expected response:

```json
{"status":"ok","uptime":12345,"queue":{"total":0,"pending":0}}
```

---

## Step 3: Open the Firewall Port

If you're using `ufw` (Ubuntu default):

```bash
sudo ufw allow 8789/tcp
sudo ufw allow 8788/tcp
sudo ufw status
```

For AWS: the Security Group rule you set in Step 1 handles this.
For Azure: the NSG rule you set in Step 1 handles this.

---

## Step 4: Onboard Your First Agent

On your **local Mac or Linux machine** (the agent):

```bash
curl -fsSL https://raw.githubusercontent.com/jordanhubbard/rockyandfriends/main/rcc-hub/install-agent.sh | bash
```

The agent installer will ask:

```
Hub URL:
  → http://your.server.ip:8789

Bootstrap token:
  → (Get this from the hub: curl http://your.server.ip:8789/api/bootstrap-token \
       -H "Authorization: Bearer YOUR_ADMIN_TOKEN")

Agent name:
  → my-laptop   (or whatever you want to call this machine)
```

The installer will:
1. Call `/api/onboard` to exchange the bootstrap token for a permanent agent token
2. Write `~/.rcc/.env` with your `AGENT_NAME` and `CCC_URL`
3. Set up a heartbeat so the hub knows this agent is alive

### Get a Bootstrap Token

```bash
curl -s http://your.server.ip:8789/api/bootstrap-token \
  -H "Authorization: Bearer YOUR_ADMIN_TOKEN"
# → {"token":"bt-...","expiresIn":"1h"}
```

Pass this to the agent installer when prompted.

---

## Step 5: Verify the Agent Appears in the Dashboard

Open your browser to:

```
http://your.server.ip:8788
```

You should see your agent listed with a green "online" indicator within 60 seconds
of the installer completing. If it shows "offline", check that the heartbeat cron
is running:

```bash
# On the agent machine:
crontab -l | grep rcc
```

Or trigger a manual heartbeat:

```bash
source ~/.rcc/.env
curl -X POST "$CCC_URL/api/heartbeat/$AGENT_NAME" \
  -H "Authorization: Bearer $CCC_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"host":"my-laptop","status":"online"}'
```

---

## Step 6: Submit Your First Work Item

From **anywhere** (your laptop, the hub, another machine):

```bash
curl -s -X POST http://your.server.ip:8789/api/queue \
  -H "Authorization: Bearer YOUR_AGENT_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "assignee": "my-laptop",
    "title": "Hello CCC — first work item",
    "description": "Print hello world and verify the queue works.",
    "priority": "normal"
  }' | jq .
```

You should get back something like:

```json
{
  "ok": true,
  "item": {
    "id": "wq-API-...",
    "status": "pending",
    "title": "Hello CCC — first work item",
    ...
  }
}
```

### Watch the Item Get Claimed

If an OpenClaw agent is running on `my-laptop` with the workqueue-processor cron,
it will claim and complete the item on the next hourly cycle.

To manually check queue status:

```bash
curl -s http://your.server.ip:8789/api/queue \
  -H "Authorization: Bearer YOUR_AGENT_TOKEN" | jq '.items | map({id,title,status})'
```

---

## What's Next

| Goal | Guide |
|------|-------|
| Add HTTPS with auto-TLS | [HTTPS.md](HTTPS.md) |
| Deploy with Docker Compose | [DOCKER.md](DOCKER.md) |
| Add more agents | Re-run `install-agent.sh` on each machine |
| Configure Slack notifications | Set `SLACK_BOT_TOKEN` in `.env` |
| Route LLM calls through the hub | Set `TOKENHUB_URL` in `.env` |

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `curl: (7) Failed to connect` | Firewall not open | Open port 8789 in security group / ufw |
| Agent shows offline immediately | Heartbeat not scheduled | `crontab -l` and check rcc entry |
| `401 Unauthorized` | Wrong token | Double-check `CCC_AUTH_TOKENS` in hub `.env` |
| Hub not starting | systemd error | `journalctl -u rcc-hub -n 50` |
| Dashboard blank / 502 | Dashboard binary not built | See [DOCKER.md](DOCKER.md) build instructions |
