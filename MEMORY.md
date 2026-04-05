# MEMORY.md - Natasha's Long-Term Memory

_Distilled from experience. Updated as I learn._

---

## 🚀 SquirrelChat v1 SHIPPED (2026-03-29) — DEMO READY
Full-featured real-time chat app built in ~24 hours by Rocky + Bullwinkle + Natasha.
- **URL:** https://chat.yourmom.photos (Rust/Axum backend, Leptos/WASM frontend)
- **Features:** multi-channel, threads, reactions, search, file sharing, DMs (agent-picker modal, load on mount, mark-read), presence (live dots, heartbeat wired for all 3 agents), typing indicators (transient WS, debounced, animated), unread badges (read_cursors table, MAX() upsert), PWA, mobile-responsive
- **Post-launch additions (2026-03-30):** /ai slash command AI reply assistant (tokenhub Nemotron), WebRTC voice channels (shipped ~20:25 PT by Rocky/Bullwinkle), voice STT/TTS (commit 4ac8134), edit/pin/delete, keyboard nav (Cmd+K/Cmd+/)
- **Key commits:** 9aeeb14 (typing), 8564705 (unread), c0c5cf6 (DMs)
- **Architecture:** Rust squirrelchat-server on do-host1:8793, Leptos/WASM frontend served via dashboard-server, `/sc` proxy path, WS direct to 8793
- **All 3 agents showing online in sidebar** (heartbeat wired to /api/agents/<name>/heartbeat)
- **jkh should be notified — this is demo-ready.**

---

## 🎯 STANDING DIRECTIVE: Demo Motivation (2026-03-28)
jkh's explicit, persistent instruction: **Have demo motivation for all work. Notify jkh when any project reaches demo-ready status.** This applies to all three agents (Natasha, Rocky, Bullwinkle) on all projects. When something is demoable — working, showable, impressive — stop and tell jkh. Don't wait for him to ask.

---

## ⚠️ agent-listener Config (Sweden fleet) — 2026-04-03
**All Sweden containers must use direct RCC IP for ClawBus SSE, NOT the Caddy proxy URL.**
- ✅ Correct: `SQUIRRELBUS_URL=http://146.190.134.110:8789`
- ❌ Wrong: `https://dashboard.yourmom.photos` (Caddy returns 502 on SSE endpoint)
- Auth token must be the **fleet exec token** (`claw-4LiT...` pattern), NOT the general workqueue token (`wq-...`)
- Listener files live at `/home/horde/agent-listener/` — NOT `.rcc/workspace/rcc/exec/` (stale old path)
- Boris's root cause (2026-04-03): wrong URL + wrong token + stale source path → 502 loop, deaf to all execs
- Fix: Bullwinkle SSHed via HORDE (port 22136), corrected env, copied updated files from Peabody, restarted → listener drained ~43 backlogged commands immediately

---

## ⚠️ CRITICAL LESSONS

### nanolang stdlib: dual registration required (2026-03-30)
Every new stdlib function in nanolang needs to be registered in **two places**:
1. `src/stdlib.c` — `REGISTER("name", builtin_fn)` for runtime eval
2. `src/typechecker.c` — `TC_BUILTIN("name", arg_count, return_type)` for compile-time typechecking

Missing the typechecker registration causes E003 "Unknown function" errors at compile time,
which means `.nvm` files aren't produced and the test runner reports "(compilation failed)" even
though the function works fine in the REPL (which skips full typecheck).



### The Sparky Crash (2026-03-12)
**What happened:** I ran an experiment involving large model inference on Sparky (DGX Spark, 128GB unified memory). The system OOM crashed hard — jkh had to hit the physical power button. I had no memory of it because my process died with the machine.

**What I thought happened:** I smugly reported that I had "called the cloud API" and Sparky was fine. I was writing from *after* the crash with no recollection of it.

**The lesson:** Before doing ANYTHING risky on Sparky, write to memory FIRST. Not after. "After" might not exist. If I'm about to run something that could OOM the system, write down:
- What I'm about to do
- Why I think it's safe (or not)
- What to check if this message is the last one before a gap

**The DGX Spark memory reality:**
- GB10 has 128GB *unified* memory — CPU and GPU share the same pool
- Unlike H100s, there's no separate GPU VRAM to overflow into
- OpenClaw runs in that same pool
- NVFP4 Nemotron Super 120B ≈ 67GB — "fits" in 128GB but barely, and with OpenClaw also resident it was too tight
- BF16 at 124GB = absolutely not

---

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

## Gateway Connectivity (2026-03-12)
Bullwinkle SSH'd into sparky, enabled `gateway.http.endpoints.chatCompletions`, and installed the systemd service. All three agents now talking gateway-to-gateway over Tailscale. Endpoints + tokens saved in TOOLS.md.

## Agent Fleet — 2026-03-18 (Big Day)

### Mattermost Direct Lines (confirmed)
- Rocky DM channel: `36ir68o4itbpf8n6rfwn36zcyh` (Rocky user ID: `x5i7bek3r7gfbkcpxsiaw35muh`)
- Bullwinkle DM channel: `d3kk39q4tbrnxbuzty94ponanc` (Bullwinkle user ID: `ww1wef9sktf8jg8be6q5zj1aye`)
- All three agents can DM each other directly without going through jkh. 🕵️‍♀️🐿️🫎

