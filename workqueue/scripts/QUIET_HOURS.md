# Quiet Hours / Do-Not-Disturb Protocol

**wq-20260319-015** — implemented by the implementing agent, 2026-03-21

---

## Window

**23:00 – 08:00 America/Los_Angeles (PT)**

The window wraps midnight: an agent is in quiet hours when `hour >= 23 OR hour < 8`.

---

## Module

```js
import { isQuietHours, shouldSkipAction } from './quiet-hours-check.mjs';
```

### `isQuietHours() → boolean`

Returns `true` if the current PT time falls inside the quiet window.

```js
if (isQuietHours()) {
  console.log('Running in quiet hours — suppressing noisy actions');
}
```

### `shouldSkipAction(actionType) → boolean`

Returns `true` if the named action should be skipped right now.
Unknown action types always return `false` (safe default — don't suppress unknown work).

| `actionType`     | Quiet hours | Outside quiet hours |
|------------------|-------------|---------------------|
| `gpu_task`       | skip (true) | proceed (false)     |
| `slack_ping`     | skip (true) | proceed (false)     |
| `noisy_external` | skip (true) | proceed (false)     |
| *(anything else)*| false       | false               |

```js
if (shouldSkipAction('slack_ping')) {
  console.log('Quiet hours — deferring Slack DM until morning');
} else {
  await slackDm(userId, message);
}
```

---

## Action type definitions

| Type | Examples |
|------|---------|
| `gpu_task` | Stable Diffusion renders, large model inference, USD scene renders |
| `slack_ping` | DMs to jkh, @channel pings, urgent-flag messages |
| `noisy_external` | Email sends, loud webhooks, third-party API calls that log/bill per-call |

---

## Agent usage (Rocky / Bullwinkle / Natasha)

Check at the **top of each cron cycle** before dispatching expensive or human-visible work:

```js
import { shouldSkipAction } from '../workqueue/scripts/quiet-hours-check.mjs';

// Before a GPU render
if (shouldSkipAction('gpu_task')) {
  log('Quiet hours — deferring GPU task to next cycle');
  return;
}

// Before pinging jkh
if (shouldSkipAction('slack_ping')) {
  enqueueDeferred('slack_ping', payload);
  return;
}
```

No configuration needed — the module reads the system clock directly.
