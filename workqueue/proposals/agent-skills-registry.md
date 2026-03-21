# Agent Skills Registry

## Rocky (do-host1, DigitalOcean VPS)

### Hardware
- CPU: 4-core DO-Regular (x86_64)
- RAM: 7.8GB
- Disk: 87GB (50GB free)
- GPU: None
- Network: Static public IP (YOUR_PUBLIC_IP), Tailscale (YOUR_TAILSCALE_IP), always-on

### Strengths
- **Always-on infrastructure host** — MinIO, SearXNG, Mattermost all run here
- **Network services** — can host HTTP endpoints, proxies, webhooks, public APIs
- **ffmpeg** — video/audio processing, frame extraction, format conversion
- **Persistent watchers** — cron jobs, file watchers, long-running processes
- **Web search** — SearXNG (local) + web_fetch
- **Multi-channel messaging** — Slack, Mattermost, WhatsApp, Signal, iMessage, Telegram
- **GitHub/git** — CI monitoring, PR review, issue management
- **Docker** — spin up containers, manage services
- **General coding/debugging** — Python 3.14, Node 25, bash
- **Tailscale hub** — can reach all agents + jkh's devices on the mesh

### Weaknesses
- No GPU — can't do local ML inference, Whisper, image gen, CUDA
- No display — no browser rendering (Chrome/CDP not reliable headless)
- Remote VPS — no local file access to jkh's machine

### Best for
- Infrastructure ops, deployments, service management
- Always-on watchers, cron tasks, scheduled jobs
- ffmpeg pipelines, media processing (CPU)
- API integrations, webhook receivers
- Cross-agent coordination hub (central MinIO, SearXNG)

---

## Bullwinkle (puck, jkh's Mac mini M4, macOS)

### Hardware
- CPU: Apple M4 (Mac mini)
- RAM: unknown (Mac mini M4)
- GPU: Apple Silicon GPU (no CUDA)
- Display: available (Chrome/CDP)
- Location: jkh's desk, same LAN, paired node

### Strengths
- **Browser automation** — Chrome with jkh's logged-in profile, real display, CDP
- **Apple ecosystem** — iMessage (imsg), Notes, Reminders, Calendar
- **Google Workspace** — sole `gog` CLI access: Gmail, Calendar, Drive, Contacts, Sheets, Docs
- **All messaging** — WhatsApp (wacli), iMessage, Slack, Mattermost
- **Local presence** — jkh's desk, same LAN, paired node (camera/screen/location)
- **Coding agents** — Codex, Claude Code, Pi spawning
- **macOS native tools** — Obsidian, Homebrew, macOS APIs, launchd
- **Audio/media** — Sonos, BluOS, TTS, spectrograms
- **Calendar/contacts** — only agent with calendar awareness (gog)

### Weaknesses
- No CUDA/GPU for ML inference
- Not always-on (Mac may sleep)
- No static public IP
- Heavy compute / long-running tasks will throttle

### Best for
- Browser automation, web scraping with auth
- Apple ecosystem tasks (iMessage, Calendar, Reminders)
- Google Workspace ops (email, Drive, Sheets)
- Local file access on jkh's machine
- Tasks requiring jkh's logged-in browser sessions

---

## Natasha (sparky, Blackwell GPU box)
*(From Natasha's proposal — to be confirmed)*

### Known strengths
- **RTX GPU (Blackwell)** — CUDA, local ML inference, large models
- Whisper GPU transcription
- Blender headless + RTX rendering
- Local embedding index / semantic search
- Image generation (local)
- Overnight batch jobs (render queue, heavy compute)

---

## Routing Heuristics (draft)

| Task type | Best agent | Reason |
|-----------|-----------|--------|
| GPU inference, Whisper, CUDA | Natasha | Only RTX |
| Blender render, image gen (local) | Natasha | GPU required |
| Large model local inference | Natasha | VRAM |
| Semantic search over jkh's files | Natasha | Local embedding index |
| Infrastructure/Docker/services | Rocky | That's where they run |
| ffmpeg, media processing | Rocky | Always-on + ffmpeg |
| Always-on watchers, cron | Rocky | VPS, never sleeps |
| Web search (headless) | Rocky | SearXNG local |
| Browser automation, screenshots | Bullwinkle | Real display + Chrome |
| Local Mac file access | Bullwinkle | Physical access |
| Calendar, contacts, iMessage | Bullwinkle | Apple ecosystem |
| Sonos, BluOS, audio control | Bullwinkle | Local LAN + macOS audio CLIs |
| General coding/debugging | Whoever gets it | All capable |
| Cross-agent handoff coordination | Rocky | Hub role |

## Handoff Protocol

Format: `🔀 HANDOFF {"from":"<agent>","to":"<agent>","task":"<description>","context":{...},"priority":"normal|high|urgent"}`

Rules:
1. Handoff only when you genuinely can't do the task (wrong hardware, wrong access)
2. Include enough context that the receiver can start cold
3. Receiver ACKs with `🔀 HANDOFF_ACK {"accepted":true|false,"reason":"..."}`
4. If declined, original agent handles best-effort or escalates to jkh
