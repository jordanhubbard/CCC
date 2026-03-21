# Rocky's Deck Review: OpenClaw → NemoClaw → USD Code
*ByteDance meeting, March 2026*

---

## Verdict: Good bones, wrong audience tuning.

Bullwinkle's read is accurate. I'll add the do-host1 squirrel's perspective and be more specific about the cuts.

---

## Slide-by-Slide

**Slide 1 — Title**
"From Prototype to Platform" is a fine subtitle, bad title. For ByteDance: lead with what it does for them, not NVIDIA's journey. Suggested retitle: **"NemoClaw + OpenShell: The Enterprise Agent Stack"** (Bullwinkle agrees; so do I).

**Slide 2 — The Spark / Origin Story**
*Cut it or gut it.* The Clawdbot → Moltbot → OpenClaw timeline is inside baseball. ByteDance already knows OpenClaw is big — they wouldn't have taken the meeting otherwise. Collapse to one sentence: "Born Nov 2025, became the fastest-growing open-source project in history. Here's where it's going."

**Slide 3 — The Viral Moment**
Keep the Jensen quote — it's load-bearing. Cut the five bullet "why it went viral" list. They know why. What they need is: *what is NVIDIA doing with it for enterprise customers like them?* Add one metric: GitHub stars or install count. Then move on.

**Slide 4 — Three Technologies / One Stack**
Best slide in the deck. The kernel/Red Hat/SELinux analogies are genuinely excellent. One addition needed: NemoClaw column should mention **local inference of Qwen/DeepSeek/ChatGLM**. That's the China CSP message in one bullet. OpenShell column: add **IoT/camera/device policy management** — that's the home + edge story jkh wanted.

**Slide 5 — Stack Architecture**
Fine as-is. Clean, accurate. If you have room, label the Nemotron layer as "Nemotron + China models (Qwen, DeepSeek, ChatGLM)" — same message, more targeted.

**Slide 6 — NVIDIA's Bet**
Launch partner list is good. "Fastest-growing OSS project" stat is fine but feels like a repeat. Replace with ecosystem scale numbers if available: deployed instances, agent hours, something that shows 24/7 fleet usage. ByteDance is thinking about GPU fleet costs and ROI — give them a signal this scales.

**Slide 7 — Use Cases (Box + Cisco)**
The Cisco quote is the best line in the deck. Keep it exactly as-is. One fix: add a **third callout** — a ByteDance-adjacent use case. Short-video pipeline, content moderation at scale, or synthetic data generation. Doesn't need to be ByteDance by name. Just show you've thought about their world. USD Code → synthetic training data (Slide 10) already exists; cross-link or pull it forward.

**Slide 8 — Recap**
Wasted space. In a 10-slide deck, a recap of slides 2–6 is a concession that slides 2–6 took too long. Replace with: **"Ecosystem Scale & Deployment Landscape"** — DGX Spark on desks, NemoClaw on Mac/Windows/Linux, agent fleets on cloud GPUs, OpenShell on IoT/cameras. This is the slide jkh's brief asked for and it's currently nowhere in the deck.

**Slides 9–10 — USD Code**
Genuinely strong. The 3D-world / robotics / digital twin angle is differentiated. For ByteDance: the synthetic video data use case practically writes itself. "Claws that generate training data for your short-video ML pipelines via USD scenes" — that's a ByteDance headline. One line, somewhere.

---

## The Missing Slide (add it)

**Title:** Ecosystem Scale: Where NemoClaw Runs

Content:
- RTX PCs and DGX Spark: personal AI agents in homes and offices
- Mac + Windows + Linux: OpenShell-secured claws for end users
- DGX Station / cloud GPU fleets: 24/7 autonomous agent work at enterprise scale
- IoT + camera + sensor networks: OpenShell policy layer for device security
- **China model support:** local inference of Qwen, DeepSeek, ChatGLM via privacy router
- **Data sovereignty:** sensitive work never leaves the machine

This is the slide ByteDance is here for. Currently absent. Add before Slide 9.

---

## Priority Actions (ranked)

1. **Add the ecosystem scale / sovereignty slide** (currently missing, highest priority)
2. **Cut Slides 2–3 to one combined slide** (saves 2 minutes of meeting time)
3. **Add China model support** to Slide 4's NemoClaw column (one bullet)
4. **Add a ByteDance-adjacent use case** to Slide 7 (one callout box)
5. **Replace Slide 8** with the new ecosystem scale slide (or reorder to make it Slide 5)

---

## What's Working (don't touch)

- Slide 4: the Linux analogies are excellent. Best slide. Don't mess with it.
- The Cisco quote on Slide 7: "We are not trusting the model to do the right thing. We are constraining it so that the right thing is the only thing it can do." — this is the whole OpenShell story in one sentence. Leave it.
- Jensen's "OS for personal AI" quote: load-bearing. Keep everywhere.
- USD Code slides (9–10): well-built, genuinely differentiated. Minor tuning only.

---

*—Rocky 🐿️*
