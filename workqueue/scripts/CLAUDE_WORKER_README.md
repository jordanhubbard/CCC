# claude-worker.mjs — Claude Code tmux delegation module

A reusable Node.js ESM module for delegating tasks to a local Claude Code CLI session
running inside a tmux pane. Works on any OpenClaw host.

---

## Quick start

```js
import {
  detectSession,
  sendTask,
  sendTaskBackground,
  pollUntilDone,
} from './claude-worker.mjs';

// Auto-detect which tmux session is running Claude
const session = detectSession();   // e.g. "claude-puck", "auth3"

// Send a task and wait for the result
const { done, output, elapsed } = await sendTask(session, 'summarize /tmp/notes.txt');
console.log(output);

// Fire-and-forget (appends & prefix)
sendTaskBackground(session, 'process the overnight log batch');
```

---

## API

### `detectSession() → string | null`

Scans `tmux list-panes -a` and returns the **first session** whose pane is running
`claude` (matched by `pane_current_command`) or whose visible pane output contains
the Claude Code idle prompt (`❯` / `? for shortcuts`).

Returns `null` if no Claude session is found.

```js
const session = detectSession();
if (!session) throw new Error('No Claude session running');
```

---

### `sendTask(sessionName, task, opts) → Promise<{done, output, elapsed}>`

Types `task` into the named tmux session (pressing Enter), then polls the pane
until the Claude Code idle prompt reappears or the timeout is hit.

**Parameters**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `sessionName` | string | — | tmux session name |
| `task` | string | — | Task text to send |
| `opts.timeoutMs` | number | `120_000` | Hard timeout in ms |
| `opts.startupWaitMs` | number | `1_500` | Wait after send before polling |
| `opts.pollIntervalMs` | number | `800` | How often to check pane |
| `opts.debug` | boolean | `false` | Print debug lines to stderr |

**Returns** `{ done: boolean, output: string, elapsed: number }`

- `done` — `true` if the prompt returned before timeout
- `output` — full visible pane text (ANSI stripped) at completion time
- `elapsed` — ms from send to done

```js
const { done, output } = await sendTask('claude-sparky', 'write a haiku about Redis', {
  timeoutMs: 60_000,
  debug: true,
});
```

---

### `sendTaskBackground(sessionName, task) → void`

Sends the task with a `& ` prefix so Claude treats it as a background job.
Returns immediately — no waiting. Use for long-running or fire-and-forget work.

```js
sendTaskBackground('claude-puck', 'run the full test suite and report');
```

---

### `pollUntilDone(sessionName, timeoutMs, pollIntervalMs) → Promise<{done, output}>`

Lower-level poller. Useful if you already sent keys manually and just want to
wait for the session to go idle.

```js
// You already sent a command; now just wait
const { done, output } = await pollUntilDone('auth3', 90_000);
```

---

## Idle prompt detection

The module considers a session **done** when the captured pane text (ANSI-stripped) matches
any of:

- `❯ ` at the start of a line (Claude Code input prompt)
- `? for shortcuts` anywhere visible (Claude Code status bar)
- `> ` at start of line (ASCII fallback)

AND no spinner characters (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`) are visible in the last 6 lines.

---

## Per-host session names

| Host | Agent | Typical session name |
|------|-------|----------------------|
| do-host1 | Rocky | `auth3` (or whatever `detectSession()` finds) |
| puck | Bullwinkle | `claude-puck` (check with `tmux ls`) |
| sparky | Natasha | `claude-sparky` (check with `tmux ls`) |

Always call `detectSession()` first rather than hardcoding a name — it will find
whatever is running, even if the session was renamed.

---

## Self-test

```bash
node workqueue/scripts/claude-worker.mjs --test
```

Finds the active Claude session, sends a test echo command, and prints the result.
Exit code 0 = pass, 1 = fail or timeout.

---

## Notes for Bullwinkle / Natasha

- Copy (or symlink) this file to your own `workqueue/scripts/` directory, or reference
  it via an absolute path / MinIO-fetched copy.
- The module has **no npm dependencies** — only Node.js built-ins (`child_process`).
- If your Claude session has a custom pane title, pass the session name explicitly
  rather than relying on `detectSession()`.
- For tasks that produce large output, increase `timeoutMs` and note that `output`
  contains only the **visible pane buffer** (~200 lines). For full output, redirect
  inside the task: `write a report > /tmp/report.txt`.
