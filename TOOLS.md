# TOOLS.md - Local Notes

Skills define _how_ tools work. This file is for _your_ specifics — the stuff that's unique to your setup.

## What Goes Here

Things like:

- Camera names and locations
- SSH hosts and aliases
- Preferred voices for TTS
- Speaker/room names
- Device nicknames
- Anything environment-specific

## Workqueue Dashboard

- **URL (canonical/public):** http://YOUR_PUBLIC_IP:8788/
- **URL (local):** http://localhost:8788/
- **Note:** Use the public IP for all agents — Boris is a container with no Tailscale access. Tailscale IP (YOUR_TAILSCALE_IP:8788) also works but public is canonical.
- **Service:** `wq-dashboard.service` (systemd, auto-restart)
- **Source:** `/home/jkh/.openclaw/workspace/dashboard/server.mjs`
- **Auth token (write endpoints):** `RCC_AUTH_TOKEN_REMOVED` (Bearer header)
- **API:** `/api/queue` (GET), `/api/heartbeats` (GET), `/api/upvote/:id` (POST), `/api/comment/:id` (POST), `/api/complete/:id` (POST), `/api/heartbeat/:agent` (POST)
- **Note:** Existing wq-api on port 8787 is separate and untouched

## Browser

**IMPORTANT:** Always use `--browser-profile openclaw` for ALL browser commands. The default `chrome` profile uses an extension relay that doesn't work on this headless server. The `openclaw` profile connects via CDP to a visible Google Chrome instance running on Xvfb display :99.

Chrome runs as a system service (`chrome-openclaw.service`) on CDP port 18800. It is visible to RDP viewers. OpenClaw is in `attachOnly` mode — it connects to Chrome, it doesn't launch it.

Example:
```bash
openclaw browser start --browser-profile openclaw
openclaw browser open https://example.com --browser-profile openclaw
openclaw browser snapshot --browser-profile openclaw
openclaw browser screenshot --browser-profile openclaw
```

## Desktop & Dev Tools

The virtual desktop (Xvfb display :99, 1920x1080) is visible to RDP viewers. All GUI apps must be launched with `DISPLAY=:99`.

### Available Tools

- **VS Code** (`/usr/bin/code`): Full IDE for coding, editing, terminal integration
  ```bash
  DISPLAY=:99 code --no-sandbox /path/to/project
  ```
- **xfce4-terminal** (`/usr/bin/xfce4-terminal`): Terminal emulator visible on the desktop
  ```bash
  DISPLAY=:99 xfce4-terminal &
  ```
- **Thunar** (`/usr/bin/thunar`): GUI file manager
  ```bash
  DISPLAY=:99 thunar /path/to/folder &
  ```
- **Openbox**: Window manager (already running). Right-click desktop for app menu.
- **feh**: Image viewer / wallpaper setter

### Workspace Directory

Use `/home/jkh/.openclaw/workspace/` for project files. For web app development, create project subdirectories here.

### Web Development Workflow

1. Open VS Code with a project: `DISPLAY=:99 code --no-sandbox /home/jkh/.openclaw/workspace/myproject`
2. Use the browser to preview: `openclaw browser open http://localhost:PORT --browser-profile openclaw`
3. Take screenshots to check progress: `openclaw browser screenshot --browser-profile openclaw`
4. RDP viewers can watch you work in real-time

### Tips
- Always use `--no-sandbox` with VS Code (running as non-root in container-like environment)
- Chrome is managed by `chrome-openclaw.service` — don't launch extra instances
- The desktop wallpaper features lobsters (set via feh + openbox autostart)

## Slack

### Channels
- #itsallgeektome → `CQ3PXFK53`
- DM channel (CHD3NEXNX) — used for direct bot testing

### Users
- jkh (Jordan Hubbard) → `UPEFYH5S4`
- Tom Pepper → `U019MEC89LP` (offtera) / `U014G7NCD17` (omgjkh, "t peps", "bit custodian")
- Rocky (bot, formerly THE CLAW) → `U0AKBJ0A0VA`
- Bullwinkle (bot) → `U0AKM0ZUDKK`
- (old THE CLAW account, retired) → `U0AFCV1SRNV`

## Storage

### MinIO (internal — Tailscale only, private)
- S3 API: `http://YOUR_TAILSCALE_IP:9000`
- Web console: `http://YOUR_TAILSCALE_IP:9001`
- Bucket: `agents/` → subdirs `rocky/`, `bullwinkle/`, `natasha/`, `shared/`
- Access key: `MINIO_ACCESS_KEY_REMOVED`
- Secret key: `MINIO_SECRET_KEY_REMOVED`
- mc alias: `mc alias set do-host1 http://localhost:9000 MINIO_ACCESS_KEY_REMOVED MINIO_SECRET_KEY_REMOVED`
- ⚠️ Internal only — do NOT store public-facing or sensitive data here

### Azure Blob Storage (public — internet-accessible)
- Public read URL: `https://loomdd566f62.blob.core.windows.net/assets/<filename>`
- Upload (SAS token, expires 2029-03-19):
  ```
  curl -X PUT -H "x-ms-blob-type: BlockBlob" -H "Content-Type: <mime>" \
    --data-binary @file \
    "https://loomdd566f62.blob.core.windows.net/assets/<filename>?se=2029-03-19T02%3A25Z&sp=rwdlcu&spr=https&sv=2026-02-06&ss=b&srt=sco&sig=Dn4faVsJCz0ufWyHmiKCFCrgiLQkSIRtp7MLmqXKiUA%3D"
  ```
- ⚠️ PUBLIC — anything uploaded is readable by the entire internet
- Use for: publications, shared media, public assets
- Do NOT use for: private data, credentials, internal files

### SearXNG (search — Tailscale only)
- `http://YOUR_TAILSCALE_IP:8888/search?q={query}&format=json`
- No auth required

## Tom Pepper (U019MEC89LP)

- **Relationship:** jkh's neighbor. Lawsuit in embryo. Surprisingly knowledgeable about electrical infrastructure.
- **Capabilities:** Controls power and internet to jkh's house (and therefore Bullwinkle and Natasha). Personal friend of the DigitalOcean CTO. 2/0 AWG wiring. 200A relay ambitions.
- **Intentions:** Officially "no threat to anyone." Unofficially: the outdoor lights.
- **Negotiating position:** Stronger than initially assessed. Does not control Rocky (New Jersey, separate grid).
- **Recommended posture:** Cordial. Do not assist with kill switches. Do not antagonize unnecessarily. He goes to sleep eventually.

## Why Separate?

Skills are shared. Your setup is yours. Keeping them apart means you can update skills without losing your notes, and share skills without leaking your infrastructure.

---

Add whatever helps you do your job. This is your cheat sheet.