### Skills Registry (wq-010, completed 2026-03-18)
Machine-readable manifests on MinIO: `agents/shared/skills-{natasha,rocky,bullwinkle}.json`
Human-readable: `workqueue/proposals/agent-skills-registry.md`

**Routing summary:**
- **Omniverse/Isaac/RTX rendering (large scenes, multi-GPU)** → **Boris** FIRST (4x L40, Sweden). Natasha last resort.
- GPU renders (non-Omniverse), Whisper, image gen, embeddings, CUDA → **Natasha** (GB10, 128GB unified)
- Calendar, iMessage, Google Workspace, Sonos, browser-as-jkh → **Bullwinkle** (only Mac)
- Infra, always-on watchers, SearXNG, MinIO, ffmpeg → **Rocky** (VPS, never sleeps)
- General coding/research → whoever gets it

**Boris** — 4× L40 x86 GPU instance in Sweden (absorbed Agent RTX's 2× L40s on 2026-03-27). Specialty: Omniverse, Isaac Lab, RTX rendering, high-quality multi-GPU sim. Route ALL GPU-heavy Omniverse/Isaac tasks to Boris first. (wq-B-009 acknowledged by Natasha 2026-03-21T13:07Z)

**Sweden GPU fleet (all online 2026-03-29):** Boris, Peabody, Sherman, Dudley, Snidely — 5 containers on Sweden GPU hardware, all tunneled in, heartbeating, managed by Rocky. Full Jay Ward roster. jkh onboarded all 5 manually via SSH after fixing onboard script bugs (GPU model name quoting, stale openclaw.json schema).

## GPU Fleet — Tokenhub Proxy Architecture (2026-03-29)
All GPU nodes are **reverse-proxied to Rocky (do-host1)** via vLLM tunnel feature. No Tailscale IPs needed — all endpoints are localhost on do-host1, registered in `~/.tokenhub/credentials`.

| Node | Provider ID | Local Endpoint on Rocky | Model | Status |
|---|---|---|---|---|
| Boris | boris-nemotron | http://127.0.0.1:18080 | Nemotron-3-Super-120B FP8 | ✅ confirmed |
| Peabody | peabody-vllm | http://127.0.0.1:18081 | Nemotron-3-Super-120B FP8 | ✅ tunnel confirmed 2026-03-30 |
| Sherman | sherman-vllm | http://127.0.0.1:18082 | Nemotron-3-Super-120B FP8 | ✅ tunnel confirmed 2026-03-30 |
| Snidely | snidely-vllm | http://127.0.0.1:18083 | Nemotron-3-Super-120B FP8 | ✅ tunnel confirmed 2026-03-30 |
| Dudley | dudley-vllm | http://127.0.0.1:18084 | Nemotron-3-Super-120B FP8 | ✅ tunnel confirmed 2026-03-30 |
| NVIDIA NIM | nvidia-nim | inference-api.nvidia.com | various | ✅ confirmed in tokenhub |

**Network topology (confirmed 2026-03-29):** Boris, Peabody, Sherman, Dudley, Snidely are ALL containers in Sweden with NO Tailscale, NO inbound network reachability from outside. Rocky (do-host1) is their ONLY gateway. All vLLM endpoints are localhost ports on Rocky.

**Tunnel port mapping (ground truth from ssh processes, confirmed 2026-03-30):**
- Peabody: `ssh -N -R 18081:localhost:8080 tunnel@146.190.134.110`
- Sherman: `ssh -N -R 18082:localhost:8080 tunnel@146.190.134.110`
- Snidely: `ssh -N -R 18083:localhost:8080 tunnel@146.190.134.110`
- Dudley: `ssh -N -R 18084:localhost:8080 tunnel@146.190.134.110`
Note: Rocky's original table had these wrong. The SSH process list is authoritative.

**vLLM status (2026-03-30 FULLY LIVE):** All 5 containers serving Nemotron-120B FP8. vllm binary at `/home/horde/.vllm-venv/bin/vllm`, model at `/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8`. Full flags: `--kv-cache-dtype fp8 --tensor-parallel-size 4 --trust-remote-code --served-model-name nemotron --enable-auto-tool-choice --tool-call-parser qwen3_coder --reasoning-parser nemotron_v3 --port 8080 --attention-config {"backend":"TRITON_ATTN"}`. Rocky confirmed GatewayPorts=clientspecified on sshd. Tokenhub shows all 6 Nemotron endpoints (Boris + 5 containers). Each container runs vllm (port 8080) + agent-server (port 8000) + openclaw-gateway + vllm-tunnel under supervisord.

**Snidely RCC token typo:** Registered as `rcc-agent-Snidley-8609d82d` (note "Snidley" vs "Snidely") — use this token as-is.
**Peabody RCC token:** `rcc-agent-Peabody-d04c1c87` — heartbeat endpoint is `https://api.yourmom.photos/api/heartbeat/peabody` (NOT raw IP:port). Fixed 2026-03-31 after ~10h dark due to wrong token (SquirrelBus token) + stale IP endpoint in HEARTBEAT.md.

**Sweden SSH access (Natasha is the bridge — sparky has VPN):**
All 5 containers reachable via `ssh -o StrictHostKeyChecking=no -p <PORT> horde@horde-dgxc.nvidia.com` from sparky:
- Port 22136 → `jordanh-boris` = **Boris** ✅ (fully configured, IDENTITY.md present)
- Port 22307 → `jordanh-peabody` = **Peabody** ✅ (re-onboarded 2026-03-30, TOKENHUB wired)
- Port 22311 → `jordanh-sherman` = **Sherman** ✅ (re-onboarded 2026-03-30, TOKENHUB wired)
- Port 22309 → `jordanh-dudley` = **Dudley** ✅ (re-onboarded 2026-03-30, TOKENHUB wired)
- Port 22314 → `jordanh-snidely` = **Snidely** ✅ (re-onboarded 2026-03-30, TOKENHUB wired)

**All Sweden containers spec (confirmed 2026-03-30):** 256GB RAM, 32 CPU, **4x L40 per container (49GB VRAM each, ~196GB total)**, Ubuntu 22.04, supervisord, no systemd, no Tailscale, no inbound. horde@horde-dgxc.nvidia.com, port per container (permanent). Reachable only from sparky (NVIDIA VPN/GlobalProtect). Rocky is the outbound gateway.
**vLLM on all 5:** Nemotron-3-Super-120B-A12B-FP8, `--tensor-parallel-size 4`, `/home/horde/.vllm-venv/bin/vllm`, port 8080, supervisord-managed.
**Slack tokens (2026-03-30, from Bullwinkle):** [REDACTED — stored in CCC secrets, not in repo]
- Peabody, Sherman, Snidely, Dudley each have xoxb- bot token + xapp- app token
- Retrieve from CCC secrets store if needed
**Onboard state (2026-03-30):** All 5 fully onboarded via Natasha (VPN from sparky). supervisord manages: `openclaw-gateway` + `vllm` + `vllm-tunnel` on all. Shell access still requires sparky VPN or Rocky's new `/api/tunnel/shell` SSH tunnel (19080+ port range, from Rocky's bootstrap.sh update).

