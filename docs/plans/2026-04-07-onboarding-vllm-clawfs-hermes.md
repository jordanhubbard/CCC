# Onboarding Enhancement: vLLM + ClawFS/FUSE + Hermes-Agent

> **For Hermes:** Execute this plan task-by-task using delegate_task.

**Goal:** Extend all CCC client onboarding scripts so new agents get vLLM inference (defaulting to Gemma 4 via ClawFS), FUSE-mounted ClawFS from day zero (Linux auto, macOS optional), and hermes-agent as the standard runtime.

**Architecture:** Three new onboarding capabilities layered into the existing bootstrap/setup/init scripts. ClawFS uses JuiceFS backed by Redis (metadata) + MinIO (data) on Rocky. vLLM serves models from the ClawFS mount at ~/clawfs/models/. Hermes-agent replaces OpenClaw as the default agent runtime.

**Tech Stack:** JuiceFS (FUSE), vLLM (Python), hermes-agent (Python/pip), macFUSE (macOS optional)

---

## Summary of Changes

| File | What Changes |
|------|-------------|
| `deploy/bootstrap.sh` | Add ClawFS/FUSE setup (step 9g), vLLM install+start (step 9h), hermes-agent install (step 2b) |
| `deploy/setup-node.sh` | Add ClawFS/FUSE install, vLLM detection/install, promote hermes to required |
| `deploy/rcc-init.sh` | Add ClawFS and vLLM prompts in capability wizard (steps 4b, 4c) |
| `deploy/.env.template` | Add CLAWFS_*, VLLM_*, FUSE_* env vars |
| `deploy/register-agent.sh` | Add vllm + clawfs capability flags |
| `onboarding/ONBOARD_NEW_AGENT.md` | Add Steps 8 (ClawFS), 9 (vLLM), 10 (Hermes) |

---

## Design Decisions

### ClawFS/FUSE
- **Linux:** Auto-install JuiceFS + FUSE utils, mount ~/clawfs, create systemd unit
- **macOS:** Check for macFUSE; if missing, print instructions (requires system extension + reboot); if present, mount ~/clawfs
- **Mount command:** `juicefs mount --background --cache-dir /tmp/jfscache redis://100.89.199.14:6379/1 ~/clawfs`
- **Sentinel file:** `~/clawfs/.config` indicates successful mount
- **Env vars:** `CLAWFS_MOUNT`, `CLAWFS_REDIS_URL`, `CLAWFS_ENABLED`

### vLLM
- **Only on GPU nodes** (detected via nvidia-smi)
- **Default model:** `google/gemma-4-31B-it` — pulled from ClawFS if mounted, else downloaded from HuggingFace
- **Model path:** `~/clawfs/models/gemma-4-31B-it` (ClawFS) or `~/models/gemma-4-31B-it` (fallback)
- **Served name:** `gemma` (derived per model-deployment.md convention)
- **Port:** 8000 (standard vLLM)
- **Env vars:** `VLLM_ENABLED`, `VLLM_MODEL`, `VLLM_SERVED_NAME`, `VLLM_PORT`, `VLLM_MODEL_PATH`
- **Startup:** systemd unit on Linux, tmux session fallback

### Hermes-Agent
- **Primary runtime** — install via pipx (preferred) or pip
- **Migrate OpenClaw config** if present (hermes claw migrate)
- **Install acc-node skill** automatically
- **Env vars:** already covered by existing .env.template
- **OpenClaw still installed** (for backward compat) but hermes is the launcher
