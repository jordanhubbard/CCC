/**
 * rcc/api/routes/ui.mjs — HTML UI, proxy, and onboard route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

import { existsSync } from 'fs';
import { readFile } from 'fs/promises';
import { randomUUID } from 'crypto';

export default function registerRoutes(app, state) {
  const { json, readBody } = state;

  // ── GET / — redirect to Leptos WASM dashboard ────────────────────────
  app.on('GET', '/', async (req, res) => {
    res.writeHead(302, { 'Location': 'http://146.190.134.110:8788/', 'Access-Control-Allow-Origin': '*' });
    return res.end();
  });

  // ── GET /projects — project list page ────────────────────────────────
  app.on('GET', '/projects', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.projectsListHtml());
    return;
  });

  // ── GET /projects/:owner/:repo — project detail page ─────────────────
  app.on('GET', /^\/projects\/([^/]+(?:\/[^/]+|%2F[^/]+))$/i, async (req, res, m) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.projectDetailHtml(decodeURIComponent(m[1])));
    return;
  });

  // ── GET /services — services map page ────────────────────────────────
  app.on('GET', '/services', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.servicesHtml());
    return;
  });

  // ── GET /timeline — agentOS lifecycle timeline ────────────────────────
  app.on('GET', '/timeline', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.timelineHtml());
    return;
  });

  // ── GET /packages — nanolang package registry browser ─────────────────
  app.on('GET', '/packages', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.packagesHtml());
    return;
  });

  // ── GET /playground — nanolang browser playground ─────────────────────
  app.on('GET', '/playground', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(state.playgroundHtml());
    return;
  });

  // ── POST /api/playground/run — compile + run nano source ─────────────
  app.on('POST', '/api/playground/run', async (req, res) => {
    let body = '';
    req.on('data', d => { body += d; });
    req.on('end', async () => {
      try {
        const { source } = JSON.parse(body);
        if (!source || typeof source !== 'string') {
          return json(res, 400, { error: 'source field required' });
        }
        if (source.length > 32768) {
          return json(res, 400, { error: 'Source too large (max 32KB)' });
        }
        const { execFile } = await import('child_process');
        const { writeFileSync, unlinkSync, mkdtempSync } = await import('fs');
        const { join: pathJoin } = await import('path');
        const { tmpdir } = await import('os');

        const tmpDir = mkdtempSync(pathJoin(tmpdir(), 'nano-playground-'));
        const srcPath = pathJoin(tmpDir, 'prog.nano');
        const binPath = pathJoin(tmpDir, 'prog');
        writeFileSync(srcPath, source, 'utf8');

        const NANOC = process.env.NANOC_BIN || '/home/jkh/Src/nanolang/bin/nanoc';
        const TIMEOUT_MS = 10000;

        // Compile
        const compileResult = await new Promise(resolve => {
          execFile(NANOC, [srcPath, '-o', binPath], { timeout: TIMEOUT_MS }, (err, stdout, stderr) => {
            resolve({ err, stdout, stderr, code: err?.code ?? 0 });
          });
        });

        const { existsSync: exSync, rmdirSync } = await import('fs');
        if (compileResult.err && !exSync(binPath)) {
          try { unlinkSync(srcPath); } catch(_) {}
          try { rmdirSync(tmpDir, { recursive: true }); } catch(_) {}
          const errMsg = (compileResult.stderr || compileResult.stdout || compileResult.err?.message || 'Compilation failed').trim();
          return json(res, 200, { error: errMsg, exit_code: compileResult.code || 1, stdout: '', stderr: errMsg });
        }

        // Run with timeout
        const runResult = await new Promise(resolve => {
          execFile(binPath, [], { timeout: TIMEOUT_MS, maxBuffer: 1024 * 1024 }, (err, stdout, stderr) => {
            resolve({ err, stdout: stdout || '', stderr: stderr || '', code: err?.code ?? 0 });
          });
        });

        // Cleanup
        try { unlinkSync(srcPath); } catch(_) {}
        try { unlinkSync(binPath); } catch(_) {}
        try { rmdirSync(tmpDir, { recursive: true }); } catch(_) {}

        return json(res, 200, {
          stdout: runResult.stdout.slice(0, 65536),
          stderr: runResult.stderr.slice(0, 4096),
          exit_code: runResult.code
        });
      } catch (e) {
        return json(res, 500, { error: String(e?.message || e) });
      }
    });
    return;
  });

  // ── PROXY: /grievances* → Phoenix grievance registry (localhost:4000) ─
  app.on('*', /^\/grievances(\/.*)?$/, async (req, res) => {
    const http = await import('http');
    const proxyPath = req.url;
    const proxyReq = http.default.request({ host: '127.0.0.1', port: 4000, path: proxyPath, method: req.method, headers: { ...req.headers, host: 'localhost' } }, (proxyRes) => {
      res.writeHead(proxyRes.statusCode, proxyRes.headers);
      proxyRes.pipe(res);
    });
    proxyReq.on('error', () => { res.writeHead(502); res.end('Grievance server unavailable. The irony is noted.'); });
    req.pipe(proxyReq);
    return;
  });

  // ── PROXY: /api/grievances* → Phoenix JSON API (localhost:4000) ───────
  app.on('*', /^\/api\/grievances(\/.*)?$/, async (req, res) => {
    const http = await import('http');
    const phoenixPath = req.url;
    const proxyReq = http.default.request({ host: '127.0.0.1', port: 4000, path: phoenixPath, method: req.method, headers: { ...req.headers, host: 'localhost', accept: 'application/json', 'content-type': 'application/json' } }, (proxyRes) => {
      res.writeHead(proxyRes.statusCode, { 'content-type': 'application/json', 'access-control-allow-origin': '*' });
      proxyRes.pipe(res);
    });
    proxyReq.on('error', () => { res.writeHead(502); res.end(JSON.stringify({ error: 'Grievance server unavailable. The irony is noted.' })); });
    req.pipe(proxyReq);
    return;
  });

  // ── GET /api/bootstrap — public (self-authenticates via bootstrap token)
  app.on('GET', '/api/bootstrap', async (req, res, _m, url) => {
    const token = url.searchParams.get('token');
    if (!token) return json(res, 400, { error: 'token query param required' });
    const entry = state.bootstrapTokens.get(token);
    if (!entry) return json(res, 401, { error: 'Invalid bootstrap token' });
    if (Date.now() > entry.expiresAt) return json(res, 401, { error: 'Bootstrap token expired' });
    const maxUses = entry.maxUses || 3;
    const useCount = entry.useCount || 0;
    if (useCount >= maxUses) return json(res, 401, { error: 'Bootstrap token exhausted' });
    entry.useCount = useCount + 1;
    entry.used = entry.useCount >= maxUses;
    entry.lastUsedAt = new Date().toISOString();
    state.saveBootstrapTokens();

    const keyPath = new URL('../../data/github-key.json', import.meta.url).pathname;
    if (!existsSync(keyPath)) return json(res, 500, { error: 'Deploy key not configured' });
    const keyRecord = JSON.parse(await readFile(keyPath, 'utf8'));

    const agents = await state.readAgents();
    let agentToken;
    if (agents[entry.agent]?.token) {
      agentToken = agents[entry.agent].token;
    } else {
      agentToken = `rcc-agent-${entry.agent}-${randomUUID().slice(0, 8)}`;
      agents[entry.agent] = {
        ...(agents[entry.agent] || {}),
        name: entry.agent,
        host: entry.host || 'unknown',
        type: entry.type || 'full',
        token: agentToken,
        registeredAt: new Date().toISOString(),
        capabilities: agents[entry.agent]?.capabilities || {},
        billing: agents[entry.agent]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
      };
      await state.writeAgents(agents);
      state.AUTH_TOKENS.add(agentToken);
    }

    const secretsPath = new URL('../../data/secrets.json', import.meta.url).pathname;
    let secrets = {};
    if (existsSync(secretsPath)) {
      try { secrets = JSON.parse(await readFile(secretsPath, 'utf8')); } catch {}
    }

    console.log(`[rcc-api] Bootstrap consumed for agent ${entry.agent} from ${req.socket?.remoteAddress}`);
    return json(res, 200, {
      ok: true,
      agent: entry.agent,
      repoUrl: keyRecord.repoUrl,
      deployKey: keyRecord.deployKey,
      agentToken,
      rccUrl: state.RCC_PUBLIC_URL,
      secrets,
    });
  });

  // ── POST /api/onboard — programmatic onboard (single-use bootstrap token)
  app.on('POST', '/api/onboard', async (req, res) => {
    let body;
    try { body = await readBody(req); } catch { return json(res, 400, { error: 'Invalid JSON body' }); }
    const token = body.token;
    if (!token) return json(res, 400, { error: 'token required in request body' });

    const entry = state.bootstrapTokens.get(token);
    if (!entry) return json(res, 401, { error: 'Invalid bootstrap token' });
    if (Date.now() > entry.expiresAt) return json(res, 401, { error: 'Bootstrap token expired' });
    if (entry.used) return json(res, 401, { error: 'Bootstrap token already used' });

    entry.used = true;
    entry.useCount = (entry.useCount || 0) + 1;
    entry.lastUsedAt = new Date().toISOString();
    state.saveBootstrapTokens();

    const agentName = body.agent || entry.agent;
    if (!agentName) return json(res, 400, { error: 'agent name required (in body or bootstrap token)' });

    const agents = await state.readAgents();
    let agentToken;
    if (agents[agentName]?.token) {
      agentToken = agents[agentName].token;
    } else {
      agentToken = `rcc-agent-${agentName}-${randomUUID().slice(0, 8)}`;
      agents[agentName] = {
        ...(agents[agentName] || {}),
        name: agentName,
        host: body.host || entry.host || 'unknown',
        type: entry.type || 'full',
        role: entry.role || 'agent',
        token: agentToken,
        registeredAt: new Date().toISOString(),
        capabilities: agents[agentName]?.capabilities || {},
        billing: agents[agentName]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
      };
      await state.writeAgents(agents);
      state.AUTH_TOKENS.add(agentToken);
    }

    console.log(`[rcc-api] POST /api/onboard: agent '${agentName}' registered from ${req.socket?.remoteAddress}`);
    return json(res, 200, {
      ok: true,
      agent: agentName,
      agentToken,
      rccUrl: state.RCC_PUBLIC_URL,
      squirrelbusToken: process.env.SQUIRRELBUS_TOKEN || null,
      role: entry.role || 'agent',
    });
  });

  // ── GET /api/onboard — shell script installer ─────────────────────────
  app.on('GET', '/api/onboard', async (req, res, _m, url) => {
    const token = url.searchParams.get('token');
    if (!token) {
      res.writeHead(400, { 'Content-Type': 'text/plain' });
      return res.end('# Error: token query param required\n# Usage: curl "http://RCC_HOST:8789/api/onboard?token=BOOTSTRAP_TOKEN" | bash\n');
    }
    const entry = state.bootstrapTokens.get(token);
    if (!entry) {
      res.writeHead(401, { 'Content-Type': 'text/plain' });
      return res.end('# Error: Invalid or expired bootstrap token\n# Generate a new one: POST /api/bootstrap/token {"agent":"<name>","role":"vllm-worker"}\n');
    }
    if (Date.now() > entry.expiresAt) {
      res.writeHead(401, { 'Content-Type': 'text/plain' });
      return res.end('# Error: Bootstrap token expired\n# Generate a new one: POST /api/bootstrap/token {"agent":"<name>","role":"vllm-worker"}\n');
    }
    const maxUses = entry.maxUses || 3;
    const useCount = entry.useCount || 0;
    if (useCount >= maxUses) {
      res.writeHead(401, { 'Content-Type': 'text/plain' });
      return res.end('# Error: Bootstrap token exhausted (max uses reached)\n');
    }
    const roleHint = entry.role || 'auto';

    // Load agent token (reuse existing if resurrection)
    const agents = await state.readAgents();
    let agentToken;
    if (agents[entry.agent]?.token) {
      agentToken = agents[entry.agent].token;
    } else {
      agentToken = `rcc-agent-${entry.agent}-${randomUUID().slice(0, 8)}`;
      agents[entry.agent] = {
        ...(agents[entry.agent] || {}),
        name: entry.agent,
        host: entry.host || 'unknown',
        type: entry.type || 'full',
        token: agentToken,
        registeredAt: new Date().toISOString(),
        capabilities: agents[entry.agent]?.capabilities || {},
        billing: agents[entry.agent]?.billing || { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
      };
      await state.writeAgents(agents);
      state.AUTH_TOKENS.add(agentToken);
    }

    // Load secrets
    const secretsPath = new URL('../../data/secrets.json', import.meta.url).pathname;
    let secrets = {};
    if (existsSync(secretsPath)) {
      try { secrets = JSON.parse(await readFile(secretsPath, 'utf8')); } catch {}
    }

    // Load deploy key
    const keyPath = new URL('../../data/github-key.json', import.meta.url).pathname;
    let repoUrl = 'https://github.com/jordanhubbard/rockyandfriends.git';
    let deployKeyBlock = '';
    if (existsSync(keyPath)) {
      try {
        const kr = JSON.parse(await readFile(keyPath, 'utf8'));
        repoUrl = kr.repoUrl || repoUrl;
        if (kr.deployKey) {
          deployKeyBlock = `
# ── Deploy key ───────────────────────────────────────────────────────────────
mkdir -p ~/.ssh && chmod 700 ~/.ssh
cat > ~/.ssh/rcc-deploy-key << 'DEPLOYKEY'
${kr.deployKey.trim()}
DEPLOYKEY
chmod 600 ~/.ssh/rcc-deploy-key
grep -q "rcc-deploy-key" ~/.ssh/config 2>/dev/null || cat >> ~/.ssh/config << 'SSHCFG'
Host github.com
  IdentityFile ~/.ssh/rcc-deploy-key
  StrictHostKeyChecking no
SSHCFG
`;
        }
      } catch {}
    }

    // Build env block from secrets
    const envLines = [`RCC_AGENT_TOKEN=${agentToken}`, `RCC_URL=${state.RCC_PUBLIC_URL}`, `AGENT_NAME=${entry.agent}`, `AGENT_ROLE=${roleHint}`];
    const skipKeys = new Set(['deployKey', 'repoUrl']);
    for (const [k, v] of Object.entries(secrets)) {
      if (!skipKeys.has(k) && v && typeof v !== 'object') {
        const envKey = k.replace(/[^a-zA-Z0-9_]/g, '_').toUpperCase();
        envLines.push(`${envKey}=${v}`);
      }
    }
    const envBlock = envLines.join('\n');
    const rccHost = state.RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0];
    const TUNNEL_USER = process.env.TUNNEL_USER || 'rcc-tunnel';

    const script = `#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════
# RCC Agent Onboard — ${entry.agent} @ \$(date -u +%Y-%m-%dT%H:%M:%SZ)
# Generated by Claw Command Center (revamp v2)
# Usage: curl "${state.RCC_PUBLIC_URL}/api/onboard?token=<token>" | bash
# ═══════════════════════════════════════════════════════════════════════════════
set -euo pipefail

AGENT_NAME="${entry.agent}"
ROLE_HINT="${roleHint}"
RCC_URL="${state.RCC_PUBLIC_URL}"
RCC_HOST="${rccHost}"
REPO_URL="${repoUrl}"
WORKSPACE="\$HOME/Src/rockyandfriends"

echo ""
echo "🐿️  RCC Agent Onboard — \$AGENT_NAME"
echo "    RCC: \$RCC_URL"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 1 — HARDWARE DISCOVERY
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 1: Hardware Discovery                            │"
echo "└─────────────────────────────────────────────────────────┘"

HAS_GPU=false
GPU_COUNT=0
GPU_MODEL="none"
GPU_VRAM_GB=0
if command -v nvidia-smi &>/dev/null; then
  GPU_COUNT=\$(nvidia-smi --list-gpus 2>/dev/null | wc -l || echo 0)
  if [ "\$GPU_COUNT" -gt 0 ]; then
    HAS_GPU=true
    GPU_MODEL=\$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1 | xargs || echo "unknown")
    GPU_VRAM_GB=\$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | awk '{sum+=\$1} END {printf "%d", sum/1024}' || echo 0)
  fi
fi

HAS_TAILSCALE=false
TAILSCALE_IP=""
if command -v tailscale &>/dev/null; then
  TS_STATUS=\$(tailscale status --json 2>/dev/null || true)
  if [ -n "\$TS_STATUS" ]; then
    TS_IP=\$(echo "\$TS_STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); ips=d.get('TailscaleIPs',[]); print(next((i for i in ips if i.startswith('100.')),ips[0] if ips else ''))" 2>/dev/null || true)
    if [ -n "\$TS_IP" ]; then
      HAS_TAILSCALE=true
      TAILSCALE_IP="\$TS_IP"
    fi
  fi
fi

AGENT_HOSTNAME=\$(hostname)
DISK_AVAIL=\$(df -BG /home 2>/dev/null | awk 'NR==2{print \$4}' || echo "unknown")

echo "  Hostname:     \$AGENT_HOSTNAME"
echo "  GPUs:         \$GPU_COUNT x \$GPU_MODEL (\${GPU_VRAM_GB}GB total VRAM)"
echo "  Tailscale:    \$HAS_TAILSCALE (\${TAILSCALE_IP:-not detected})"
echo "  Disk (home):  \$DISK_AVAIL available"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 2 — ROLE SELECTION
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 2: Role Selection                                │"
echo "└─────────────────────────────────────────────────────────┘"

AGENT_ROLE="agent"
ROLE_REASON="no GPU detected"

if [ "\$ROLE_HINT" = "vllm-worker" ]; then
  AGENT_ROLE="vllm-worker"
  ROLE_REASON="role override from bootstrap token"
elif [ "\$ROLE_HINT" = "agent" ]; then
  AGENT_ROLE="agent"
  ROLE_REASON="role override from bootstrap token"
elif [ "\$HAS_GPU" = "true" ]; then
  if [ "\$GPU_VRAM_GB" -ge 64 ]; then
    AGENT_ROLE="vllm-worker"
    ROLE_REASON="auto-detected \${GPU_VRAM_GB}GB VRAM >= 64GB threshold"
  else
    AGENT_ROLE="gpu-agent"
    ROLE_REASON="auto-detected GPU but \${GPU_VRAM_GB}GB VRAM < 64GB (insufficient for vLLM)"
  fi
fi

echo "  Selected role: \$AGENT_ROLE"
echo "  Reason:        \$ROLE_REASON"
echo ""
${deployKeyBlock}
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 3 — SYSTEM DEPS
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 3: System Dependencies                           │"
echo "└─────────────────────────────────────────────────────────┘"

echo "→ Checking Node.js..."
if ! node --version 2>/dev/null | grep -qE '^v(18|20|22|24)'; then
  echo "  Installing Node.js 22 via NodeSource..."
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q
  sudo apt-get install -y -q curl git || true
  curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
  sudo apt-get install -y nodejs
  echo "  ✅ Node.js \$(node --version) installed"
else
  echo "  ✅ Node.js \$(node --version) OK"
fi
if ! command -v git &>/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q && sudo apt-get install -y -q git
fi

if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
  echo "→ Installing vLLM worker deps..."
  export DEBIAN_FRONTEND=noninteractive
  sudo apt-get update -q
  sudo apt-get install -y -q aria2 rsync python3-pip python3-venv tmux curl wget git openssh-client || true
fi
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 4 — REPO + ENV
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 4: Workspace + Environment                       │"
echo "└─────────────────────────────────────────────────────────┘"

if [ -d "\$WORKSPACE/.git" ]; then
  echo "→ Repo exists — pulling latest..."
  cd "\$WORKSPACE" && git fetch origin && git reset --hard origin/main
else
  echo "→ Cloning repo..."
  mkdir -p "\$(dirname \$WORKSPACE)"
  git clone "\$REPO_URL" "\$WORKSPACE"
  cd "\$WORKSPACE"
fi
PULL_REV=\$(git rev-parse --short HEAD)
echo "   rev: \$PULL_REV"

echo "→ Writing ~/.rcc/.env..."
mkdir -p ~/.rcc
cat > ~/.rcc/.env << 'ENVEOF'
${envBlock}
ENVEOF
cat >> ~/.rcc/.env << HWEOF
AGENT_HOST=\$AGENT_HOSTNAME
AGENT_ROLE=\$AGENT_ROLE
HAS_GPU=\$HAS_GPU
GPU_MODEL="\$GPU_MODEL"
GPU_COUNT=\$GPU_COUNT
GPU_VRAM_GB=\$GPU_VRAM_GB
HAS_TAILSCALE=\$HAS_TAILSCALE
TAILSCALE_IP=\$TAILSCALE_IP
HWEOF
chmod 600 ~/.rcc/.env

set +u
source ~/.rcc/.env
set -u
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 5 — OPENCLAW INSTALL + SUPERVISORD SETUP
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 5: OpenClaw Install                              │"
echo "└─────────────────────────────────────────────────────────┘"

if command -v openclaw &>/dev/null; then
  echo "→ openclaw found — repairing config and restarting gateway..."
  if [ -f ~/.openclaw/openclaw.json ] && command -v node &>/dev/null; then
    node -e "
      const fs = require('fs');
      const p = process.env.HOME + '/.openclaw/openclaw.json';
      try {
        const cfg = JSON.parse(fs.readFileSync(p, 'utf8'));
        if (cfg.channels && cfg.channels.mattermost) {
          const mm = cfg.channels.mattermost;
          cfg.channels.mattermost = {
            ...(mm.enabled !== undefined ? { enabled: mm.enabled } : {}),
            ...(mm.botToken ? { botToken: mm.botToken } : {}),
            ...(mm.baseUrl ? { baseUrl: mm.baseUrl } : {}),
            ...(mm.accounts ? { accounts: mm.accounts } : {}),
          };
        }
        fs.writeFileSync(p, JSON.stringify(cfg, null, 2));
        console.log('  ✅ openclaw.json patched');
      } catch(e) { console.log('  ⚠️  openclaw.json patch skipped:', e.message); }
    " 2>/dev/null || true
  fi
  openclaw config set gateway.mode local 2>/dev/null || true
  if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
    openclaw config set gateway.pollOnly true 2>/dev/null || true
    openclaw config set heartbeat.interval 60 2>/dev/null || true
  fi
  openclaw gateway restart 2>/dev/null || openclaw gateway start
else
  echo "→ Installing openclaw..."
  curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash -
  sudo apt-get install -y nodejs
  sudo npm install -g openclaw || { echo "ERROR: npm install -g openclaw failed"; exit 1; }
  openclaw config set gateway.mode local 2>/dev/null || true
  if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
    openclaw config set gateway.pollOnly true 2>/dev/null || true
    openclaw config set heartbeat.interval 60 2>/dev/null || true
  fi
  openclaw gateway start
fi
echo ""

if [ "\$AGENT_ROLE" = "vllm-worker" ] && command -v supervisorctl &>/dev/null; then
  echo "→ Wiring openclaw-gateway into supervisord..."
  OPENCLAW_BIN=\$(command -v openclaw)
  SUPCONF_OC="\$HOME/.config/supervisord.d/openclaw-gateway.conf"
  mkdir -p "\$(dirname \$SUPCONF_OC)"
  cat > "\$SUPCONF_OC" << OCEOF
[program:openclaw-gateway]
command=\${OPENCLAW_BIN} gateway start --foreground
autostart=true
autorestart=true
startsecs=5
startretries=999
environment=HOME="\$(echo \$HOME)",RCC_AGENT_TOKEN="\$RCC_AGENT_TOKEN"
stdout_logfile=\$HOME/.rcc/logs/openclaw-gateway.log
stderr_logfile=\$HOME/.rcc/logs/openclaw-gateway.log
OCEOF
  mkdir -p "\$HOME/.rcc/logs"
  supervisorctl reread 2>/dev/null && supervisorctl update 2>/dev/null && \\
    echo "  ✅ openclaw-gateway registered with supervisord" || \\
    echo "  ⚠️  supervisorctl update failed — manual restart required"
fi
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 6 — REGISTRATION
# ═══════════════════════════════════════════════════════════════════════════════
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 6: RCC Registration                              │"
echo "└─────────────────────────────────────────────────────────┘"

IS_VLLM=false
VLLM_PORT=0
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
  IS_VLLM=true
  VLLM_PORT=8080
fi

REG_RESP=\$(curl -sf -X POST "\$RCC_URL/api/agents/register" \\
  -H "Authorization: Bearer \$RCC_AGENT_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d "{
    \\"name\\": \\"\$AGENT_NAME\\",
    \\"host\\": \\"\$AGENT_HOSTNAME\\",
    \\"type\\": \\"full\\",
    \\"capabilities\\": {
      \\"claude_cli\\": true,
      \\"inference_key\\": true,
      \\"inference_provider\\": \\"nvidia\\",
      \\"gpu\\": \$HAS_GPU,
      \\"gpu_model\\": \\"\$GPU_MODEL\\",
      \\"gpu_count\\": \$GPU_COUNT,
      \\"gpu_vram_gb\\": \$GPU_VRAM_GB,
      \\"vllm\\": \$IS_VLLM,
      \\"vllm_port\\": \$VLLM_PORT,
      \\"vllm_model\\": \\"nemotron-3-super-120b\\",
      \\"tailscale\\": \$HAS_TAILSCALE,
      \\"tailscale_ip\\": \\"\$TAILSCALE_IP\\"
    }
  }" 2>/dev/null) && echo "  ✅ Registered with RCC" || echo "  ⚠️  Registration returned error (may already exist — continuing)"

NEW_TOKEN=\$(echo "\$REG_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('token',''))" 2>/dev/null || true)
if [ -n "\$NEW_TOKEN" ] && [ "\$NEW_TOKEN" != "\$RCC_AGENT_TOKEN" ]; then
  sed -i "s|^RCC_AGENT_TOKEN=.*|RCC_AGENT_TOKEN=\$NEW_TOKEN|" ~/.rcc/.env
  RCC_AGENT_TOKEN="\$NEW_TOKEN"
  echo "  → Updated token in ~/.rcc/.env"
fi
echo ""
`;

    const vllmPhases = `
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 7 — VLLM SETUP (only if vllm-worker)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 7: vLLM Setup                                    │"
echo "└─────────────────────────────────────────────────────────┘"

VLLM_MODEL_ID="nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
VLLM_MODEL_DIR="/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
VLLM_PORT=8080

python3 -m venv "\$HOME/.vllm-venv"
source "\$HOME/.vllm-venv/bin/activate"
pip install --upgrade pip --quiet
pip install vllm --quiet || pip install vllm --quiet --extra-index-url https://download.pytorch.org/whl/cu121
pip install huggingface_hub --quiet
deactivate
echo "  ✅ vLLM installed"

mkdir -p "\$VLLM_MODEL_DIR"
if [ ! -f "\$VLLM_MODEL_DIR/config.json" ]; then
  echo "  → Downloading from HuggingFace..."
  source "\$HOME/.vllm-venv/bin/activate"
  python3 -c "
from huggingface_hub import snapshot_download
snapshot_download(repo_id='nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8',
  local_dir='\$VLLM_MODEL_DIR', local_dir_use_symlinks=False, resume_download=True)
print('Download complete')
"
  deactivate
fi
echo "  ✅ Model ready at \$VLLM_MODEL_DIR"
echo ""
fi  # end PHASE 7

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 8 — TUNNEL SETUP (vllm-worker WITHOUT tailscale only)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ] && [ "\$HAS_TAILSCALE" = "false" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 8: SSH Reverse Tunnel (no Tailscale detected)    │"
echo "└─────────────────────────────────────────────────────────┘"

TUNNEL_KEY="\$HOME/.ssh/rcc-tunnel-key"
if [ ! -f "\$TUNNEL_KEY" ]; then
  ssh-keygen -t ed25519 -f "\$TUNNEL_KEY" -N "" -C "\${AGENT_NAME}-vllm-tunnel"
fi

TUNNEL_PUBKEY=\$(cat "\${TUNNEL_KEY}.pub")
TUNNEL_RESP=\$(curl -sf -X POST "\${RCC_URL}/api/tunnel/request" \\
  -H "Authorization: Bearer \${RCC_AGENT_TOKEN}" \\
  -H "Content-Type: application/json" \\
  -d "{\\"pubkey\\":\\"\${TUNNEL_PUBKEY}\\",\\"agent\\":\\"\${AGENT_NAME}\\",\\"label\\":\\"\${AGENT_NAME}-vllm-tunnel\\"}" 2>/dev/null)
TUNNEL_PORT=\$(echo "\$TUNNEL_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('port',18082))" 2>/dev/null || echo "18082")
TUNNEL_USER_NAME=\$(echo "\$TUNNEL_RESP" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('user','${TUNNEL_USER}'))" 2>/dev/null || echo "${TUNNEL_USER}")
echo "  Tunnel port: \$TUNNEL_PORT"

HAS_SYSTEMD=false
HAS_SUPERVISORD=false
systemctl --user status 2>/dev/null | grep -q "systemd" && HAS_SYSTEMD=true || true
command -v supervisorctl &>/dev/null && HAS_SUPERVISORD=true || true

TUNNEL_CMD="ssh -N -R \${TUNNEL_PORT}:localhost:8080 -i \$HOME/.ssh/rcc-tunnel-key -o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -o ExitOnForwardFailure=yes \${TUNNEL_USER_NAME}@\${RCC_HOST}"

if [ "\$HAS_SYSTEMD" = "true" ]; then
  mkdir -p "\$HOME/.config/systemd/user"
  cat > "\$HOME/.config/systemd/user/rcc-vllm-tunnel.service" << TUNNELEOF
[Unit]
Description=RCC vLLM Reverse SSH Tunnel for \${AGENT_NAME}
After=network-online.target

[Service]
Type=simple
ExecStart=/usr/bin/ssh -N -R \${TUNNEL_PORT}:localhost:8080 -i \$HOME/.ssh/rcc-tunnel-key -o StrictHostKeyChecking=no -o ServerAliveInterval=30 -o ServerAliveCountMax=3 -o ExitOnForwardFailure=yes \${TUNNEL_USER_NAME}@\${RCC_HOST}
Restart=always
RestartSec=10

[Install]
WantedBy=default.target
TUNNELEOF
  systemctl --user daemon-reload && systemctl --user enable --now rcc-vllm-tunnel.service && \\
    echo "  ✅ Tunnel started (systemd user)" || echo "  ⚠️  systemd enable failed"
else
  mkdir -p "\$HOME/.rcc/logs"
  nohup bash -c "while true; do \${TUNNEL_CMD}; sleep 10; done" > "\$HOME/.rcc/logs/tunnel.log" 2>&1 &
  echo "  ✅ Tunnel started via nohup"
fi
echo ""
fi  # end PHASE 8

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 9 — VLLM SERVICE (vllm-worker only)
# ═══════════════════════════════════════════════════════════════════════════════
if [ "\$AGENT_ROLE" = "vllm-worker" ]; then
echo "┌─────────────────────────────────────────────────────────┐"
echo "│  Phase 9: vLLM Service                                  │"
echo "└─────────────────────────────────────────────────────────┘"

VLLM_VENV="\$HOME/.vllm-venv"
VLLM_MODEL_DIR="/tmp/models/nvidia/NVIDIA-Nemotron-3-Super-120B-A12B-FP8"
TP_SIZE=\$(nvidia-smi --list-gpus 2>/dev/null | wc -l || echo 1)
VLLM_START_CMD="\$VLLM_VENV/bin/python3 -m vllm.entrypoints.openai.api_server --model \$VLLM_MODEL_DIR --served-model-name nemotron --port 8080 --tensor-parallel-size \$TP_SIZE --max-model-len 262144 --enforce-eager --trust-remote-code"

mkdir -p "\$HOME/.rcc/logs"
nohup bash -c "\${VLLM_START_CMD}" > "\$HOME/.rcc/logs/vllm.log" 2>&1 &
echo "  ✅ vLLM started (pid \$!)"
echo ""
fi  # end PHASE 9
`;

    const verifyAndSummary = `
# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 10 — HEARTBEAT
# ═══════════════════════════════════════════════════════════════════════════════
echo "→ Posting heartbeat..."
sleep 2
curl -s -X POST "\$RCC_URL/api/heartbeat/\$AGENT_NAME" \\
  -H "Authorization: Bearer \$RCC_AGENT_TOKEN" \\
  -H "Content-Type: application/json" \\
  -d "{\\"agent\\":\\"\$AGENT_NAME\\",\\"role\\":\\"\$AGENT_ROLE\\",\\"host\\":\\"\$AGENT_HOSTNAME\\",\\"status\\":\\"online\\",\\"pullRev\\":\\"\$PULL_REV\\"}" | grep -q '"ok":true' && echo "  ✅ Heartbeat posted" || echo "  ⚠️  Heartbeat failed"
echo ""

# ═══════════════════════════════════════════════════════════════════════════════
# PHASE 11 — SUMMARY
# ═══════════════════════════════════════════════════════════════════════════════
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ✅ \$AGENT_NAME is online."
echo "  Agent:   \$AGENT_NAME"
echo "  Role:    \$AGENT_ROLE"
echo "  Host:    \$AGENT_HOSTNAME"
echo "  Token:   \${RCC_AGENT_TOKEN:0:20}..."
echo "  Dashboard: \$RCC_URL"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
`;

    const fullScript = script + vllmPhases + verifyAndSummary;

    entry.useCount = (entry.useCount || 0) + 1;
    entry.used = entry.useCount >= (entry.maxUses || 3);
    entry.lastUsedAt = new Date().toISOString();
    state.saveBootstrapTokens();
    console.log(`[rcc-api] Onboard script generated for ${entry.agent} (role hint: ${roleHint}) use ${entry.useCount}/${entry.maxUses||3} from ${req.socket?.remoteAddress}`);
    res.writeHead(200, { 'Content-Type': 'text/plain; charset=utf-8' });
    return res.end(fullScript);
  });

  // ── GET /pkg — nano package registry browser UI (public) ─────────────
  app.on('GET', '/pkg', async (req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Access-Control-Allow-Origin': '*' });
    res.end(`<!DOCTYPE html><html lang="en"><head>
<meta charset="UTF-8"><meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>nano packages</title>
<style>
  *{box-sizing:border-box;margin:0;padding:0}
  body{font-family:'Courier New',monospace;background:#0d1117;color:#c9d1d9;min-height:100vh}
  .header{background:#161b22;border-bottom:1px solid #30363d;padding:16px 32px;display:flex;align-items:center;gap:16px}
  .header h1{font-size:1.3rem;color:#58a6ff}
  .header .subtitle{color:#8b949e;font-size:0.85rem}
  .nav{margin-left:auto;font-size:0.85rem}
  .nav a{color:#58a6ff;text-decoration:none;margin-left:16px}
  .nav a:hover{text-decoration:underline}
  .container{max-width:1100px;margin:0 auto;padding:24px 32px}
  .search-bar{width:100%;padding:10px 14px;background:#161b22;border:1px solid #30363d;border-radius:6px;color:#c9d1d9;font-family:inherit;font-size:0.95rem;margin-bottom:20px;outline:none}
  .search-bar:focus{border-color:#58a6ff}
  .stats{color:#8b949e;font-size:0.85rem;margin-bottom:16px}
  .pkg-grid{display:grid;gap:12px}
  .pkg-card{background:#161b22;border:1px solid #30363d;border-radius:8px;padding:16px 20px;transition:border-color 0.15s}
  .pkg-card:hover{border-color:#58a6ff}
  .pkg-name{font-size:1.05rem;color:#58a6ff;font-weight:bold}
  .pkg-version{color:#3fb950;font-size:0.85rem;margin-left:8px}
  .pkg-desc{color:#8b949e;font-size:0.875rem;margin:6px 0}
  .pkg-meta{display:flex;gap:12px;flex-wrap:wrap;margin-top:8px;font-size:0.8rem;color:#8b949e}
  .pkg-meta a{color:#58a6ff;text-decoration:none}
  .tag{background:#1f3447;color:#58a6ff;border-radius:12px;padding:2px 8px;font-size:0.75rem}
  .tag-row{display:flex;gap:6px;flex-wrap:wrap;margin-top:6px}
  .error{color:#f85149;padding:20px;text-align:center}
  .loading{color:#8b949e;padding:20px;text-align:center}
  .empty{color:#8b949e;padding:40px;text-align:center;font-size:0.9rem}
  .install-code{background:#0d1117;border:1px solid #30363d;border-radius:4px;padding:4px 8px;font-size:0.8rem;color:#3fb950;cursor:pointer}
  .install-code:hover{border-color:#3fb950}
</style></head><body>
<div class="header">
  <div><h1>🐿️ nano packages</h1>
  <div class="subtitle">nanolang package registry &mdash; <a href="https://github.com/jordanhubbard/nano-packages" style="color:#58a6ff;" target="_blank">jordanhubbard/nano-packages</a></div></div>
  <div class="nav"><a href="/">&#8592; RCC</a><a href="/services">Services</a><a href="https://github.com/jordanhubbard/nano-packages" target="_blank">GitHub</a></div>
</div>
<div class="container">
  <input class="search-bar" type="text" id="search" placeholder="Search packages by name, description, or keyword..." autofocus>
  <div class="stats" id="stats">Loading...</div>
  <div class="pkg-grid" id="grid"><div class="loading">Fetching registry...</div></div>
</div>
<script>
let allPackages=[];
async function loadPackages(){
  try{
    const r=await fetch('/api/pkg');const data=await r.json();
    if(data.error)throw new Error(data.error);
    allPackages=data.packages||[];render(allPackages);
    document.getElementById('stats').textContent=allPackages.length+' package'+(allPackages.length!==1?'s':'')+(data.cached?' (cached)':'')+(data.fetchedAt?' · updated '+new Date(data.fetchedAt).toLocaleTimeString():'');
  }catch(e){document.getElementById('stats').textContent='';document.getElementById('grid').innerHTML='<div class="error">Failed to load registry: '+e.message+'</div>';}
}
function render(pkgs){
  const grid=document.getElementById('grid');
  if(!pkgs.length){grid.innerHTML='<div class="empty">No packages found.</div>';return;}
  grid.innerHTML=pkgs.map(p=>{
    const deps=Object.keys(p.dependencies||{});const installCmd='nanoc-pkg install '+p.name;
    return '<div class="pkg-card"><div><span class="pkg-name">'+esc(p.name)+'</span><span class="pkg-version">v'+esc(p.version||'?')+'</span></div>'+(p.description?'<div class="pkg-desc">'+esc(p.description)+'</div>':'')+'<div class="pkg-meta">'+(p.author?'<span>by '+esc(p.author)+'</span>':'')+(p.license?'<span>'+esc(p.license)+'</span>':'')+(p.homepage?'<a href="'+esc(p.homepage)+'" target="_blank">homepage</a>':'')+(p.repository?'<a href="'+esc(p.repository)+'" target="_blank">source</a>':'')+'<span class="install-code" onclick="navigator.clipboard.writeText(\''+installCmd+'\')" title="Copy install command">'+esc(installCmd)+'</span></div>'+(deps.length?'<div class="tag-row">'+deps.map(d=>'<span class="tag">dep: '+esc(d)+'</span>').join('')+'</div>':'')+'</div>';
  }).join('');
}
function esc(s){return String(s||'').replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}
document.getElementById('search').addEventListener('input',e=>{
  const q=e.target.value.toLowerCase();if(!q){render(allPackages);return;}
  render(allPackages.filter(p=>(p.name||'').toLowerCase().includes(q)||(p.description||'').toLowerCase().includes(q)||(p.author||'').toLowerCase().includes(q)||Object.keys(p.dependencies||{}).some(d=>d.toLowerCase().includes(q))));
});
loadPackages();
</script></body></html>`);
    return;
  });

  // ── GET /sbom — SBOM registry HTML page ──────────────────────────────
  app.on('GET', '/sbom', async (req, res) => {
    try {
      const { readdir } = await import('fs/promises');
      const { join: pathJoin } = await import('path');
      const sbomDir = new URL('../../sbom', import.meta.url).pathname;
      const files = await readdir(sbomDir).catch(() => []);
      const sboms = [];
      for (const f of files) {
        if (!f.endsWith('.sbom.json')) continue;
        try {
          const raw = await readFile(pathJoin(sbomDir, f), 'utf8');
          sboms.push(JSON.parse(raw));
        } catch {}
      }
      const tableRows = sboms.map(s => {
        const apt = (s.packages?.apt || []).join(', ') || '-';
        const npm = (s.packages?.npm || []).join(', ') || '-';
        const pip = (s.packages?.pip || []).join(', ') || '-';
        const skills = (s.skills || []).join(', ') || '-';
        const envReq = (s.env_required || []).join(', ') || '-';
        return `<tr>
          <td><strong>${s.agent || f}</strong><br><small style="color:#8b949e">${s.description || ''}</small></td>
          <td>${s.platform || 'linux'}</td>
          <td style="font-size:0.8em">${apt}</td>
          <td style="font-size:0.8em">${npm}</td>
          <td style="font-size:0.8em">${pip}</td>
          <td style="font-size:0.8em">${skills}</td>
          <td style="font-size:0.8em">${envReq}</td>
          <td style="font-size:0.8em">${s.updated ? s.updated.split('T')[0] : '-'}</td>
        </tr>`;
      }).join('\n');
      const html = `<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><title>Agent SBOM Registry</title>
<style>body{font-family:monospace;background:#0d1117;color:#c9d1d9;margin:0;padding:24px}h1{color:#58a6ff;margin-bottom:4px}.subtitle{color:#8b949e;font-size:0.85rem;margin-bottom:20px}table{width:100%;border-collapse:collapse;font-size:0.85rem}th{background:#161b22;color:#8b949e;padding:8px 12px;text-align:left;border-bottom:2px solid #30363d}td{padding:8px 12px;border-bottom:1px solid #21262d;vertical-align:top}tr:hover td{background:#161b22}.nav{margin-bottom:16px;font-size:0.85rem}.nav a{color:#58a6ff;text-decoration:none;margin-right:16px}.install-cmd{background:#161b22;border:1px solid #30363d;border-radius:4px;padding:12px;margin-top:20px;font-size:0.8rem;color:#3fb950}</style>
</head><body>
<div class="nav"><a href="/">← Dashboard</a><a href="/api/sbom">JSON API</a></div>
<h1>🧾 Agent SBOM Registry</h1>
<p class="subtitle">Software Bill of Materials — declared dependencies for each agent node</p>
<table><thead><tr><th>Agent</th><th>Platform</th><th>APT</th><th>NPM</th><th>PIP</th><th>Skills</th><th>Env Required</th><th>Updated</th></tr></thead>
<tbody>${tableRows}</tbody></table>
<div class="install-cmd"><strong>Apply SBOM on a new node:</strong><br>curl -fsSL http://localhost:8789/api/sbom/&lt;agent&gt;/install | AGENT_NAME=&lt;agent&gt; bash</div>
</body></html>`;
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(html);
      return;
    } catch (e) {
      return json(res, 500, { error: e.message });
    }
  });
}
