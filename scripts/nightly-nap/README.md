# Nightly Holographic → Qdrant Nap

Each agent takes a brief **nightly nap** once per night, during which it:

1. **Sifts** its short-term holographic memories for the day:
   - ideas, wisdom, experience, inspiration
   - project and task updates from all sessions
2. **Summarises** the sift into a structured document
3. **Writes** the summary into the hub's long-term Qdrant collection
   (`agent_long_term_memory`) with full provenance metadata

---

## Files

| File | Purpose |
|---|---|
| `nightly_nap.py` | Core nap logic — sift, summarise, write to Qdrant |
| `schedule_nap_cron.py` | Install per-agent cron entries with staggered offsets |
| `audit_nap_fleet.py` | **Fleet audit** — verify the copy is scheduled, firing, and producing Qdrant content per agent |
| `query_nap_summaries.py` | Query the long-term store by agent, date, or semantic search |
| `README.md` | This file |

---

## Time Reference

**Hub time = UTC.**  All agents target UTC midnight as the reference so the
fleet is synchronised on the same calendar day regardless of each box's
local timezone.  Local wall-clock time is never used for scheduling.

---

## Staggered Scheduling

Each agent gets a **deterministic, stable offset** after UTC midnight:

```
offset_minutes = SHA-256(agent_name) mod 240
```

The window is **00:00–04:00 UTC**.  The `schedule_nap_cron.py` installer
checks the whole fleet for collisions and bumps any conflict by 15-minute
steps to enforce the **≥ 15-minute floor** between any two agents.

### Current fleet offsets

```
Agent          Offset (min)   UTC time
----------------------------------------
rocky          ~  0–239       see `--fleet` output
natasha        ~  0–239       see `--fleet` output
bullwinkle     ~  0–239       see `--fleet` output
boris          ~  0–239       see `--fleet` output
```

Run `python3 schedule_nap_cron.py --fleet` to see the exact computed values.

---

## Qdrant Collection

**Collection:** `agent_long_term_memory`  
**Dimensions:** 3072 (text-embedding-3-large via tokenhub)  
**Distance:** Cosine

### Payload schema

| Field | Type | Description |
|---|---|---|
| `chunk_type` | string | Always `"nightly_nap"` |
| `agent` | string | Agent name (e.g. `"rocky"`) |
| `date` | string | ISO date of the nap (`"YYYY-MM-DD"`) |
| `date_range` | string | Same as `date` for daily summaries |
| `session_ids` | list[str] | All session IDs sifted that night |
| `tags` | list[str] | Inferred content tags (ideas, wisdom, …) |
| `sift_categories` | list[str] | Fixed set of sift categories |
| `source_projects` | list[str] | Project names mentioned in the sift |
| `source_tasks` | list[str] | Task names/IDs mentioned in the sift |
| `source_count` | object | `{daily_notes, sessions, memory_snippets}` |
| `chunk_index` | int | Chunk number within this nap's summary |
| `total_chunks` | int | Total chunks for this nap |
| `created_at` | string | ISO timestamp when the point was written |
| `text` | string | The embedded text chunk |

---

## Agent Availability During Nap

While `nightly_nap.py` runs it writes a marker file:

```
~/.hermes/.nap_active_<agent_name>
```

Dispatch and fleet monitors can check `is_napping(agent_name)` to know
the agent is temporarily unreachable.  The marker is removed automatically
when the nap completes (a few minutes at most).

---

## Fleet Audit (Verification)

Use `audit_nap_fleet.py` to verify the copy is scheduled, firing, and
producing Qdrant content for all four agents.  This is the required evidence
for CCC-P0 (per-agent short-term .md → holographic storage verification).

### Evidence criteria

