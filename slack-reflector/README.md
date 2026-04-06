# slack-reflector

Mirrors messages between two paired Slack workspaces transparently. Users and channels are matched by **name**, not by ID — if a user and channel exist with the same name on both workspaces, messages flow bidirectionally.

## How it works

```
┌─────────────────┐                          ┌─────────────────┐
│  omgjkh.slack   │ ◄── Socket Mode ──►      │  offtera.slack   │
│                 │                    │      │                 │
│  #general       │  ◄─── reflector ──►│      │  #general       │
│  #engineering   │  ◄─── reflector ──►│      │  #engineering   │
│  @jkh           │  ◄─── reflector ──►│      │  @jkh           │
│  @natasha       │  ◄─── reflector ──►│      │  @natasha       │
└─────────────────┘                    │      └─────────────────┘
                                       │
                              ┌────────┴────────┐
                              │ slack-reflector  │
                              │  • name cache    │
                              │  • thread map    │
                              │  • loop filter   │
                              └─────────────────┘
```

### Mirroring rules

- **Channel match**: A channel `#foo` exists on both workspaces → messages are mirrored
- **User match**: A user `@alice` exists on both workspaces → their messages are mirrored
- **No match → skip**: If either the user or channel doesn't have a counterpart, the message is silently dropped
- **Threads preserved**: Thread relationships are tracked across workspaces so replies stay threaded
- **Bot loop prevention**: Messages from the reflector bot itself are never re-mirrored

## Setup

### 1. Create Slack Apps

Create a Slack App on **each** workspace with these scopes:

**Bot Token Scopes:**
- `channels:history` — read messages
- `channels:read` — list channels
- `chat:write` — post messages
- `chat:write.customize` — post with custom username
- `users:read` — list users
- `groups:read` — list private channels
- `groups:history` — read private channel messages

**Socket Mode:** Enable Socket Mode and generate an App-Level Token with `connections:write` scope.

**Event Subscriptions (Socket Mode):**
- `message.channels`
- `message.groups`

### 2. Configure

```bash
cp config.example.yaml config.yaml
# Edit config.yaml with your tokens
```

### 3. Build & Run

```bash
cargo build --release
./target/release/slack-reflector config.yaml
```

### 4. Deploy as systemd service

```bash
sudo cp slack-reflector.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now slack-reflector
sudo journalctl -fu slack-reflector
```

## Configuration

```yaml
workspaces:
  - name: omgjkh
    bot_token: "xoxb-..."
    app_token: "xapp-..."
  - name: offtera
    bot_token: "xoxb-..."
    app_token: "xapp-..."

exclude_channels:
  - "random"        # skip even if name matches

exclude_users:
  - "slackbot"      # skip system users

cache_refresh_interval_secs: 300  # re-fetch user/channel lists
log_level: "info"                 # trace|debug|info|warn|error
health_port: 8780                 # HTTP health endpoint (0 = disabled)
```

## Architecture

- **Socket Mode** connections to both workspaces (no public URL needed)
- **DashMap** caches for concurrent name↔ID lookups
- **Thread map** tracks `(workspace, ts) → mirrored_ts` for threading
- **Single async event loop** — one `mpsc` channel feeds the reflector from both workspace listeners
- Posts messages using `chat.postMessage` with `username` override so messages appear as the original sender
