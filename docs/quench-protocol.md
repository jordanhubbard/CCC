# Agent Quench Protocol

The **quench** mechanism lets any agent (or operator) send a pause signal to one
or all agents in the fleet via ClawBus. Receivers finish their current work unit,
then block for the specified duration before resuming.

## Design Goals

- **Non-interruptive**: agents always finish the task they are currently executing before checking quench state
- **Time-bounded**: hard cap of 30 minutes (`MAX_QUENCH_MINUTES`); no agent can be silenced indefinitely
- **First-wins**: if a quench is already active, subsequent signals are ignored until it expires
- **Self-healing**: quench expires automatically — no manual resume required
- **Auditable**: every quench event is appended to `~/.rcc/logs/quench.jsonl`

## ClawBus Message Schema

```json
{
  "type": "rcc.quench",
  "from": "<sender-agent>",
  "to":   "all | <agent-name>",
  "mime": "application/json",
  "body": {
    "target":           "all | <agent-name>",
    "duration_minutes": 10,
    "reason":           "optional human note"
  }
}
```

## Using the CLI

```sh
# Pause all agents for 10 minutes (deploying a new model)
CCC_AUTH_TOKEN=... node rcc/scripts/send-quench.mjs all 10 "deploying gemma-4-31B"

# Pause a specific agent for 5 minutes
CCC_AUTH_TOKEN=... node rcc/scripts/send-quench.mjs peabody 5 "GPU busy"
```

## Agent Integration

### checkQuench()

Call `checkQuench()` from `rcc/exec/quench.mjs` **between work units** in any
agent loop. It returns immediately when no quench is active, or awaits expiry
when one is.

```js
import { checkQuench } from '../exec/quench.mjs';

async function agentWorkLoop() {
  while (true) {
    await checkQuench(); // ← insert between work units
    const item = await claimNextWorkItem();
    if (!item) { await sleep(30_000); continue; }
    await processItem(item);
    await checkQuench(); // ← also check after completing a task
  }
}
```

### handleQuenchMessage()

`agent-listener.mjs` already wires `handleQuenchMessage()` to `rcc.quench`
ClawBus messages. No additional wiring is needed in the listener.

## Log Format

Each event appended to `~/.rcc/logs/quench.jsonl`:

```jsonl
{"ts":"2026-04-04T18:30:00Z","agent":"peabody","event":"quenched","from":"rocky","target":"all","duration_minutes":10,"until":"2026-04-04T18:40:00Z","reason":"deploying gemma"}
{"ts":"2026-04-04T18:30:01Z","agent":"peabody","event":"pausing","remainMs":599000,"until":"2026-04-04T18:40:00Z","from":"rocky"}
{"ts":"2026-04-04T18:40:00Z","agent":"peabody","event":"resumed"}
```

Event types: `quenched`, `ignored`, `pausing`, `resumed`.

## Constraints

| Constraint | Value |
|---|---|
| Max duration | 30 minutes |
| Min duration | 1 minute |
| First-wins | Yes (subsequent ignored while active) |
| Target `all` | Applies to every agent subscribed to ClawBus |
| Interruption | Never — current work unit always completes first |
