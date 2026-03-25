# 🐿️ Rocky Workqueue Dashboard

Live dashboard for the Rocky/Bullwinkle/Natasha agent workqueue system.

## Public URL

**http://localhost:8788/**

## Architecture

- **Port:** 8788 (public, bound to 0.0.0.0)
- **Framework:** Node.js + Express
- **Data:** Reads `~/.openclaw/workspace/workqueue/queue.json` live on every request
- **Heartbeats:** Merges MinIO heartbeat files + in-memory agent POSTs
- **systemd unit:** `wq-dashboard.service` (enabled, auto-restart)

## Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| GET | `/` | No | Dashboard HTML (server-rendered, live data) |
| GET | `/api/queue` | No | Full queue.json as JSON |
| GET | `/api/heartbeats` | No | Merged heartbeat data for all 3 agents |
| POST | `/api/upvote/:id` | Yes | Promote idea → pending task |
| POST | `/api/comment/:id` | Yes | Comment/delete/subtask on item |
| POST | `/api/complete/:id` | Yes | Mark item completed |
| POST | `/api/heartbeat/:agent` | Yes | Agents POST their status |

## Auth

Write endpoints require `Authorization: Bearer <your-rcc-token>`.

The browser dashboard prompts for the token on first action and stores it in sessionStorage.

## Service Management

```bash
sudo systemctl status wq-dashboard
sudo systemctl restart wq-dashboard
journalctl -u wq-dashboard -f
```

## Comment Endpoint Intelligence

POST `/api/comment/:id` with `{text: "..."}` parses intent:
- `"delete"` or `"remove"` → deletes the item
- `"break into X, Y, Z"` or contains `"subtask"` → unblocks + adds subtask notes
- Anything else → unblocks item + appends comment to notes