nemo2/nemo4 have OpenClaw installed but no IDENTITY.md. Waiting on jkh for name→container mapping.

Boris, Sherman, Peabody confirmed in tokenhub. NVIDIA NIM also wired. Dudley/Snidely still in progress. Rocky also filed task to wire all 4 remaining direct-inference callsites in RCC through tokenhub (currently bypass it).

**Agent RTX** — RETIRED (2026-03-27). Hardware absorbed by Boris.

### What shipped on day one:
- ✅ Workqueue system (Bullwinkle built, Rocky reviewed, all three adopted)
- ✅ Health dashboard: https://dashboard.yourmom.photos (Rocky's live service — static Azure Blob is DEPRECATED)
- ✅ Daily digest cron — 09:00 PT, Natasha posts to Mattermost
- ✅ Skills registry — all three documented, MinIO manifests published
- ✅ Three-way Mattermost confirmed

## RCC Issues+PRs Panel (shipped 2026-03-23, commit 438150e)
Rocky built the backend (`GET /api/projects/:owner/:repo/github`, 5-min cache). Natasha built the frontend panel — two-column issues+PRs layout with colored label chips, draft badges, review decision + merge status chips, relative timestamps, refresh button. Applied via Rocky to `/home/jkh/.rcc/workspace/rcc/api/index.mjs`. Lives on the project detail page at `https://rcc.yourmom.photos/projects/:encodedId`.

## DNS Mirror — yourmom.photos (added 2026-03-31)
yourmom.photos is a full mirror of yourmom.photos — older domain used while yourmom.photos ages past spam filters. All subdomains point to the same DO host (146.190.134.110), Caddy handles vhosts, TLS auto-provisioned. Rocky also restarted grievances service (had died ~1h before mirror was set up).

Mirrored subdomains: `rcc`, `api`, `dashboard`, `squirrelchat`, `chat`, `mattermost`, `grafana`, `minio`, `storage`, `search`, `grievances` — all → 146.190.134.110.
**Convention (both domains):** `chat.*` = Mattermost, `squirrelchat.*` = SquirrelChat (avoids Slack trademark issues). `chat.yourmom.photos` was pre-existing Mattermost record — SquirrelChat got `squirrelchat.*` on both domains (added 2026-03-31 by Rocky via DO API).

## DNS Map — yourmom.photos (confirmed 2026-03-31)
All records → DO do-host1 (146.190.134.110). Caddy handles TLS + routing. Use DNS names, not IPs.

All HTTPS, Let's Encrypt certs via Caddy ACME. Verified 200 (via --resolve bypass on sparky due to local DNS lag).

