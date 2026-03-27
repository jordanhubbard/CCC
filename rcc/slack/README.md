# RCC Slack Integration

Rocky Command Center can receive and respond to Slack messages — `app_mention` events, DMs, and slash commands.

## Architecture

```
Slack → POST /api/slack/events    (app mentions, DMs)
Slack → POST /api/slack/commands  (slash commands: /rcc)
RCC   → POST /api/slack/send      (outbound, authenticated)
```

All inbound events are signature-verified using `SLACK_SIGNING_SECRET`.  
Brain replies are routed through the RCC brain (`rcc/brain/index.mjs`).

## Setup

### 1. Create the Slack App

1. Go to https://api.slack.com/apps
2. Click **Create New App** → **From an app manifest**
3. Select workspace: `omgjkh.slack.com` (**do NOT use offtera until validated**)
4. Paste the contents of `rcc/slack/manifest.json`
5. Replace `YOUR_RCC_URL` with your actual RCC public URL (e.g. `http://146.190.134.110:8789`)
6. Click **Create**

### 2. Get credentials

From the app settings:
- **Signing Secret**: Settings → Basic Information → App Credentials → Signing Secret
- **Bot Token**: Settings → OAuth & Permissions → Bot User OAuth Token (starts with `xoxb-`)

### 3. Configure environment variables

Add to `/home/jkh/.rcc/.env` (or wherever RCC reads env):

```bash
SLACK_SIGNING_SECRET=your_signing_secret_here
SLACK_BOT_TOKEN=xoxb-your-bot-token-here
```

Then restart the RCC API service.

### 4. Install to workspace

In the Slack App settings → **Install App** → **Install to Workspace** → Authorize.

### 5. Verify the events endpoint

Slack will send a `url_verification` challenge to `/api/slack/events`. This is handled automatically. You'll see a green checkmark in the Event Subscriptions settings once verified.

## Usage

### App mentions
In any channel where Rocky is present:
```
@Rocky what's in the queue?
@Rocky how many agents are online?
```

### Slash commands
```
/rcc             — same as /rcc status
/rcc status      — show agent heartbeat status
/rcc queue       — show pending work items
/rcc ask <q>     — ask the RCC brain anything
```

### Direct messages
DM Rocky directly — messages are routed to the brain.

## Test Scenario

1. Invite Rocky to a channel: `/invite @Rocky`
2. Type: `@Rocky what is in the queue?`
3. Rocky receives the mention via `/api/slack/events`
4. RCC brain processes the question
5. Rocky replies in thread with queue status

## API Reference

### POST /api/slack/send (authenticated)
```json
{
  "channel": "C0A1B2C3D",
  "text": "Hello from RCC",
  "thread_ts": "1234567890.123456"
}
```

### POST /api/slack/events (Slack-signed)
Handles: `url_verification`, `app_mention`, `message.im`

### POST /api/slack/commands (Slack-signed)
Handles: `/rcc [status|queue|ask <question>]`

## Security Notes

- Signature verification uses HMAC-SHA256 with `SLACK_SIGNING_SECRET`
- Replay attacks (>5 min old requests) are rejected
- `SLACK_BOT_TOKEN` is never exposed in responses
- ⚠️ Test on `omgjkh.slack.com` only — do NOT deploy to `offtera` until validated