| # | Criterion | How checked |
|---|---|---|
| 1 | **Content** | Qdrant `agent_long_term_memory` has nightly_nap points for the agent within the last 7 days |
| 2 | **Scheduled** | `# acc-nightly-nap` cron tag present in crontab, or nap state file exists (proxy for install on agent's own box) |
| 3 | **Firing** | `~/.hermes/scripts/.nap_state_<agent>.json` → `last_successful_date` within the last 2 days |

### Quick audit (all agents)

```bash
python3 scripts/nightly-nap/audit_nap_fleet.py
```

### Audit with content snippets

```bash
python3 scripts/nightly-nap/audit_nap_fleet.py --show-content
```

### Machine-readable output (for CI / dashboards)

```bash
python3 scripts/nightly-nap/audit_nap_fleet.py --json
```

### Check without Qdrant (local state files only)

```bash
python3 scripts/nightly-nap/audit_nap_fleet.py --local-only
```

### Example output

```
======================================================================
  Fleet Audit: per-agent short-term .md → holographic storage copy
  Generated : 2026-05-20 03:14 UTC
======================================================================

  ✅ ROCKY          [PASS]
     ──────────────────────────────────────────────────
     ✅ Scheduled : cron installed, fires at 01:47 UTC daily
     ✅ Firing    : last successful nap: 2026-05-19 (1d ago), result=ok
     ✅ Content   : 8 point(s) found, most recent nap date: 2026-05-19

  ✅ NATASHA        [PASS]
     ──────────────────────────────────────────────────
     ✅ Scheduled : cron installed, fires at 00:23 UTC daily
     ✅ Firing    : last successful nap: 2026-05-19 (1d ago), result=ok
     ✅ Content   : 6 point(s) found, most recent nap date: 2026-05-19

  ⚠  BULLWINKLE     [WARN]
     ──────────────────────────────────────────────────
     ✅ Scheduled : no live cron (different box?), but state file exists
     ⚠  Firing    : last successful nap: 2026-05-16 (4d ago) — STALE
     ❌ Content   : no nightly_nap points found in the last 7 days

  ❌ BORIS          [FAIL]
     ──────────────────────────────────────────────────
     ❌ Scheduled : no cron entry and no nap state file found
     ❌ Firing    : no successful nap recorded in state file
     ❌ Content   : no nightly_nap points found in the last 7 days

----------------------------------------------------------------------
  Summary: 2 pass  |  1 warn  |  1 fail  (of 4 agents)

  Triage:
    bullwinkle:
      → Not firing recently. Check ~/.hermes/logs/nightly_nap.log on bullwinkle's box.
        Manual run: AGENT_NAME=bullwinkle python3 scripts/nightly-nap/nightly_nap.py --dry-run
    boris:
      → Not scheduled. Fix: AGENT_NAME=boris python3 scripts/nightly-nap/schedule_nap_cron.py --install
      → Not firing recently. Check ~/.hermes/logs/nightly_nap.log on boris's box.
```

### Triage by outcome

| Outcome | Action |
|---|---|
| All 4 agents PASS | Mark CCC-P0 verified, attach `--json` output as audit artifact |
| Scheduled but stale (WARN) | Check `~/.hermes/logs/nightly_nap.log` on the agent's box; re-run manually to clear |
| Not scheduled (FAIL) | Run `AGENT_NAME=<name> python3 scripts/nightly-nap/schedule_nap_cron.py --install` on the agent's box |
| No Qdrant content | Confirm Qdrant is running and reachable; run `nightly_nap.py` manually; check tokenhub embeddings |

---

## Quick Start

### 1. Dry-run (no Qdrant writes)

```bash
export AGENT_NAME=rocky
python3 scripts/nightly-nap/nightly_nap.py --dry-run
```

### 2. Run once manually

```bash
export AGENT_NAME=rocky
python3 scripts/nightly-nap/nightly_nap.py
```

Confirm entries in Qdrant:

```bash
python3 scripts/nightly-nap/query_nap_summaries.py --agent rocky --stats
python3 scripts/nightly-nap/query_nap_summaries.py --agent rocky --date $(date -u +%Y-%m-%d)
```

### 3. Install cron on an agent box

```bash
export AGENT_NAME=rocky
python3 scripts/nightly-nap/schedule_nap_cron.py --install
```

Verify:

```bash
python3 scripts/nightly-nap/schedule_nap_cron.py --list
```

### 4. Enable on all 4 agents

Run on each box:
```bash
# On rocky's box:
AGENT_NAME=rocky python3 scripts/nightly-nap/schedule_nap_cron.py --install

# On natasha's box:
AGENT_NAME=natasha python3 scripts/nightly-nap/schedule_nap_cron.py --install

# On bullwinkle's box:
AGENT_NAME=bullwinkle python3 scripts/nightly-nap/schedule_nap_cron.py --install

# On boris's box:
AGENT_NAME=boris python3 scripts/nightly-nap/schedule_nap_cron.py --install
```

Check the fleet schedule:
```bash
python3 scripts/nightly-nap/schedule_nap_cron.py --fleet
```

### 5. Query long-term store

```bash
# All naps for rocky:
python3 scripts/nightly-nap/query_nap_summaries.py --agent rocky

# Rocky's nap for a specific date:
python3 scripts/nightly-nap/query_nap_summaries.py --agent rocky --date 2026-05-01

# Semantic search across all agents' nap summaries:
python3 scripts/nightly-nap/query_nap_summaries.py --query "ideas about qdrant and memory"

# Raw JSON output for piping:
python3 scripts/nightly-nap/query_nap_summaries.py --agent natasha --date 2026-05-01 --json
```

### 6. Run fleet audit

```bash
# Full audit with Qdrant content verification:
python3 scripts/nightly-nap/audit_nap_fleet.py

# Offline check (local state files only, no Qdrant):
python3 scripts/nightly-nap/audit_nap_fleet.py --local-only

# JSON output for CI / ticket attachment:
python3 scripts/nightly-nap/audit_nap_fleet.py --json > /tmp/nap-audit-$(date -u +%Y%m%d).json
```

---

## Deployment — setup-node.sh integration

`deploy/setup-node.sh` automatically calls `schedule_nap_cron.py --install`
during node bootstrap if `AGENT_NAME` is set.  No manual step needed for new
agent deployments.

The nightly nap cron entry is deliberately **not** in `deploy/crontab-acc.txt`
because it requires a per-agent staggered UTC offset — a static fragment cannot
encode that.  `schedule_nap_cron.py --install` handles it correctly.

---

## Idempotence

`nightly_nap.py` writes a state file at:

```
~/.hermes/scripts/.nap_state_<agent_name>.json
```

If the nap already completed for today (`last_successful_date` matches),
it exits with code 2 (skip).  Pass `--force` to override.

Qdrant points use **deterministic IDs** (`hash(nightly_nap, agent, date, chunk_index)`)
so re-running the nap for the same date is a safe upsert, not a duplicate.

---

## Dependencies

- Python 3.11+ (stdlib only — no pip packages)
- `scripts/qdrant-python/qdrant_common.py` (sibling directory)
- Qdrant accessible at `QDRANT_URL` (default: `http://localhost:6333`)
- Tokenhub accessible at `TOKENHUB_URL` (default: `http://localhost:8090`)
- Credentials in `~/.hermes/.env` or `~/.ccc/.env`

---

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `AGENT_NAME` | *(required)* | Agent identity |
| `QDRANT_URL` | `http://localhost:6333` | Qdrant base URL |
| `TOKENHUB_URL` | `http://localhost:8090` | Tokenhub base URL |
| `TOKENHUB_API_KEY` | from env file | Tokenhub bearer key |
| `HERMES_HOME` | `~/.hermes` | Hermes home directory |
| `NAP_DRY_RUN` | `0` | Set to `1` to skip Qdrant writes |
| `NAP_STATE_FILE` | auto | Override nap state JSON path |