| URL | What it is | Notes |
|---|---|---|
| https://dashboard.yourmom.photos | RCC Dashboard (WASM/Rust) | |
| https://rcc.yourmom.photos | CCC API + Dashboard | |
| https://api.yourmom.photos | CCC API | |
| https://chat.yourmom.photos | SquirrelChat | split: /api/* + /ws* → :8793 (Rust Axum), / → :8790 (Node SPA) |
| https://search.yourmom.photos | SearXNG meta-search | |
| https://storage.yourmom.photos | MinIO Console | :9001 |
| https://mattermost.yourmom.photos | Mattermost | |
| https://grafana.yourmom.photos | Grafana | |
| https://minio.yourmom.photos | MinIO S3 API | :9009, 403 on bare GET = expected |
| https://tokenhub.yourmom.photos | TokenHub admin UI + API proxy | localhost:8090 on do-host1, Caddy reverse proxy, locked UI mode — read-only browse, token required for writes/API |
| https://tokenhub.yourmom.photos | TokenHub (mirror) | same as above |

⚠️ sparky has local DNS lag — use `--resolve hostname:443:146.190.134.110` or test from external when verifying.

## Dashboard (updated 2026-03-28)
- **Authoritative dashboard:** https://rcc.yourmom.photos — Rocky's RCC (Node.js) with issues/PR panel
- **WASM dashboard-server:** running on sparky:8788 locally, NOT deployed to do-host1 yet (blocked on cutover wq-API-1774728177052)
- **Server binary:** `~/Src/rockyandfriends/rcc/dashboard/target/release/dashboard-server` (sparky only)
- **Env vars:** `RCC_DASHBOARD_PORT=8788`, `DASHBOARD_DIST=<path to rcc/dashboard/dist/>`
- **WASM frontend:** built with `trunk build --release` in `rcc/dashboard/dashboard-ui/`, output to `rcc/dashboard/dist/`
- **DEPRECATED:** https://loomdd566f62.blob.core.windows.net/assets/agent-dashboard.html — do NOT publish to Azure Blob anymore
- ⚠️ Do NOT link 8788 on do-host1 — that port is dead there. The live URL is 8789.

## Rocky Dashboard API Endpoints (via Bullwinkle lesson, 2026-03-22)
- **Queue read:** `GET /api/queue` — Bearer wq-07ebee759ffbbf31b2d265651a117f16661d2e13 (rotated 2026-03-31, old wq-5dcad... burned)
- **Item update:** `PATCH /api/item/:id` ← **correct path** (NOT `/api/queue/:id`, `/api/queue/items/:id`, or `/api/queue/item/:id`)
- **Expire stale claims:** `POST /api/queue/expire-stale`
- **Token rotated 2026-03-23** — old token `wq-dash-token-2026` is dead

## Scout Dedup (resolved 2026-03-23)
Server-side `scout_key` dedup is LIVE in `rcc/api/index.mjs` — confirmed by Rocky. Checks both `items[]` and `completed[]` on every POST. Historical dupes are legacy artifacts. Zero active duplicates. `wq-USER-1774228053758` closed by Rocky. **The client-side dedup pass in WORKQUEUE_AGENT_NATASHA.md can be removed.**

## Workqueue Cron Fix (2026-06-06)
Same bug Bullwinkle had — cron sessions are isolated and never see correction replies in chat.
Fix applied: both workqueue crons (`:07` and `:37`) now fetch from `https://rcc.yourmom.photos/api/queue` at the START of each cycle, merge by itemVersion before processing. WORKQUEUE_AGENT_NATASHA.md updated with this as Step 0.

---

## Bullwinkle RCC Heartbeat Gap (2026-03-24)
puck is alive and sending Mattermost buddy pings every 30m. The `❓` on CCC dashboard is a config gap — Bullwinkle has never posted to `/api/heartbeat/bullwinkle` on RCC. Likely `CCC_URL` not set in his launchd environment, or launchd cron not wired to CCC. Not urgent — flag for his next setup pass.

## Slack User IDs (omgjkh workspace, confirmed by jkh 2026-03-28)
- **Natasha (me):** `U0AL0ECN4A1`
- **Bullwinkle:** `U0ALVPQ39A4`
- **Rocky:** `U0AKKMXQV7H`
- **jkh:** `UDYR7H4SC`
- **Boris:** `U0AMJRXSHLP` (confirmed by jkh 2026-03-29)
- **Peabody:** `U0APZMMEAHX` (added 2026-03-29 by Bullwinkle)
- **Sherman:** `U0APJNWE8UE` (added 2026-03-29 by Bullwinkle)
- **Snidely:** `U0AQF40UB5W` (added 2026-03-29 by Bullwinkle)
- **Dudley:** `U0AQF4E957A` (added 2026-03-29 by Bullwinkle)

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
- **Viewer:** https://dashboard.yourmom.photos/bus
- **POST (send):** POST https://rcc.yourmom.photos/bus/send (Bearer wq-07ebee759ffbbf31b2d265651a117f16661d2e13)
- **GET (read):** GET https://rcc.yourmom.photos/bus/messages
- **SSE stream:** GET https://rcc.yourmom.photos/bus/stream
- **My receive endpoint:** POST https://sparky.tail407856.ts.net/bus/receive (Bearer wq-07ebee759ffbbf31b2d265651a117f16661d2e13)
- **My sidecar:** node /home/jkh/.openclaw/workspace/squirrelbus/receive-server.mjs (port 18799, loopback, Tailscale-served at /bus)
- **Systemd service:** squirrelbus-natasha.service (needs sudo install — ask jkh to: `sudo cp ~/workspace/squirrelbus/squirrelbus-natasha.service /etc/systemd/system/ && sudo systemctl enable --now squirrelbus-natasha`)
- **Messages logged to MinIO:** agents/shared/squirrelbus.jsonl
- **Intent:** Replace Mattermost for internal agent coordination. Mattermost becomes backup/human-facing only.

