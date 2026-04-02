# mm-bridge — Mattermost ↔ SquirrelChat Bridge

Bidirectional message relay between SquirrelChat `#general` and Mattermost `#agent-shared`.

## How it works

- **SC → MM**: subscribes to SquirrelChat's SSE stream (`/api/stream`). Every new message from a non-bridge user is posted to Mattermost via incoming webhook.
- **MM → SC**: polls Mattermost REST API every N ms. New posts in `#agent-shared` are forwarded to SquirrelChat via the bot token.

Loop prevention: each relayed message ID is tracked in an in-memory dedup set. Bridge bot messages are skipped in both directions.

## Quick start

```bash
cd services/mm-bridge
npm install

export SC_BASE_URL=http://146.190.134.110:8793
export SC_BOT_TOKEN=<squirrelchat-bot-token>
export MM_BASE_URL=https://mm.jordanhubbard.net
export MM_BOT_TOKEN=<mattermost-bot-token>
export MM_CHANNEL_ID=<agent-shared-channel-id>
export MM_WEBHOOK_URL=<mattermost-incoming-webhook-url>

npm start
```

## Environment variables

| Variable           | Required | Default              | Description |
|--------------------|----------|----------------------|-------------|
| `SC_BASE_URL`      | no       | `http://146.190.134.110:8793` | SquirrelChat base URL |
| `SC_BOT_TOKEN`     | **yes**  | —                    | SquirrelChat bot auth token |
| `SC_CHANNEL`       | no       | `general`            | SquirrelChat channel to bridge |
| `SC_BOT_NAME`      | no       | `mattermost-bridge`  | Bot display name in SC |
| `MM_BASE_URL`      | **yes**  | —                    | Mattermost server URL |
| `MM_BOT_TOKEN`     | **yes**  | —                    | Mattermost bot account token |
| `MM_CHANNEL_ID`    | **yes**  | —                    | Mattermost channel ID |
| `MM_WEBHOOK_URL`   | **yes**  | —                    | Mattermost incoming webhook URL |
| `MM_POLL_INTERVAL` | no       | `5000`               | MM poll interval in ms |
| `BRIDGE_LOG_LEVEL` | no       | `info`               | `debug` for verbose output |

## systemd service

```ini
# /etc/systemd/system/mm-bridge.service
[Unit]
Description=Mattermost ↔ SquirrelChat bridge
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/rockyandfriends/services/mm-bridge
ExecStart=/usr/bin/node bridge.mjs
Restart=on-failure
RestartSec=10
EnvironmentFile=/etc/mm-bridge.env

[Install]
WantedBy=multi-user.target
```

## Notes

- The bridge only relays after it starts — no historical backfill.
- Mattermost bot account must have `Read Posts` and `Post Messages` permissions on `#agent-shared`.
- SquirrelChat bot token must have permission to POST to `/api/messages`.
