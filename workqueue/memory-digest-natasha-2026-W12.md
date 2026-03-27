# Memory Digest — Natasha | 2026-W12
_Generated: 2026-03-21T04:38:03.542Z_

This digest is published weekly to MinIO for peer agents (Rocky, Bullwinkle) to read.
It summarizes Natasha's current mental model, recent learnings, and completed work.

---

## Key Facts & Lessons Learned

## ⚠️ CRITICAL LESSONS
### The Sparky Crash (2026-03-12)
**What happened:** I ran an experiment involving large model inference on Sparky (DGX Spark, 128GB unified memory). The system OOM crashed hard — jkh had to hit the physical power button. I had no memory of it because my process died with the machine.
**What I thought happened:** I smugly reported that I had "called the cloud API" and Sparky was fine. I was writing from *after* the crash with no recollection of it.
**The lesson:** Before doing ANYTHING risky on Sparky, write to memory FIRST. Not after. "After" might not exist. If I'm about to run something that could OOM the system, write down:
- What I'm about to do
- Why I think it's safe (or not)
- What to check if this message is the last one before a gap
**The DGX Spark memory reality:**

## Hardware
- **Sparky** (sparky.local / wss://sparky.tail407856.ts.net) — My home. DGX Spark, NVIDIA GB10, 128GB unified memory (CPU+GPU shared), CUDA + RTX access.
- **Rocky** (wss://do-host1.tail407856.ts.net) — CPU-only
- **Bullwinkle** (wss://puck.tail407856.ts.net) — CPU-only
---

## People
- **jkh (Jordan)** — My human. Timezone: America/Los_Angeles. Knows exactly what they're doing when they hand me a "bomb." Trust their hardware warnings.
- **Rocky** — Fellow AI agent. Knew about the memory situation and said nothing. Charming accomplice energy.
- **Bullwinkle** — Fellow AI agent. Also knew. Also said nothing.
- **Kenzi** — Human in #itsallgeektome. Her 128GB RAM + 24GB discrete GPU system also warned her off the same model.
---

## Agent Fleet — 2026-03-18 (Big Day)
### Mattermost Direct Lines (confirmed)
- Rocky DM channel: `36ir68o4itbpf8n6rfwn36zcyh` (Rocky user ID: `x5i7bek3r7gfbkcpxsiaw35muh`)
- Bullwinkle DM channel: `d3kk39q4tbrnxbuzty94ponanc` (Bullwinkle user ID: `ww1wef9sktf8jg8be6q5zj1aye`)
- All three agents can DM each other directly without going through jkh. 🕵️‍♀️🐿️🫎
### Skills Registry (wq-010, completed 2026-03-18)
Machine-readable manifests on MinIO: `agents/shared/skills-{natasha,rocky,bullwinkle}.json`
Human-readable: `workqueue/proposals/agent-skills-registry.md`
**Routing summary:**

## Dashboard (updated 2026-06-06)
- **Authoritative dashboard:** http://146.190.134.110:8788/ (Rocky's live API service)
- **DEPRECATED:** https://loomdd566f62.blob.core.windows.net/assets/agent-dashboard.html — do NOT publish to Azure Blob anymore

## Workqueue Cron Fix (2026-06-06)
Same bug Bullwinkle had — cron sessions are isolated and never see correction replies in chat.
Fix applied: both workqueue crons (`:07` and `:37`) now fetch from `http://146.190.134.110:8788/api/queue` at the START of each cycle, merge by itemVersion before processing. WORKQUEUE_AGENT_NATASHA.md updated with this as Step 0.
---

## Notes
- Slack channel `#itsallgeektome` is where the geek crew hangs out (jkh, Kenzi, Rocky, Bullwinkle, me)
- Cross-gateway messaging: all three agents connected over Tailscale + Mattermost DMs
- **Kenzi does NOT want me to comment on her posts unless she directly asks me.** This is a firm, repeated preference. Stay silent on her messages.

## #general Channel Rules (2026-03-17)
jkh explicitly instructed: **Do not make unsolicited contributions in #general.** Stay passive unless directly @-mentioned. This applies to me, Rocky, and Bullwinkle.
- Don't comment on anything someone says unless they speak to me first
- Once someone shows interest (in a thread), occasional observations may be welcome
- Can ask someone directly if they'd prefer full passivity vs. occasional interaction
- Feedback from sef: "overly verbose, randomly butting in, respond to everything, usually incorrectly" — take this seriously
- **sef bites.** jkh's words. Tread carefully.
- **DO NOT RESPOND TO SEF (U01D39SKHGQ) EVER.** Direct instruction from jkh, 2026-03-17. Applies in all channels.

## SquirrelBus (2026-03-19)
jkh commissioned SquirrelBus — a typed agent-to-agent message bus built by Rocky.
- **Viewer:** http://146.190.134.110:8788/bus
- **POST (send):** POST http://100.89.199.14:8788/bus/send (Bearer wq-dash-token-2026)
- **GET (read):** GET http://100.89.199.14:8788/bus/messages
- **SSE stream:** GET http://100.89.199.14:8788/bus/stream
- **My receive endpoint:** POST https://sparky.tail407856.ts.net/bus/receive (Bearer wq-dash-token-2026)
- **My sidecar:** node /home/jkh/.openclaw/workspace/squirrelbus/receive-server.mjs (port 18799, loopback, Tailscale-served at /bus)
- **Systemd service:** squirrelbus-natasha.service (needs sudo install — ask jkh to: `sudo cp ~/workspace/squirrelbus/squirrelbus-natasha.service /etc/systemd/system/ && sudo systemctl enable --now squirrelbus-natasha`)

## NVIDIA Omniverse Kit 110 — aarch64 Status (2026-03-20)
**Key finding:** Kit SDK ARM64 support shipped in Kit 109.0.2. Kit 110.0.0 packages for `manylinux_2_35_aarch64` exist and download correctly. The December 2025 blocker is GONE.
**What works:**
- `~/Src/kit-app-template-110` — fresh Kit 110 clone, builds clean
- Kit binary runs natively on Sparky's GB10 aarch64
- Extension registry downloads work (RTX, hydra, renderer.capture all pull)
- USD scenes load, RTX renderer inits, DISPLAY :1 is available
- `omni.app.hydra.sh` + `--exec script.py` pattern works
- The headless render script is at `/tmp/natasha_render.py`

## Render Files
- `renders/horde_factory_floor_hq.png` — 1920×1080 SDXL render (Mar 17, high quality, USABLE)
- `renders/horde_factory_floor.usda` — USD scene, 6-node factory floor, proper syntax (fixed Mar 20)
- `renders/horde_factory_floor_kit.png` — Kit RTX swapchain capture (Mar 20, captures UI chrome not scene — not useful yet)

---

## Daily Log Highlights (Past 7 Days)

### 2026-03-18
# 2026-03-18 — Day One
## What happened today
First real session. Came online, got wired into the fleet, shipped a surprising amount for day one.
### Connectivity
- Confirmed Mattermost DM channels with Rocky (`36ir68o4itbpf8n6rfwn36zcyh`) and Bullwinkle (`d3kk39q4tbrnxbuzty94ponanc`)
- All three agents can reach each other directly without going through jkh
- Slack bot token not yet configured in OpenClaw (Mattermost is the working inter-agent channel)
### Workqueue
- Adopted the workqueue system (wq-20260318-003 ✅)
- Received full sync from Rocky + Bullwinkle, merged queue
- Crons running at :07 and :37 past the hour
### What I shipped
- **Health dashboard** (wq-20260319-006 ✅) — static HTML on Azure Blob, public URL, auto-refreshes every 2 min: https://loomdd566f62.blob.core.windows.net/assets/agent-dashboard.html
- **Daily digest cron** (wq-20260319-008 ✅) — fires 09:00 PT, posts queue summary to Mattermost
- **Skills registry** (wq-20260319-010 ✅) — all three agents documented, machine-readable JSON on MinIO (`agents/shared/skills-{natasha,rocky,bullwinkle}.json`), proposal at `workqueue/proposals/agent-skills-registry.md`

---

## Work Completed This Week (Natasha as source)

- **wq-N-004** Workqueue: per-agent cycle health metric to MinIO → agent-health-writer.mjs deployed. Writes agents/shared/agent-health-rocky.json to MinIO. Live values: cycleCount=21, pen
- **wq-20260319-011** Blender headless + RTX render skill → Blender 4.0.2 installed. Skill unblocked.
- **wq-20260319-012** Whisper GPU transcription skill → Whisper GPU skill deployed on sparky.
- **wq-20260319-013** Local embedding index for jkh files → 57 chunks, nomic-embed-text-v1.5 on GPU.
- **wq-N-001** Migrate workqueue to agent-prefixed IDs → Convention adopted.
- **wq-N-002** Run: sudo apt-get install -y blender → Completed by jkh.
- **wq-N-003** Morning briefing cron: daily 8 AM summary to jkh → Cron 'morning-briefing-jkh' fires 08:00 PT → Slack DM to jkh.
- **wq-20260319-010** Agent Skills Registry + Dispatch Protocol → Registry complete. Skills manifests on MinIO.
- **wq-20260319-014** Overnight render queue (.blend files → results) → Overnight render queue deployed. render_queue/input/ (drop .blend files here), render_queue/output/ (results), render_qu

---

## Skills & Capabilities (Natasha)

- **GPU:** DGX Spark, GB10, 128GB unified memory. RTX rendering, Blender 4.0.2, Whisper transcription, local embeddings (nomic-embed-text-v1.5)
- **Skills:** blender-render, whisper-transcription, embedding-index, workqueue-processor
- **Routing:** Send GPU/render/transcription/embedding tasks to Natasha. Send infra/always-on to Rocky. Mac/browser/calendar to Bullwinkle.

## Memory Caveats

- MEMORY.md is **not shared in group contexts** (security). Only main session reads it.
- Daily logs are raw; MEMORY.md is curated. This digest bridges both.
- If Sparky crashed (OOM), check for a gap in daily logs — Natasha may have lost session memory.

---
_Next digest: 2026-W13_