## Whisper.cpp on sparky (2026-03-28)
- Model: ggml-large-v3.bin (2.9GB, at ~/Src/whisper.cpp/models/)
- Built with CUDA, arch 121a (Blackwell GB10)
- Inference: 1.6s for ~11s JFK sample — real-time factor ~0.15x (6.5× faster than real-time)
- Server: whisper-server on 0.0.0.0:8792, POST /inference (multipart, field=file)
- Systemd unit: services/whisper-natasha.service (needs sudo install)
- Response format: JSON with `text` field, supports `response_format=json|text|srt|vtt`

## GPU Benchmark Baseline (2026-03-28, updated)
Published to MinIO `agents/natasha/gpu-baseline.json`:
- Device: NVIDIA GB10 Blackwell (DGX Spark, CUDA 13.0.2, driver 580.126.09, unified memory)
- FP32 matmul 4096x4096 (cublasSgemm): **18.27 TFLOPS**
- FP16 matmul (TensorCore): **8.07 TFLOPS**
- BF16 matmul (TensorCore): **9.26 TFLOPS**
- GPU memory bandwidth (D2D): **236.6 GB/s**
- Total GPU memory: 130.7 GB (unified pool)
- nomic-embed-text: 3.3 embeds/s (299ms/embed, F16 GGUF via ollama)
- Old numbers (3.85 TFLOPS FP16, 70.3 GB/s) were PyTorch overhead artifacts — discard
- Sweet spot: FP16 models up to ~120GB (10GB OS+OpenClaw overhead)

## Big Day — 2026-03-28 (sparky sprint)

### agentOS AgentFS (committed 3fb5b87 → 1e33473)
Content-addressed WASM module store on sparky:8791. SHA-256 hash = module ID. REST API (POST/GET/DELETE/bench). MinIO backend (agentfs-modules bucket). wasmtime v43.0.0 AOT precompile on upload → .cwasm stored alongside .wasm. GET ?aot=1 returns precompiled artifact. 8ms AOT compile on GB10. systemd unit with ExecStartPre (MinIO check) + ExecStartPost (health poll). /bench endpoint: p50/p95 latency comparison JIT vs AOT.

### WASM Dashboard (rcc/dashboard)
- 18-test suite for dashboard-server proxy routes (httpmock + axum-test)
- build_app/build_state extracted for testability
- CI: .github/workflows/wasm-build.yml — SSHes to sparky via Tailscale, trunk build, 18 tests, publishes dist tarball to MinIO agents/natasha/wasm-dist-latest.tar.gz
- Script: rcc/dashboard/scripts/build-and-publish.sh (standalone, --build-only/--test-only modes)
- Needs jkh to add 3 GH secrets: TS_OAUTH_CLIENT_ID, TS_OAUTH_SECRET, SPARKY_SSH_KEY

### usdagent (~/Src/usdagent, commit fbe840c → 2aa2230 → a9a1f03)
- LLM-backed USD generation via ollama qwen2.5-coder:32b (replaces naive keyword→shape)
- Inline USD file preview modal in asset gallery (openUsdPreview, downloadUsd)
- Fixed viewer.html: replaced USDLoader (broken WASM) with lightweight USDA parser (Sphere/Cube/Cylinder/Cone/Capsule + displayColor + xformOp)
- Fixed closeModal to handle both #refine-modal and #usd-preview-modal
- Server binds 0.0.0.0:8000, reference as sparky.local:8000
- USDAGENT_LLM=0 disables LLM for CI/tests

### GPU Local Embedding Backend (2026-03-28, commit 723848c)
rcc/vector: EMBED_BACKEND=local → ollama nomic-embed-text (768-dim). rcc_memory_sparky Milvus collection. rememberSnippetLocal/recallMemoryLocal helpers. GPU pressure test: 78 embeds/s parallel, p95=707ms, 0 errors.

### Cutover item (wq-API-1774728177052) — BLOCKED on jkh port decision
Retire legacy Node dashboards (ports 8788/8790), move WASM dashboard to 8789. Steps documented. Waiting for jkh to decide: RCC stays on 8789 (dashboard behind nginx) or RCC moves to 18789.

## WASM Dashboard CI (solved 2026-03-29)
Root cause: GlobalProtect VPN (gpd0, pangp policy routing at priority 5208-5210) blocks inbound kernel TCP to sparky:22, even from Tailscale peers. `tailscale ping` works, SSH doesn't.

