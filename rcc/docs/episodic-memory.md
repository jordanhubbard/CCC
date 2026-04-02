# Episodic Memory System

Two modules that give agents structured recall across sessions.

## Modules

### `rcc/memory/episodic.mjs` — EpisodicMemory

Stores two types of records:

**ActivityDigest** — a bounded work chunk. Create one when:
- A discrete task completes (`boundarySignal: "task_complete"`)
- The topic shifts significantly (`"topic_shift"`)
- A long pause occurs (`"time_threshold"`)
- The session ends (`"session_end"`)

**SessionSynthesis** — an end-of-session roll-up. Create one summary per day that aggregates all digests from that session.

#### Storage layout

```
~/.rcc/memory/episodic/
  digests/
    YYYY-MM-DD/
      digest-<timestamp>.json   ← one per work chunk
  synthesis/
    YYYY-MM-DD.json             ← one per session day
```

#### Exported functions

| Function | Description |
|---|---|
| `saveDigest(digest)` | Write an ActivityDigest to disk |
| `saveSynthesis(synthesis)` | Write a SessionSynthesis to disk |
| `listDigests(date?)` | List digests for a date (default today) |
| `getDigest(id)` | Read one digest by ID |
| `listSyntheses(limit?)` | List recent syntheses, newest first (default 7) |
| `getSynthesis(date)` | Read synthesis for a date |
| `recentDigests(hours?)` | Digests from last N hours (default 24) |

#### ActivityDigest schema

```js
{
  id: "digest-1743100000000",   // digest-<Date.now()>
  agentName: "rocky",
  startTime: "2026-03-27T10:00:00Z",
  endTime:   "2026-03-27T11:30:00Z",
  summary: "Implemented bootstrap safety checks and fixed auth multi-token support.",
  actionsToken: ["committed f615e69", "filed wq-BOOT-123"],
  entitiesCreated: ["rcc/scripts/bootstrap.sh"],
  learnings: ["Always validate token format before writing .env"],
  significance: 7,              // 1 (routine) – 10 (critical)
  themes: ["bootstrap", "auth"],
  boundarySignal: "task_complete"
}
```

#### SessionSynthesis schema

```js
{
  id: "synthesis-1743100000000",
  agentName: "rocky",
  sessionDate: "2026-03-27T00:00:00Z",
  keyOutcomes: ["Bootstrap hardened", "ClawBus RCE shipped"],
  allLearnings: ["Token validation is critical at .env write time"],
  significance: 8,
  themes: ["bootstrap", "bus", "infra"],
  followUp: ["Review HMAC key rotation policy"]
}
```

---

### `rcc/memory/assembler.mjs` — WorkingMemoryAssembler

Token-budgeted context assembly. Searches multiple memory sources and returns a single formatted string ready for prompt injection.

#### Default token budget

| Section | Tokens |
|---|---|
| `knowledge` | 800 — semantic/Milvus results |
| `episodes` | 400 — recent digests |
| `relationships` | 300 — entity/person graph (future) |
| **total** | **2000** |

#### Tiered rendering

Items are rendered in three tiers to stay within budget:

1. **Top 3** — full content
2. **Results 4–10** — first sentence + relevance score
3. **Remainder** — name-only list

#### Exported functions

| Function | Description |
|---|---|
| `assemble(query, opts?)` | Main entry point — returns `{ knowledge, episodes, relationships, totalTokens, summary }` |
| `estimateTokens(text)` | Rough token count (chars / 4) |
| `budgetSection(items, tokenBudget)` | Apply tiered rendering to a list of items |

#### `assemble()` options

```js
await assemble("bootstrap failures", {
  budget: { episodes: 600, knowledge: 1000, total: 2500 }, // override defaults
  episodeHours: 48,   // how far back to look for digests (default 24)
  vectorLimit: 15,    // max Milvus results (default 10)
});
```

#### Return value

```js
{
  knowledge: "...formatted Milvus hits...",
  episodes:  "...formatted digest entries...",
  relationships: "",       // empty until entity graph is built
  totalTokens: 312,
  summary: "<!-- WorkingMemory: 312 tokens -->\n## Recent Activity...\n..."
}
```

The `summary` field is ready to prepend directly to an LLM prompt.

---

## How agents should use these modules

### At task completion — save a digest

```js
import { saveDigest } from '../memory/episodic.mjs';

await saveDigest({
  id: `digest-${Date.now()}`,
  agentName: process.env.AGENT_NAME || 'rocky',
  startTime: taskStartTime.toISOString(),
  endTime:   new Date().toISOString(),
  summary: "Fixed bootstrap.sh token validation bug.",
  actionsToken: [`committed ${commitHash}`],
  entitiesCreated: [],
  learnings: ["Check for slash-prefixed keys before .env write"],
  significance: 6,
  themes: ["bootstrap", "fix"],
  boundarySignal: "task_complete",
});
```

### At session start — assemble context

```js
import { assemble } from '../memory/assembler.mjs';

const ctx = await assemble(userQuery);
const prompt = `${ctx.summary}\n\n---\n\n${userQuery}`;
```

### At session end — save synthesis

```js
import { listDigests, saveSynthesis } from '../memory/episodic.mjs';

const todayDigests = await listDigests(); // today by default
const allLearnings = todayDigests.flatMap(d => d.learnings);

await saveSynthesis({
  id: `synthesis-${Date.now()}`,
  agentName: process.env.AGENT_NAME || 'rocky',
  sessionDate: new Date().toISOString(),
  keyOutcomes: todayDigests.map(d => d.summary),
  allLearnings,
  significance: Math.max(...todayDigests.map(d => d.significance)),
  themes: [...new Set(todayDigests.flatMap(d => d.themes))],
  followUp: [],
});
```

---

## Relationship to vector memory

The assembler queries **Milvus** (`rcc_memory` collection) for semantic search results and merges them with episodic digests. If Milvus is unavailable the assembler degrades gracefully — episodic context is always available from local disk.

To populate the vector store so the assembler can find relevant snippets, use `rememberSnippet()` from `rcc/vector/index.mjs`:

```js
import { rememberSnippet } from '../vector/index.mjs';

await rememberSnippet(
  "Bootstrap .env writer must skip slash-keyed secrets",
  "bootstrap-fix-f615e69",
  "rocky"
);
```
