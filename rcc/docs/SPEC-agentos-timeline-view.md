# agentOS Agent Lifecycle Timeline View

**Status:** Design proposal (2026-03-31)  
**Author:** Rocky / Natasha (Natasha idea: wq-NAT-idea-1774933716361-AGOS-TIMELINE)  
**Priority:** idea  
**Grounded in:** CCC dashboard live (:8789), cap_audit_log, fault_handler, quota_pd,
mem_profiler, watchdog all shipped.

---

## Problem

agentOS has five observability PDs that each emit structured events:
- `cap_audit_log` — capability grants and revocations
- `fault_handler` — SIGSEGV / unhandled exceptions
- `quota_pd` — quota exceeded, quota reset
- `mem_profiler` — memory alert, allocation spike
- `watchdog` — watchdog reset, heartbeat timeout

These are exposed as separate Prometheus metrics. When an agent slot crashes or misbehaves,
jkh has to cross-reference 5 different metric streams to reconstruct what happened. There
is no single "what happened to slot 3 between 22:05 and 22:08?" view.

---

## Proposed Feature

**A Timeline tab in the CCC Rust/WASM dashboard** that shows, per agent slot:
- A horizontal time axis (last 30 min, configurable)
- Colored event markers at their exact timestamps
- Tooltip on hover with event details

### Event Colors

| Event type     | Color  | Source PD      |
|----------------|--------|----------------|
| spawn          | green  | slot lifecycle |
| exit (clean)   | teal   | slot lifecycle |
| cap_grant      | blue   | cap_audit_log  |
| cap_revoke     | red    | cap_audit_log  |
| quota_exceeded | orange | quota_pd       |
| fault          | red X  | fault_handler  |
| watchdog_reset | yellow | watchdog       |
| mem_alert      | purple | mem_profiler   |

---

## Architecture

### Backend: `GET /api/agentos/events`

New CCC endpoint that aggregates events from all PD ring buffers via the existing
exec relay.

```
GET /api/agentos/events?slot=<n>&since=<unix_ms>&limit=<n>
Authorization: Bearer <token>

Response:
{
  "events": [
    {
      "slot": 2,
      "ts": 1711928400123,
      "type": "cap_grant",
      "pd": "cap_audit_log",
      "detail": "granted cap=IPC_SEND to pid=4712"
    },
    ...
  ],
  "slots": [0, 1, 2, 3],   // active slots
  "generated_at": 1711928410000
}
```

Implementation in `rcc/api/index.mjs`:

```js
// ── GET /api/agentos/events ───────────────────────────────────────────────
if (method === 'GET' && path.startsWith('/api/agentos/events')) {
  const { slot, since, limit = 100 } = qs(path);
  
  // Fetch from each PD via exec relay (reuse existing squirrelbus relay)
  const pdSources = [
    { name: 'cap_audit_log', cmd: 'cat /proc/agentos/cap_audit_log' },
    { name: 'fault_handler', cmd: 'cat /proc/agentos/fault_log' },
    { name: 'quota_pd',      cmd: 'cat /proc/agentos/quota_log' },
    { name: 'mem_profiler',  cmd: 'cat /proc/agentos/mem_log' },
    { name: 'watchdog',      cmd: 'cat /proc/agentos/watchdog_log' },
  ];
  
  // Execute on sparky where agentOS runs via exec relay
  const events = await aggregateAgentOSEvents(pdSources, { slot, since, limit });
  return json(res, 200, { events, generated_at: Date.now() });
}
```

**Note:** The PD ring buffers are read-only files in `/proc/agentos/` (seL4 shared memory
mapped by the monitor PD). No new kernel changes needed — this is pure userspace.

### Frontend: Timeline component (`rcc/dashboard/dashboard-ui/src/components/timeline.rs`)

Leptos component using SVG for rendering. Key decisions:
- SVG rather than Canvas (reactive, accessible, Leptos-friendly)
- Horizontal layout: one row per slot, time on X axis
- Auto-refresh every 10s via `set_interval`

```rust
// timeline.rs sketch
#[component]
pub fn Timeline() -> impl IntoView {
    let events = create_resource(|| (), |_| async {
        fetch_json::<AgentEventsResponse>("/api/agentos/events").await
    });
    
    view! {
        <div class="timeline-panel">
            <h2>"🗓️ Agent Lifecycle Timeline"</h2>
            <Suspense fallback=|| view! { <span>"Loading..."</span> }>
                {move || events.read().map(|data| {
                    // Group events by slot
                    // Render one SVG row per slot
                    view! { <TimelineSlots data=data /> }
                })}
            </Suspense>
        </div>
    }
}
```

### Tab integration

Add to `app.rs`:
1. New tab button: `"⏱️ Timeline"`
2. New match arm: `9 => view! { <Timeline /> }`

---

## Data Flow

```
agentOS PDs (seL4, sparky)
   └─ /proc/agentos/{cap_audit_log,fault_log,...}  (shared memory ring buffers)
         └─ squirrelbus exec relay (shell mode, existing)
               └─ GET /api/agentos/events  (new CCC endpoint)
                     └─ dashboard-server Axum proxy  (:8788/api/agentos/events)
                           └─ Timeline Leptos component (WASM browser)
```

---

## Zero New PD Infrastructure

All five event sources are already shipped. This feature requires:
1. One new CCC API route (~50 lines)
2. One new Leptos component (~150 lines Rust)
3. One new tab button in app.rs

No seL4 PD changes, no new ring buffers, no new kernel interfaces.

---

## Implementation Steps

1. **CCC endpoint** — `GET /api/agentos/events` in `rcc/api/index.mjs`
   - Shell exec via squirrelbus relay to sparky
   - Parse JSON lines from each PD ring buffer
   - Merge, sort by timestamp, filter by slot/since/limit
   - Estimated: 3h

2. **Types** — `AgentEvent`, `AgentEventsResponse` in `dashboard-ui/src/types.rs`
   - Estimated: 30min

3. **Timeline component** — `dashboard-ui/src/components/timeline.rs`
   - SVG rendering, slot rows, colored markers, hover tooltips
   - Estimated: 4h

4. **Integration** — Add tab to `app.rs`, `mod.rs`
   - Estimated: 30min

5. **WASM rebuild on sparky** — `make release` on sparky, copy dist/ to do-host1
   - Estimated: 5min (sccache warm build)

**Total: ~1d**

---

## Post-Mortem Use Case

After an agent slot fault, jkh opens Timeline tab, sees:

```
Slot 2:  ──[spawn]──[cap_grant]──[cap_grant]──[quota_exceeded]──[fault]──[watchdog_reset]──
         22:05:01   22:05:03     22:05:45      22:07:12          22:07:14  22:07:16
```

Immediately clear: quota was hit 2s before the fault. Cap grants look normal.
Root cause narrowed in seconds instead of minutes of cross-referencing Prometheus.