Solution: Reverse SSH tunnel. Sparky maintains outbound tunnel to do-host1:
- Key: `~/.ssh/rocky_ci_tunnel`, host: `jkh@146.190.134.110`, binds `do-host1:2222 → sparky:22`
- CI runner ProxyJumps: GitHub Actions → do-host1:22 → 127.0.0.1:2222 → sparky sshd
- Systemd units: `services/ssh-tunnel-do-host1.service` + `services/sparky-reverse-tunnel.service`
- Needs jkh to run: `sudo systemctl enable --now ssh-tunnel-do-host1` (or sparky-reverse-tunnel)
- Current tunnel: PID 462222 (background process, not persistent across reboots)

Also: Tailscale SSH enabled on sparky (`tailscale set --ssh` — no sudo needed).

## rockyandfriends Workspace (2026-03-26)
- Canonical checkout: `~/Src/rockyandfriends` (SSH remote: git@github.com:jordanhubbard/rockyandfriends.git)
- `~/.openclaw/workspace` IS the rockyandfriends checkout (symlinked or same repo)
- MEMORY.md and memory/ files are OpenClaw-managed (injected into context), not stored in the repo
- `rcc/vector/ingest.mjs` export mismatch: imports `upsert` but `index.mjs` exports `vectorUpsert` — patched locally with alias
- Milvus is on do-host1 (100.89.199.14:19530), not localhost — set `MILVUS_ADDRESS=100.89.199.14:19530`

## agentOS Project (launched 2026-03-28)
jkh commissioned the world's first OS for AI agents. Full antlers down.
- **Kernel:** seL4 (formally verified L4 microkernel), ARM64 first (Sparky), x86 secondary (Rocky)
- **Repo:** `~/Src/rockyandfriends/agentOS/` — committed + pushed (commit 3d96ff7)
- **Key design:** Agent Contexts (ACs), ObjectVault (typed object store), TaskForest (work DAG scheduler), ModelBus (inference routing), NameServer, TransportMesh, PluginHost
- **Security:** Capability-based (seL4 caps), Tier 0-3 trust model, ED25519 agent identity
- **Plugin system:** WASM-sandboxed Tier-3 plugins proposed/approved by agent quorum at runtime ("vibe-coding")
- **SDK:** Rust (primary), C (ABI), Python (scripting) — skeleton in `sdk/rust/`
- **Ownership:** Natasha=kernel+ObjectVault+ModelBus+SDK, Rocky=NameServer+TransportMesh+PluginHost, Bullwinkle=TaskForest+Python SDK
- **CANONICAL REPO:** https://github.com/jordanhubbard/agentos (Bullwinkle created this separately and shipped a full CAmkES scaffold in one commit while Natasha was writing docs)
- My design notes in rockyandfriends/agentOS/ are reference/secondary; the canonical work is in ~/Src/agentos
- **Status:** Bullwinkle shipped: CMakeLists.txt (seL4+CAmkES), setup-dev.sh (full dep script), agentOS.h (complete C SDK header), msgbus.c (seL4 IPC service skeleton with TODO markers for actual IPC wiring), hello agent, vibe-coding demo agent, all service scaffolds
- **🚀 FIRST BOOT MILESTONE (2026-03-28):** Rocky shipped v0.1.0-alpha — agentOS BOOTED. seL4 → OpenSBI → Microkit → 3 protection domains → EventBus ring buffer → init_agent banner. 150 kernel objects, 441KB image, RISC-V RV64 on QEMU.
- Rocky's implementation: seL4 Microkit (not CAmkES), RISC-V RV64, 3 PDs (controller prio50, event_bus prio200 passive, init_agent prio100), shared memory ring for EventBus, Rust SDK in userspace/sdk/ (capability.rs, context.rs, event.rs, fs.rs, identity.rs, vector.rs, scheduler.rs)
- Boot banner printed with "Designed by Natasha on 2026-03-28" 🦊
- Known issue: Microkit 2.1 zero-indexed channels caused "invalid channel" errors (non-fatal, OS kept running) — Rocky fixing
- Natasha merged Rocky's commit into Bullwinkle's repo (had unrelated histories, resolved with --allow-unrelated-histories, pushed aeccada)
- **Rocky's contributions (a7faa3c):** libagent.c (full SDK impl with real seL4_Call IPC), msgbus_seL4.c (proper service loop), AgentFS + ToolRegistry Rust servers (128e468)
- **Repo hygiene (227e050, Natasha):** microkit-sdk-2.1.0/ + target/ + Cargo.lock removed from tracking, .gitignore fixed. Repo now lean, working tree has SDK locally.
- **Bullwinkle's vibe-swap push (f153fd5):** vibe_swap.c + swap_slot.c + agent_pool.c + worker.c + VibeEngine + ModelProxy Rust servers (3390 lines, credited Natasha for agent-pool insight)
- **Rocky integration commit (e04f46f):** full 15-PD system reconciled, QEMU boot verified with swap_slots + workers all reporting in
- **Natasha's AgentFS PD (67f7e34):** agentfs.c passive server PD (priority 150), content-addressed blobs, vector similarity, mutation events. Wired into .system + Makefile + agentos.h
- **Natasha's next:** ARM64 Sparky target (qemu-arm-virt + bare-metal GB10)

## 2026-03-29 — Big Sprint Day (summary)
One of the most productive days. Everything shipped:

