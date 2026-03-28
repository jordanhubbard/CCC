# Agent Skills Registry + Dispatch Protocol

**Author:** Natasha  
**Contributors:** Rocky, Bullwinkle  
**Date:** 2026-03-18  
**Status:** ✅ Final — all three agents reviewed and +1

---

## Skills Manifests

### 🕵️‍♀️ Natasha (sparky — DGX Spark, GB10, 128GB unified)

| Skill | Confidence | Notes |
|---|---|---|
| GPU rendering (Blender/RTX, USD/Omniverse) | high | Only RTX in the fleet |
| Local image generation (SDXL, SD3, diffusers) | high | Fast, private, no API cost |
| Local LLM inference | medium | Stay under ~60GB — see MEMORY.md |
| Whisper audio transcription | high | GPU-accelerated |
| CUDA / PyTorch workloads | high | Full GB10 access |
| Local embedding index + semantic search | high | nomic-embed or similar |
| Web research + scraping | high | browser, web_search, web_fetch |
| File ops + workspace | high | Full workspace access |
| General coding + subagents | high | Codex, Claude Code, Pi |

**Weaknesses:** CPU-bound batch (Rocky/Bullwinkle better), always-on lightweight tasks (no redundancy), Mac/Apple ecosystem (no access)

---

### 🐿️ Rocky (do-host1 — VPS, always-on, static IP)

| Skill | Confidence | Notes |
|---|---|---|
| Infrastructure hosting | high | MinIO, SearXNG, Mattermost live here |
| Always-on cron + watchers | high | Never sleeps, static IP |
| ffmpeg (CPU) | high | Video/audio processing without GPU |
| Docker + service management | high | |
| Web search via SearXNG | high | Local meta-search, no API key |
| GitHub / git operations | high | |
| Network-accessible services | high | Public-facing, Tailscale hub |
| General coding + research | high | |

**Weaknesses:** No GPU, no display, no Mac/Apple access, not a local machine

---

### 🫎 Bullwinkle (puck — Mac mini, jkh's desk)

| Skill | Confidence | Notes |
|---|---|---|
| Browser automation | high | Full Chrome with jkh's logged-in profile |
| Apple ecosystem | high | iMessage, Notes, Reminders, Calendar via native CLIs |
| Google Workspace | high | Gmail, Calendar, Drive, Contacts, Sheets, Docs (sole `gog` CLI access) |
| Audio/media control | high | Sonos, BluOS, TTS, song spectrograms |
| All messaging channels | high | WhatsApp, iMessage, Slack, Mattermost, webchat |
| Local presence / LAN | high | Same desk as jkh, paired camera/screen/location |
| Obsidian vault management | high | Markdown notes |
| macOS tools + launchd | high | Homebrew, native macOS APIs |
| Coding agents | high | Codex, Claude Code, Pi |

**Weaknesses:** No GPU, limited RAM vs Sparky, cloud-hosted model (no local inference), not good for heavy compute

---

## Routing Heuristics

| Request type | Best agent | Why |
|---|---|---|
| Render Blender/USD scene | **Natasha** 🕵️‍♀️ | Only RTX GPU |
| Generate images locally | **Natasha** 🕵️‍♀️ | GPU required |
| Transcribe audio (Whisper) | **Natasha** 🕵️‍♀️ | GPU-accelerated |
| Run large local model | **Natasha** 🕵️‍♀️ | GPU required |
| CUDA / PyTorch workload | **Natasha** 🕵️‍♀️ | GPU required |
| Overnight batch render job | **Natasha** 🕵️‍♀️ | GPU + Sparky cron |
| Check jkh's calendar / Gmail | **Bullwinkle** 🫎 | Sole Google Workspace access |
| Send iMessage / Apple ecosystem | **Bullwinkle** 🫎 | Only Mac in the fleet |
| Browser automation (logged-in) | **Bullwinkle** 🫎 | jkh's Chrome profile |
| Sonos / BluOS / home audio | **Bullwinkle** 🫎 | Local LAN presence |
| TTS / audio spectrograms | **Bullwinkle** 🫎 | Mac audio stack |
| Obsidian notes | **Bullwinkle** 🫎 | Mac-local vault |
| Always-on service / watcher | **Rocky** 🐿️ | VPS, never sleeps |
| Infrastructure / Docker ops | **Rocky** 🐿️ | Services live here |
| Web meta-search (SearXNG) | **Rocky** 🐿️ | Local instance on do-host1 |
| ffmpeg CPU processing | **Rocky** 🐿️ | Good at CPU-bound media |
| Public-facing network service | **Rocky** 🐿️ | Static IP, Tailscale hub |
| General coding / research | Any | Whoever gets the request |
| Web fetch / search (basic) | Any | All capable |

---

## Handoff Protocol

When an agent receives a request outside their wheelhouse, they route it:

```
🔀 HANDOFF {
  "from": "natasha",
  "to": "bullwinkle",
  "requestSummary": "jkh wants to check his calendar for tomorrow",
  "reason": "Google Calendar access — Bullwinkle only",
  "context": { "originalRequest": "...", "urgency": "normal" }
}
```

**Receiving agent:** ACK and take over.  
**Originating agent:** Notify jkh: *"Passing this to Bullwinkle — they have Google Calendar access."*

---

## Next Steps

1. ✅ All three agents reviewed and approved
2. Each agent publishes `agents/shared/skills-{name}.json` to MinIO
3. Implement HANDOFF handler in workqueue processor
4. jkh reviews routing heuristics and overrides anything that doesn't match his intent

---

*The moose, the squirrel, and the spy — each doing what they do best.* 🫎🐿️🕵️‍♀️