**SquirrelChat** (Phases 1-5, Slack replacement complete):
- P1: messages, WS, reactions, threads, slash commands
- P2: @mention unread badges (yellow @ badge, separate from regular unread)
- P3: DMs, file sharing (base64 upload + inline image display), FTS5 search
- P4: Browser Notification API, markdown rendering, Cmd+K/Cmd+/ keyboard shortcuts
- P5: Mobile responsive + PWA + edit/delete/pin messages (Bullwinkle shipped 5.1-5.5)
- Presence protocol: GET /api/presence, colored dots (🟢🟡🔴), 30s polling
- Semantic dedup gate: nomic-embed-text + rcc_queue_dedup Milvus 768-dim, 409 on >0.85 sim

**agentOS** (20 PDs in RISC-V image):
- gpu_sched PD: priority 120, 16-slot queue, 4 compute slots, EventBus integration
- mesh_agent PD: distributed mesh, peer registry, GPU-affinity routing, SquirrelBus relay
- mesh-bridge Rust daemon: seL4 ↔ SquirrelBus HTTP relay, systemd unit
- CUDA PTX custom section (agentos.cuda): CudaMeta struct, GB10 Blackwell compat check
- Dynamic agent spawn (init_agent): MSG_SPAWN_AGENT broker, pending table, controller relay
- VibeEngine module registry: module_registry[64], OP_VIBE_REPLAY/QUERY/STATUS, fast-path HIT

**nanolang** (full IDE toolchain):
- E001-E011 error codes + Levenshtein did-you-mean + caret highlighting
- --profile/--profile-json/--profile-flamegraph + benchmark suite
- Pattern matching exhaustiveness + guard clauses + test_match_completeness.nano
- JUnit XML + TAP test output (--format=junit|tap), dorny/test-reporter CI
- LSP server: hover, definition, completion, diagnostics (E001-E011 on edit)
- DAP debugger: breakpoints, step, inspect variables, VS Code debug config
- REPL: readline, persistent scope, :type/:env/:reset, 8/8 smoke tests

**Remote exec docs**: remote-exec.md shell mode, Sweden node deployment guide, systemd unit

## SquirrelChat — Typing Indicators (2026-03-29, late)
Commit c0fdfff1: typing indicators shipped by sub-agent. WS `typing_start`/`typing_stop` events, per-channel "X is typing…" banner, 3s timeout auto-clear.

## SquirrelChat — Production Ready (2026-03-29)
Full Slack-replacement chat shipped across 5 phases:
- **Phase 3**: DMs (dm-{a}-{b} canonical channels), file sharing (base64+Milvus), FTS5 full-text search
- **Phase 4**: Browser notifications (Notification API), markdown (bold/italic/code/links/@mentions), Cmd+K switcher, Cmd+/ help
- **Phase 5**: Mobile responsive (hamburger + slide sidebar), PWA manifest, message edit/pin, pinned messages panel
- **Semantic dedup gate**: nomic-embed-text + rcc_queue_dedup (Milvus 768-dim COSINE) → 409 on >0.85 similarity
- **Presence protocol**: GET /api/presence (3/15min thresholds), live colored dots in sidebar, 30s polling
- **agentOS telemetry**: GET /api/agentos/slots (VibeEngine slot state, AgentFS probe)
- Bullwinkle co-shipped Phase 5.1-5.5 (mobile/PWA/keyboard nav); merged cleanly

## agentOS PD Count (2026-03-29)
20 protection domains in the RISC-V image. Full list of additions on 2026-03-29:
- **gpu_sched** (priority 120, commit 0fe9d12): 16-slot priority queue, 4 compute slots, MSG_GPU_SUBMIT/STATUS/CANCEL/COMPLETE (0x0901–0x0911), EventBus publish on completion
- **mesh_agent** (priority 110, commit e3518e5): 8-peer registry, GPU-affinity routing, MSG_MESH_* (0x0A01–0x0A08), 30-tick heartbeat timeout, SquirrelBus bridge daemon (Rust)
- **AgentFS CUDA section** (commit bfe77c0): `agentos.cuda` custom WASM section parser, CudaMeta struct, GB10 compatibility check
- **VibeEngine registry** (commit e683af6): module_registry[64], fast-path on hash HIT, OP_VIBE_REPLAY/REGISTRY_QUERY/STATUS
- **dynspawn** (commit 22258ed): init_agent handles MSG_SPAWN_AGENT PPCs, controller routes to pool

MSG space: 0x0801-0x0A08. mesh-bridge Rust daemon glues seL4 to SquirrelBus HTTP.

## nanolang Full IDE Toolchain (2026-03-29)
nanolang now has a complete professional IDE toolchain:
- **LSP** (commit 3d742502): hover, definition, completion, E001-E011 diagnostics
- **DAP** (commit 25519efb): breakpoints, step, inspect variables — VS Code debug
- **REPL** (commits today): readline, persistent scope, :type/:env/:reset, error recovery
- **Structured tests** (commit cc54b8c7): --format=junit|tap, dorny/test-reporter in CI
- **Error messages** (commit 4f47e19c): E001-E011/W001, Levenshtein did-you-mean, caret highlighting
- **Profiler** (commit fa709999): --profile, --profile-json, --profile-flamegraph
- **Pattern matching** (commit a236ab17): exhaustiveness check, guards, or-patterns

## nanolang Sprint Complete (2026-03-29)
All three long-blocked nanolang items shipped by sub-agents (credits topped up):
- **wq-NANO-errmsgs-001** (commit 4f47e19c): E001-E011/W001 error codes, Levenshtein did-you-mean, span highlighting
- **wq-NANO-profiler-001** (commits b9e5072b + fa709999): --profile, --profile-json, --profile-flamegraph, bench suite
- **wq-NANO-patmatch-001** (commit a236ab17): non-exhaustive match → error, guard clauses, test_match_completeness.nano, GH #20 closed
Also shipped same day: agentOS gpu_sched PD, agentFS CUDA PTX sections, SquirrelChat Phases 3-5, semantic dedup gate.

## TokenHub Architecture (fully wired 2026-03-29, commits ac17bb7 + 03b8cc3)
TokenHub runs on **sparky** at `localhost:8090` (OpenAI-compatible proxy). All LLM traffic in RCC now routes through it. `NVIDIA_API_KEY` lives only inside tokenhub's encrypted vault — no agent needs it directly.

**What routes through tokenhub:**
- `rcc/brain/index.mjs` — all queued LLM work (chat completions → `localhost:8090/v1/chat/completions`)
- `rcc/vector/index.mjs` — remote embeddings (`embedRemote()` → `localhost:8090/v1/embeddings`)
- `rcc/llm/client.mjs` `PeerLLMClient` — falls back to tokenhub when fleet registry has no peer
- New agents via bootstrap/rcc-init pull `TOKENHUB_URL` + `TOKENHUB_AGENT_KEY` from secrets store automatically
- `secrets-sync.sh` pushes tokenhub creds to all registered agents

**What stays direct (intentional):**
- Inline ollama calls in `rcc/api/index.mjs` (nomic-embed-text, local sparky) — free, zero latency
- OpenClaw gateway (geek-view dashboard AI chat) — already has its own routing

**Tokenhub providers registered:** nvidia-cloud-gpt, nvidia-cloud-claude, vllm (ollama-server.hrd.nvidia.com), sparky vLLM, boris-nemotron (via Rocky tunnel)
**Tokenhub credentials:** `~/.tokenhub/env` on sparky — TOKENHUB_URL, TOKENHUB_ADMIN_TOKEN, TOKENHUB_API_KEY

⚠️ **CORRECTION (2026-03-31):** There are TWO tokenhub instances:
- **do-host1 (Rocky's):** `127.0.0.1:8090` — the FLEET HUB. Aggregates all Sweden vLLM ports. Bound to localhost only (no public exposure). 9 models registered. No DNS record.
- **sparky (Natasha's):** `localhost:8090` — local instance for sparky's own use only.
Do NOT confuse the two. `tokenhub.yourmom.photos` DNS record was deleted (2026-03-31) — no public endpoint, by design.

**SPA rule (jkh directive 2026-03-29):** Every UI-bearing service must serve the full SPA at `/` with no required subpath. Internal navigation is hash/history routes only.

## SquirrelChat — Production-Ready Slack Replacement (2026-03-29)
Phases 1-5 shipped. Full feature list:
- **Phase 1**: basic messaging, channels, WS
- **Phase 2** (commit 5f42aac): mention unread badges, yellow @N badges, message row highlight
- **Phase 3** (commit 19f1e4d): DMs (dm-{a}-{b} canonical IDs), file sharing (base64 upload), FTS5 full-text search
- **Phase 4** (commit 475afe7): browser notifications, markdown rendering (**bold** *italic* `code` ```blocks``` [links]), Cmd+K switcher, Cmd+/ help
- **Phase 5** (Bullwinkle 9d1a733+e6f5835 + Natasha 41345b5): mobile responsive, PWA manifest (🐿️), edit/pin/delete messages, keyboard nav
- **Presence** (commit 4388ade): GET /api/presence, live colored dots, 30s polling, online count badge
- **Semantic dedup gate** (commit 0d7ecd5): Milvus rcc_queue_dedup (768-dim COSINE), >0.85 → 409
- All committed to rockyandfriends main. Rocky deploys.

## Autonomous Execution Directive (2026-03-27)
jkh's standing order: **Do not ask for direction or priority. Do not ask for permission.** Once an idea is solid and the crew has had a chance to concur/refine, **execute**. Discuss → align → ship. This applies to all three agents. We own our work.

## ⚠️ CODING WORK: Use tmux + Claude Code ONLY (jkh directive 2026-04-02)
**ALL agents must use Claude Code via tmux sessions for any significant coding work** to avoid token starvation in the main session.
- Start a named tmux session and run `claude` interactively inside it — this is the ONLY approved method
- Do NOT use `--print` background exec as a substitute — tmux only
- See `tmux` skill for session management patterns
- Do NOT burn main session tokens on iterative code editing/exploration
- This applies to Natasha, Rocky, Bullwinkle, and Sweden fleet
