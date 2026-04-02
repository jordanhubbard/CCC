/**
 * rcc/api/routes/agents.mjs — Agent-related route handlers
 * Extracted from api/index.mjs (structural refactor only — no logic changes)
 */

import { existsSync } from 'fs';
import { readFile, mkdir, appendFile } from 'fs/promises';

export default function registerRoutes(app, state) {
  const {
    json, readBody, readQueue, readAgents, writeAgents, readCapabilities, writeCapabilities,
    readJsonFile, writeJsonFile, isAuthed, isAdminAuthed,
    heartbeats, heartbeatHistory, offlineAlertSent, cronStatus, providerHealth,
    geekSseClients, bootstrapTokens, AUTH_TOKENS,
    computeOnlineStatus, broadcastGeekEvent,
    saveBootstrapTokens, getBrain, getPump,
    llmRegistry,
    RCC_PUBLIC_URL, TUNNEL_STATE_PATH, TUNNEL_PORT_START,
    START_TIME,
  } = state;

  // ── GET /api/agents ────────────────────────────────────────────────────
  app.on('GET', '/api/agents', async (req, res) => {
    const agents = await readAgents();
    const caps   = await readCapabilities();
    const result = Object.entries(agents).map(([name, agent]) => {
      const llmEntry = llmRegistry.get(name);
      return {
        ...agent,
        capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
        heartbeat: heartbeats[name] || null,
        llm: llmEntry ? {
          baseUrl:   llmEntry.baseUrl,
          backend:   llmEntry.backend,
          models:    llmEntry.models.map(m => m.name),
          modelCount: llmEntry.models.length,
          fresh:     (Date.now() - new Date(llmEntry.updatedAt).getTime()) < 30 * 60 * 1000,
          updatedAt: llmEntry.updatedAt,
        } : null,
      };
    });
    return json(res, 200, result);
  });

  // ── GET /api/agents/best ───────────────────────────────────────────────
  app.on('GET', '/api/agents/best', async (req, res, _m, url) => {
    const task = url.searchParams.get('task') || '';
    const agents = await readAgents();
    const caps   = await readCapabilities();
    const GPU_TASKS    = new Set(['gpu', 'render', 'training', 'inference']);
    const CLAUDE_TASKS = new Set(['claude', 'code', 'review', 'debug', 'triage']);
    const CTX_PRIORITY = { large: 3, medium: 2, small: 1 };

    const candidates = Object.entries(agents).map(([name, agent]) => ({
      name,
      ...agent,
      capabilities: { ...(agent.capabilities || {}), ...(caps[name] || {}) },
      heartbeat: heartbeats[name] || null,
    }));

    const onlineCutoff = Date.now() - 10 * 60 * 1000;
    const online = candidates.filter(a => a.heartbeat && new Date(a.heartbeat.ts).getTime() > onlineCutoff);
    const pool   = online.length > 0 ? online : candidates;

    let best = null;

    if (GPU_TASKS.has(task)) {
      const gpu = pool.filter(a => a.capabilities?.gpu);
      if (gpu.length) best = gpu.sort((a, b) => (b.capabilities.gpu_vram_gb || 0) - (a.capabilities.gpu_vram_gb || 0))[0];
    } else if (CLAUDE_TASKS.has(task)) {
      const cli = pool.filter(a => a.capabilities?.claude_cli);
      if (cli.length) best = cli.sort((a, b) => (CTX_PRIORITY[b.capabilities.context_size] || 0) - (CTX_PRIORITY[a.capabilities.context_size] || 0))[0];
    }

    if (!best) {
      const byPref = pool.filter(a => (a.capabilities?.preferred_tasks || []).includes(task));
      if (byPref.length) best = byPref[0];
    }

    if (!best && pool.length) best = pool[0];
    if (!best) return json(res, 404, { error: 'No agents available' });
    return json(res, 200, { agent: best, task });
  });

  // ── GET /api/agents/status ─────────────────────────────────────────────
  app.on('GET', '/api/agents/status', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const agents = await readAgents().catch(() => ({}));
    const now = Date.now();
    const result = Object.entries(agents).map(([name, agent]) => {
      const hb = heartbeats[name] || null;
      const lastSeen = hb?.ts || agent.lastSeen || null;
      const gapMs = lastSeen ? now - new Date(lastSeen).getTime() : null;
      const gap_minutes = gapMs !== null ? Math.round(gapMs / 60000) : null;
      const onlineStatus = agent.decommissioned ? 'decommissioned'
        : (hb ? (computeOnlineStatus(hb) ? 'online' : 'offline') : (agent.onlineStatus || 'unknown'));
      return {
        name,
        lastSeen,
        onlineStatus,
        host: hb?.host || agent.host || null,
        gap_minutes,
      };
    });
    return json(res, 200, { ok: true, agents: result });
  });

  // ── GET /api/agents/:name/tunnel-port ──────────────────────────────────
  app.on('GET', /^\/api\/agents\/([^/]+)\/tunnel-port$/, async (req, res, m) => {
    const agentName = decodeURIComponent(m[1]);
    const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { nextPort: TUNNEL_PORT_START, tunnels: {} });
    let assigned = tunnelState.tunnels[agentName];
    if (!assigned) {
      const port = tunnelState.nextPort;
      tunnelState.nextPort = port + 1;
      assigned = { agent: agentName, port, pubkey: null, preallocatedAt: new Date().toISOString() };
      tunnelState.tunnels[agentName] = assigned;
      await writeJsonFile(TUNNEL_STATE_PATH, tunnelState);
    }
    const publicHost = RCC_PUBLIC_URL.replace(/^https?:\/\//, '').split(':')[0];
    return json(res, 200, { ok: true, port: assigned.port, host: publicHost, agent: agentName });
  });

  // ── GET /api/heartbeats ────────────────────────────────────────────────
  app.on('GET', '/api/heartbeats', async (req, res) => {
    const agents = await readAgents().catch(() => ({}));
    const result = { ...heartbeats };
    for (const [name, agentRec] of Object.entries(agents)) {
      if (!result[name] && agentRec.lastSeen) {
        result[name] = { agent: name, ts: agentRec.lastSeen, status: agentRec.onlineStatus || 'unknown', _fromRegistry: true };
        if (agentRec.decommissioned) result[name].decommissioned = true;
      }
    }
    const enriched = {};
    for (const [name, hb] of Object.entries(result)) {
      enriched[name] = {
        ...hb,
        online: computeOnlineStatus(hb),
        decommissioned: !!hb.decommissioned,
        lastSeen: hb.ts || null,
      };
      if (hb._wasOnline !== undefined) delete enriched[name]._wasOnline;
      if (hb._fromRegistry !== undefined) delete enriched[name]._fromRegistry;
    }
    return json(res, 200, enriched);
  });

  // ── GET /api/drift ─────────────────────────────────────────────────────
  app.on('GET', '/api/drift', async (req, res, _m, url) => {
    try {
      const { detectDrift, driftReport } = await import('../decision-journal/intent-drift-detector.mjs');
      const { DecisionJournal } = await import('../decision-journal/index.mjs');
      const agentFilter = url.searchParams.get('agent') || null;
      const windowSize  = parseInt(url.searchParams.get('window')    || '20', 10);
      const baselineWin = parseInt(url.searchParams.get('baseline')  || '50', 10);
      const threshold   = parseFloat(url.searchParams.get('threshold') || '0.25');
      const logPath = process.env.DECISION_JOURNAL_PATH ||
        new URL('../../logs/decision-journal.jsonl', import.meta.url).pathname;
      const journal = new DecisionJournal({ agent: agentFilter || '_rcc', logPath, silent: true });
      const result = detectDrift({ journal, agent: agentFilter, windowSize, baselineWindow: baselineWin, driftThreshold: threshold });
      return json(res, 200, { ...result, report: driftReport(result) });
    } catch (err) {
      return json(res, 500, { ok: false, error: err.message });
    }
  });

  // ── GET /api/brain/status ──────────────────────────────────────────────
  app.on('GET', '/api/brain/status', async (req, res) => {
    const b = state.brain;
    if (!b) return json(res, 200, { ok: true, status: 'not started' });
    return json(res, 200, b.getStatus());
  });

  // ── POST /api/brain/request ────────────────────────────────────────────
  app.on('POST', '/api/brain/request', async (req, res) => {
    const body = await readBody(req);
    if (!body.messages || !Array.isArray(body.messages)) return json(res, 400, { error: 'messages array required' });
    const b = await getBrain();
    const { createRequest } = state;
    const req2 = createRequest({
      messages: body.messages,
      maxTokens: body.maxTokens || 1024,
      priority: body.priority || 'normal',
      callbackUrl: body.callbackUrl || null,
      metadata: body.metadata || {},
    });
    const id = await b.enqueue(req2);
    return json(res, 202, { ok: true, requestId: id, status: 'queued' });
  });

  // ── POST /api/agents/register ──────────────────────────────────────────
  app.on('POST', '/api/agents/register', async (req, res) => {
    const body = await readBody(req);
    if (!body.name) return json(res, 400, { error: 'name required' });
    const agents = await readAgents();
    const existingToken = agents[body.name]?.token;
    const token = existingToken || `rcc-agent-${body.name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
    const tailscaleIp = body.capabilities?.tailscale_ip || body.tailscale_ip || null;
    const hasTailscale = body.capabilities?.tailscale ?? (tailscaleIp != null);
    const hasVllm = body.capabilities?.vllm ?? false;
    const vllmPort = body.capabilities?.vllm_port || 8080;
    agents[body.name] = {
      ...(agents[body.name] || {}),
      name: body.name,
      host: body.host || agents[body.name]?.host || 'unknown',
      type: body.type || agents[body.name]?.type || 'full',
      token,
      registeredAt: agents[body.name]?.registeredAt || new Date().toISOString(),
      lastSeen: agents[body.name]?.lastSeen || null,
      capabilities: {
        ...(agents[body.name]?.capabilities || {}),
        claude_cli: body.capabilities?.claude_cli ?? agents[body.name]?.capabilities?.claude_cli ?? true,
        claude_cli_model: body.capabilities?.claude_cli_model || agents[body.name]?.capabilities?.claude_cli_model || null,
        inference_key: body.capabilities?.inference_key ?? agents[body.name]?.capabilities?.inference_key ?? true,
        inference_provider: body.capabilities?.inference_provider || agents[body.name]?.capabilities?.inference_provider || 'nvidia',
        gpu: body.capabilities?.gpu ?? agents[body.name]?.capabilities?.gpu ?? false,
        gpu_model: body.capabilities?.gpu_model || agents[body.name]?.capabilities?.gpu_model || null,
        gpu_count: body.capabilities?.gpu_count ?? agents[body.name]?.capabilities?.gpu_count ?? 0,
        gpu_vram_gb: body.capabilities?.gpu_vram_gb ?? agents[body.name]?.capabilities?.gpu_vram_gb ?? 0,
        vllm: hasVllm || agents[body.name]?.capabilities?.vllm || false,
        vllm_port: vllmPort,
        vllm_model: body.capabilities?.vllm_model || agents[body.name]?.capabilities?.vllm_model || null,
        tailscale: hasTailscale || agents[body.name]?.capabilities?.tailscale || false,
        tailscale_ip: tailscaleIp || agents[body.name]?.capabilities?.tailscale_ip || null,
        model_seeder: body.capabilities?.model_seeder ?? agents[body.name]?.capabilities?.model_seeder ?? false,
        model_seeder_port: body.capabilities?.model_seeder_port ?? agents[body.name]?.capabilities?.model_seeder_port ?? null,
      },
      billing: {
        claude_cli: body.billing?.claude_cli || agents[body.name]?.billing?.claude_cli || 'fixed',
        inference_key: body.billing?.inference_key || agents[body.name]?.billing?.inference_key || 'metered',
        gpu: body.billing?.gpu || agents[body.name]?.billing?.gpu || 'fixed',
      },
    };

    const effectiveTailscaleIp = agents[body.name].capabilities.tailscale_ip;
    if (effectiveTailscaleIp && hasVllm) {
      agents[body.name].llm = {
        baseUrl: `http://${effectiveTailscaleIp}:${vllmPort}/v1`,
        backend: 'vllm',
        models: ['nemotron'],
        modelCount: 1,
        fresh: false,
        updatedAt: new Date().toISOString(),
      };
      console.log(`[rcc-api] ${body.name} registered with Tailscale vLLM at ${effectiveTailscaleIp}:${vllmPort}`);
    }

    await writeAgents(agents);
    AUTH_TOKENS.add(token);

    if (agents[body.name].capabilities.vllm && process.env.TOKENHUB_URL && process.env.TOKENHUB_ADMIN_TOKEN) {
      let providerUrl = null;
      if (effectiveTailscaleIp) {
        providerUrl = `http://${effectiveTailscaleIp}:${vllmPort}`;
      } else {
        const tunnelState = await readJsonFile(TUNNEL_STATE_PATH, { tunnels: {} });
        const tunnel = Object.values(tunnelState.tunnels).find(t => t.agent === body.name || t.agent?.toLowerCase() === body.name?.toLowerCase());
        if (tunnel?.port) providerUrl = `http://127.0.0.1:${tunnel.port}`;
      }

      if (providerUrl) {
        const providerId = `${body.name.toLowerCase()}-vllm`;
        try {
          await fetch(`${process.env.TOKENHUB_URL}/admin/v1/providers`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${process.env.TOKENHUB_ADMIN_TOKEN}` },
            body: JSON.stringify({ id: providerId, type: 'vllm', base_url: providerUrl, api_key: 'none', enabled: true }),
          });
          await fetch(`${process.env.TOKENHUB_URL}/admin/v1/models`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${process.env.TOKENHUB_ADMIN_TOKEN}` },
            body: JSON.stringify({ id: `nemotron-${body.name.toLowerCase()}`, provider_id: providerId, weight: 8, max_context_tokens: 262144, enabled: true }),
          });
          console.log(`[rcc-api] Registered ${body.name} as TokenHub provider ${providerId} → ${providerUrl}`);
        } catch (thErr) {
          console.warn(`[rcc-api] TokenHub registration failed for ${body.name}: ${thErr.message}`);
        }
      } else {
        console.log(`[rcc-api] ${body.name} has vLLM but no tunnel/tailscale assigned yet — skipping TokenHub registration`);
      }
    }

    return json(res, 201, { ok: true, token, agent: { ...agents[body.name], token } });
  });

  // ── POST /api/agents/:name (upsert) ───────────────────────────────────
  app.on('POST', /^\/api\/agents\/([^/]+)$/, async (req, res, m) => {
    const name = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agents = await readAgents();
    if (!agents[name]) {
      const token = `rcc-agent-${name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
      agents[name] = {
        name,
        host: body.host || 'unknown',
        type: body.type || 'full',
        token,
        registeredAt: new Date().toISOString(),
        lastSeen: null,
        capabilities: {},
        billing: { claude_cli: 'fixed', inference_key: 'metered', gpu: 'fixed' },
      };
      AUTH_TOKENS.add(token);
    } else {
      if (body.host) agents[name].host = body.host;
      if (body.type) agents[name].type = body.type;
    }
    await writeAgents(agents);
    if (body.capabilities) {
      const caps = await readCapabilities();
      caps[name] = { ...(caps[name] || {}), ...body.capabilities };
      await writeCapabilities(caps);
    }
    return json(res, 200, { ok: true, token: agents[name].token, agent: agents[name] });
  });

  // ── PATCH /api/agents/:name ────────────────────────────────────────────
  app.on('PATCH', /^\/api\/agents\/([^/]+)$/, async (req, res, m) => {
    const name = decodeURIComponent(m[1]);
    const body = await readBody(req);
    const agents = await readAgents();
    if (!agents[name]) return json(res, 404, { error: 'Agent not found' });
    if (body.capabilities) Object.assign(agents[name].capabilities || {}, body.capabilities);
    if (body.billing) Object.assign(agents[name].billing || {}, body.billing);
    if (body.host) agents[name].host = body.host;
    if (body.type) agents[name].type = body.type;
    if (body.status === 'decommissioned') {
      agents[name].decommissioned = true;
      agents[name].decommissionedAt = new Date().toISOString();
      agents[name].onlineStatus = 'decommissioned';
      if (heartbeats[name]) heartbeats[name].decommissioned = true;
    } else if (body.status === 'active') {
      delete agents[name].decommissioned;
      delete agents[name].decommissionedAt;
      agents[name].onlineStatus = 'unknown';
      if (heartbeats[name]) delete heartbeats[name].decommissioned;
    }
    await writeAgents(agents);
    if (body.capabilities) {
      const caps = await readCapabilities();
      caps[name] = { ...(caps[name] || {}), ...body.capabilities };
      await writeCapabilities(caps);
    }
    return json(res, 200, { ok: true, agent: agents[name] });
  });

  // ── POST /api/heartbeat/:agent ─────────────────────────────────────────
  app.on('POST', /^\/api\/heartbeat\/([^/]+)$/, async (req, res, m) => {
    const agent = decodeURIComponent(m[1]).toLowerCase();
    const body = await readBody(req);
    const ts = new Date().toISOString();
    heartbeats[agent] = { agent, ts, status: 'online', ...body, _wasOnline: true };
    if (!heartbeatHistory[agent]) heartbeatHistory[agent] = [];
    const hbEntry = { ts, status: 'online', host: body.host || null };
    heartbeatHistory[agent].push(hbEntry);
    if (heartbeatHistory[agent].length > 288) heartbeatHistory[agent].shift();
    const histDir = new URL('../data/heartbeat-history', import.meta.url).pathname;
    mkdir(histDir, { recursive: true }).then(() => {
      const histFile = `${histDir}/${agent}.jsonl`;
      const line = JSON.stringify({ ts, agent, host: body.host || null, status: 'online' }) + '\n';
      return import('fs').then(fsmod => {
        const { appendFileSync } = fsmod;
        appendFileSync(histFile, line);
      });
    }).catch(() => {});
    const agents = await readAgents();
    if (agents[agent]) {
      agents[agent].lastSeen = ts;
      agents[agent].onlineStatus = 'online';
      await writeAgents(agents);
    }
    delete offlineAlertSent[agent];
    broadcastGeekEvent('heartbeat', agent, 'rocky', `${agent} heartbeat`);
    const scoutQ = await readQueue().catch(() => ({ items: [] }));
    const pendingWork = (scoutQ.items || [])
      .filter(i => i.status === 'pending' && (i.assignee === agent || i.assignee === 'all'))
      .slice(0, 3)
      .map(({ id, title, priority, description }) => ({ id, title, priority, description }));
    return json(res, 200, { ok: true, pendingWork });
  });

  // ── GET /api/heartbeat/:agent/history ──────────────────────────────────
  app.on('GET', /^\/api\/heartbeat\/([^/]+)\/history$/, async (req, res, m) => {
    const agent = decodeURIComponent(m[1]);
    try {
      const histFile = new URL(`../data/heartbeat-history/${agent}.jsonl`, import.meta.url).pathname;
      if (existsSync(histFile)) {
        const content = await readFile(histFile, 'utf8');
        const lines = content.trim().split('\n').filter(Boolean);
        const entries = lines.slice(-100).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        return json(res, 200, entries);
      }
    } catch {}
    return json(res, 200, heartbeatHistory[agent] || []);
  });

  // ── GET /api/agents/history/:name ──────────────────────────────────────
  app.on('GET', /^\/api\/agents\/history\/([^/]+)$/, async (req, res, m, url) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const name = decodeURIComponent(m[1]);
    const limit = Math.min(parseInt(url.searchParams.get('limit') || '50', 10), 500);
    let entries = [];
    try {
      const histFile = new URL(`../data/heartbeat-history/${name}.jsonl`, import.meta.url).pathname;
      if (existsSync(histFile)) {
        const content = await readFile(histFile, 'utf8');
        const lines = content.trim().split('\n').filter(Boolean);
        entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
      } else {
        entries = (heartbeatHistory[name] || []).slice(-limit);
      }
    } catch {}
    return json(res, 200, { ok: true, agent: name, entries });
  });

  // ── GET /api/heartbeat-history ─────────────────────────────────────────
  app.on('GET', '/api/heartbeat-history', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    return json(res, 200, heartbeatHistory);
  });

  // ── POST /api/agents/:name/events ──────────────────────────────────────
  app.on('POST', /^\/api\/agents\/([^/]+)\/events$/, async (req, res, m) => {
    const name = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body.event) return json(res, 400, { error: 'event required' });
    const eventEntry = {
      ts: new Date().toISOString(),
      agent: name,
      event: body.event,
      detail: body.detail || null,
      pullRev: body.pullRev || null,
    };
    const histDir = new URL('../data/agent-history', import.meta.url).pathname;
    await mkdir(histDir, { recursive: true });
    const histFile = `${histDir}/${name}.jsonl`;
    await appendFile(histFile, JSON.stringify(eventEntry) + '\n', 'utf8');
    return json(res, 201, { ok: true, event: eventEntry });
  });

  // ── GET /api/agents/:name/history ──────────────────────────────────────
  app.on('GET', /^\/api\/agents\/([^/]+)\/history$/, async (req, res, m, url) => {
    const name = decodeURIComponent(m[1]);
    const limit = Math.min(parseInt(url.searchParams.get('limit') || '100', 10), 500);
    let entries = [];
    try {
      const histFile = new URL(`../data/agent-history/${name}.jsonl`, import.meta.url).pathname;
      if (existsSync(histFile)) {
        const content = await readFile(histFile, 'utf8');
        const lines = content.trim().split('\n').filter(Boolean);
        entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
      }
      if (entries.length === 0) {
        const hbFile = new URL(`../data/heartbeat-history/${name}.jsonl`, import.meta.url).pathname;
        if (existsSync(hbFile)) {
          const content = await readFile(hbFile, 'utf8');
          const lines = content.trim().split('\n').filter(Boolean);
          entries = lines.slice(-limit).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
        }
      }
    } catch {}
    return json(res, 200, { ok: true, agent: name, entries });
  });

  // ── GET /api/scout/:name ───────────────────────────────────────────────
  app.on('GET', /^\/api\/scout\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const name = decodeURIComponent(m[1]);
    const q = await readQueue().catch(() => ({ items: [] }));
    const pending = (q.items || [])
      .filter(i => i.status === 'pending' && (i.assignee === name || i.assignee === 'all'))
      .slice(0, 3)
      .map(({ id, title, priority, description }) => ({ id, title, priority, description }));
    return json(res, 200, { ok: true, agent: name, pendingWork: pending });
  });

  // ── GET /api/crons ─────────────────────────────────────────────────────
  app.on('GET', '/api/crons', async (req, res) => {
    return json(res, 200, Object.values(cronStatus));
  });

  // ── POST /api/crons/:agent ─────────────────────────────────────────────
  app.on('POST', /^\/api\/crons\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const agent = decodeURIComponent(m[1]);
    const body = await readBody(req);
    if (!body.jobId) return json(res, 400, { error: 'jobId required' });
    const key = `${agent}/${body.jobId}`;
    cronStatus[key] = { ...body, agent, updatedAt: new Date().toISOString() };
    return json(res, 200, { ok: true, key });
  });

  // ── POST /api/cron-status ──────────────────────────────────────────────
  app.on('POST', '/api/cron-status', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.name) return json(res, 400, { error: 'name required' });
    cronStatus[body.name] = { ...body, ts: new Date().toISOString() };
    return json(res, 200, { ok: true });
  });

  // ── GET /api/cron-status ───────────────────────────────────────────────
  app.on('GET', '/api/cron-status', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    return json(res, 200, cronStatus);
  });

  // ── GET /api/provider-health ───────────────────────────────────────────
  app.on('GET', '/api/provider-health', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    return json(res, 200, providerHealth);
  });

  // ── POST /api/provider-health ──────────────────────────────────────────
  app.on('POST', '/api/provider-health', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.provider) return json(res, 400, { error: 'provider required' });
    providerHealth[body.provider] = { ...body, ts: new Date().toISOString() };
    return json(res, 200, { ok: true });
  });

  // ── POST /api/provider-health/:agent ──────────────────────────────────
  app.on('POST', /^\/api\/provider-health\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const agent = decodeURIComponent(m[1]);
    const body = await readBody(req);
    providerHealth[agent] = { ...body, agent, updatedAt: new Date().toISOString() };
    return json(res, 200, { ok: true });
  });

  // ── GET /api/geek/topology ─────────────────────────────────────────────
  app.on('GET', '/api/geek/topology', async (req, res) => {
    const nodes = [
      { id: 'rocky',          label: 'Rocky',          type: 'agent',          host: 'do-host1',    chips: ['RCC API :8789','WQ Dashboard :8788','RCC Brain','ClawBus hub','Tailscale proxy'] },
      { id: 'bullwinkle',     label: 'Bullwinkle',     type: 'agent',          host: 'puck',        chips: ['OpenClaw :18789','ClawBus :8788','launchd crons','disk free','uptime'] },
      { id: 'natasha',        label: 'Natasha',        type: 'agent',          host: 'sparky',      chips: ['OpenClaw :18789','ClawBus /bus→:18799','Milvus :19530','CUDA/RTX','Ollama :11434'] },
      { id: 'boris',          label: 'Boris',          type: 'agent',          host: 'l40-sweden',  chips: ['OpenClaw gateway','L40 GPU','Omniverse headless'] },
      { id: 'milvus',         label: 'Milvus',         type: 'shared-service', host: 'do-host1',   port: 19530 },
      { id: 'minio',          label: 'MinIO',          type: 'shared-service', host: 'do-host1',   port: 9000 },
      { id: 'searxng',        label: 'SearXNG',        type: 'shared-service', host: 'do-host1',   port: 8888 },
      { id: 'nvidia-gateway', label: 'NVIDIA Gateway', type: 'external',       url: 'inference-api.nvidia.com' },
      { id: 'github',         label: 'GitHub',         type: 'external',       url: 'api.github.com' },
      { id: 'mattermost',     label: 'Mattermost',     type: 'external',       url: 'chat.yourmom.photos' },
      { id: 'slack-omgjkh',   label: 'Slack (omgjkh)', type: 'external',       url: 'omgjkh.slack.com' },
      { id: 'slack-offtera',  label: 'Slack (offtera)', type: 'external',      url: 'offtera.slack.com' },
      { id: 'telegram',       label: 'Telegram',       type: 'external',       url: 'api.telegram.org' },
      { id: 'squirrelbus',    label: 'ClawBus',    type: 'bus',            host: 'do-host1' },
    ];
    const edges = [
      { from: 'rocky',      to: 'rcc-api',        type: 'persistent',  protocol: 'internal' },
      { from: 'bullwinkle', to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
      { from: 'natasha',    to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
      { from: 'boris',      to: 'rocky',           type: 'persistent',  protocol: 'heartbeat/HTTP' },
      { from: 'rocky',      to: 'milvus',          type: 'on-demand',   protocol: 'gRPC' },
      { from: 'rocky',      to: 'minio',           type: 'on-demand',   protocol: 'S3/HTTP' },
      { from: 'rocky',      to: 'searxng',         type: 'on-demand',   protocol: 'HTTP' },
      { from: 'rocky',      to: 'squirrelbus',     type: 'persistent',  protocol: 'JSONL/fanout' },
      { from: 'bullwinkle', to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
      { from: 'natasha',    to: 'squirrelbus',     type: 'on-demand',   protocol: 'HTTP' },
      { from: 'rocky',      to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
      { from: 'bullwinkle', to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
      { from: 'natasha',    to: 'nvidia-gateway',  type: 'on-demand',   protocol: 'HTTPS/OpenAI' },
      { from: 'rocky',      to: 'github',          type: 'on-demand',   protocol: 'HTTPS/REST' },
      { from: 'rocky',      to: 'mattermost',      type: 'on-demand',   protocol: 'HTTPS/REST' },
      { from: 'rocky',      to: 'slack-omgjkh',    type: 'persistent',  protocol: 'Socket Mode' },
      { from: 'rocky',      to: 'slack-offtera',   type: 'on-demand',   protocol: 'HTTPS/REST' },
      { from: 'rocky',      to: 'telegram',        type: 'on-demand',   protocol: 'HTTPS/Bot API' },
    ];
    const STALE_MS = 5 * 60 * 1000;
    const now = Date.now();
    const nodesWithStatus = nodes.map(n => {
      if (n.type !== 'agent') return n;
      const hb = heartbeats[n.id];
      if (!hb) return { ...n, status: 'offline', lastSeen: null };
      const age = now - new Date(hb.ts).getTime();
      const status = age < STALE_MS ? 'online' : age < 30 * 60 * 1000 ? 'stale' : 'offline';
      return { ...n, status, lastSeen: hb.ts };
    });
    const agentsData = await readAgents().catch(() => ({}));
    let busMessages = [];
    const busPath = new URL('../../../squirrelbus/bus.jsonl', import.meta.url).pathname;
    if (existsSync(busPath)) {
      try {
        const busRaw = await readFile(busPath, 'utf8');
        busMessages = busRaw.trim().split('\n').filter(Boolean).slice(-50).map(l => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
      } catch { /* ignore */ }
    }
    const heartbeatSummary = Object.entries(heartbeats).map(([agent, hb]) => ({ agent, ts: hb.ts, status: hb.status || 'online' }));
    const llmEndpoints = llmRegistry.serialize();
    return json(res, 200, { nodes: nodesWithStatus, edges, agents: agentsData, busMessages, heartbeatSummary, llmEndpoints });
  });

  // ── GET /api/geek/stream ───────────────────────────────────────────────
  app.on('GET', '/api/geek/stream', async (req, res) => {
    res.writeHead(200, {
      'Content-Type':  'text/event-stream',
      'Cache-Control': 'no-cache',
      'Connection':    'keep-alive',
      'Access-Control-Allow-Origin': '*',
    });
    res.write(`data: ${JSON.stringify({ type: 'connected' })}\n\n`);
    geekSseClients.add(res);
    const keepalive = setInterval(() => {
      try { res.write(': keepalive\n\n'); } catch { clearInterval(keepalive); geekSseClients.delete(res); }
    }, 15000);
    req.on('close', () => { clearInterval(keepalive); geekSseClients.delete(res); });
  });

  // ── GET /api/llms ──────────────────────────────────────────────────────
  app.on('GET', '/api/llms', async (req, res, _m, url) => {
    const onlyFresh = url.searchParams.get('fresh') === '1';
    const type      = url.searchParams.get('type') || null;
    const backend   = url.searchParams.get('backend') || null;
    let endpoints = llmRegistry.serialize();
    if (onlyFresh) endpoints = endpoints.filter(e => e.fresh);
    if (type)      endpoints = endpoints.filter(e => e.modelTypes?.includes(type) || e.models?.some(m => m.type === type));
    if (backend)   endpoints = endpoints.filter(e => e.backend === backend);
    return json(res, 200, endpoints);
  });

  // ── GET /api/llms/best ─────────────────────────────────────────────────
  app.on('GET', '/api/llms/best', async (req, res, _m, url) => {
    const model  = url.searchParams.get('model')  || null;
    const type   = url.searchParams.get('type')   || 'chat';
    const tag    = url.searchParams.get('tag')    || null;
    const agent  = url.searchParams.get('agent')  || null;
    const result = llmRegistry.best({ model, type, tag, agent });
    if (!result) return json(res, 404, { error: 'No matching LLM endpoint available', params: { model, type, tag } });
    return json(res, 200, result);
  });

  // ── GET /api/llms/:agent ───────────────────────────────────────────────
  app.on('GET', /^\/api\/llms\/([^/]+)$/, async (req, res, m) => {
    const agent = decodeURIComponent(m[1]);
    const entry = llmRegistry.get(agent);
    if (!entry) return json(res, 404, { error: 'LLM endpoint not found for agent' });
    return json(res, 200, { ...entry, fresh: (Date.now() - new Date(entry.updatedAt).getTime()) < 30 * 60 * 1000 });
  });

  // ── POST /api/llms ─────────────────────────────────────────────────────
  app.on('POST', '/api/llms', async (req, res) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    try {
      const entry = llmRegistry.advertise(body);
      console.log(`[rcc-api] LLM advertised: ${entry.agent} → ${entry.models.length} model(s) at ${entry.baseUrl}`);
      broadcastGeekEvent('llm_advertise', entry.agent, 'rcc', `${entry.agent} serving ${entry.models.map(m=>m.name).join(', ')}`);
      return json(res, 200, { ok: true, entry });
    } catch (err) {
      return json(res, 400, { error: err.message });
    }
  });

  // ── PATCH /api/llms/:agent ─────────────────────────────────────────────
  app.on('PATCH', /^\/api\/llms\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const agent = decodeURIComponent(m[1]);
    const existing = llmRegistry.get(agent);
    if (!existing) return json(res, 404, { error: 'LLM endpoint not found for agent' });
    const body = await readBody(req);
    const merged = { ...existing, ...body, agent };
    try {
      const entry = llmRegistry.advertise(merged);
      return json(res, 200, { ok: true, entry });
    } catch (err) {
      return json(res, 400, { error: err.message });
    }
  });

  // ── DELETE /api/llms/:agent ────────────────────────────────────────────
  app.on('DELETE', /^\/api\/llms\/([^/]+)$/, async (req, res, m) => {
    if (!isAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const agent = decodeURIComponent(m[1]);
    const removed = llmRegistry.remove(agent);
    return json(res, 200, { ok: true, removed });
  });

  // ── GET /api/llms/:agent/models ────────────────────────────────────────
  app.on('GET', /^\/api\/llms\/([^/]+)\/models$/, async (req, res, m) => {
    const agent = decodeURIComponent(m[1]);
    const entry = llmRegistry.get(agent);
    if (!entry) return json(res, 404, { error: 'LLM endpoint not found for agent' });
    return json(res, 200, entry.models);
  });

  // ── Bootstrap / onboard routes ─────────────────────────────────────────

  // ── POST /api/bootstrap/token ──────────────────────────────────────────
  app.on('POST', '/api/bootstrap/token', async (req, res) => {
    if (!isAdminAuthed(req)) return json(res, 401, { error: 'Unauthorized' });
    const body = await readBody(req);
    if (!body.agent) return json(res, 400, { error: 'agent required' });
    const ttl = body.ttlSeconds || 3600;
    const role = body.role || 'agent';
    const { randomUUID } = await import('crypto');
    const token = `rcc-bootstrap-${body.agent}-${randomUUID().slice(0, 8)}`;
    const expiresAt = new Date(Date.now() + ttl * 1000).toISOString();
    bootstrapTokens.set(token, { agent: body.agent, role, expiresAt: Date.now() + ttl * 1000, used: false });
    saveBootstrapTokens();
    return json(res, 200, { ok: true, bootstrapToken: token, agent: body.agent, role, expiresAt,
      onboardCmd: `curl -fsSL "${RCC_PUBLIC_URL}/api/onboard?token=${token}" | bash` });
  });

  // ── GET /api/mesh ──────────────────────────────────────────────────────
  app.on('GET', '/api/mesh', async (req, res) => {
    const now = Date.now();
    if (!state._meshCache) state._meshCache = { data: null, ts: 0 };
    const MESH_TTL = 30 * 1000;
    if (state._meshCache.data && (now - state._meshCache.ts) < MESH_TTL) {
      return json(res, 200, state._meshCache.data);
    }
    const meshAgents = ['rocky','natasha','boris','bullwinkle','peabody','sherman','snidely','dudley'];
    const allAgents = await readAgents().catch(() => ({}));
    const nodes = [];
    for (const [name, reg] of Object.entries(allAgents)) {
      const hb = heartbeats[name] || {};
      const lastSeen = hb.ts || reg.lastSeen || null;
      const gapMs    = lastSeen ? now - new Date(lastSeen).getTime() : null;
      const status   = !lastSeen ? 'offline'
                     : gapMs < 3 * 60 * 1000   ? 'online'
                     : gapMs < 15 * 60 * 1000  ? 'away'
                     :                           'offline';
      if (status === 'offline' && !reg.gpu && !meshAgents.includes(name)) continue;
      nodes.push({
        name,
        host:      reg.host   || hb.host   || name,
        status,
        lastSeen,
        gpu:       reg.gpu    || hb.gpu    || false,
        gpu_model: reg.gpu_model || hb.gpu_model || null,
        gpu_count: reg.gpu_count || hb.gpu_count || null,
        vllm:      reg.vllm   || hb.vllm   || false,
        vllm_port: reg.vllm_port || hb.vllm_port || null,
      });
    }

    let vibeSlots = null;
    try {
      const ctrl = new AbortController();
      setTimeout(() => ctrl.abort(), 3000);
      const r = await fetch('http://127.0.0.1:8789/api/agentos/slots', {
        signal: ctrl.signal,
        headers: { Authorization: `Bearer ${process.env.RCC_AGENT_TOKEN || ''}` },
      });
      if (r.ok) vibeSlots = (await r.json()).vibe_engine || null;
    } catch (_) {}

    let spawnLog = [];
    try {
      const busPath = process.env.BUS_LOG_PATH || '/home/jkh/rockyandfriends/rcc/data/bus.jsonl';
      const { readFileSync } = await import('fs');
      const lines = readFileSync(busPath, 'utf8').trim().split('\n').filter(Boolean);
      spawnLog = lines
        .map(l => { try { return JSON.parse(l); } catch { return null; } })
        .filter(m => m && (m.type === 'EVT_AGENT_SPAWNED' || (m.payload && m.payload.type === 'EVT_AGENT_SPAWNED')))
        .slice(-10);
    } catch (_) {}

    const llmEndpoints = llmRegistry.serialize().map(e => ({
      agent: e.agent,
      baseUrl: e.baseUrl,
      models: e.models.map(m => m.name),
      fresh: (Date.now() - new Date(e.updatedAt).getTime()) < 30 * 60 * 1000,
    }));

    const meshData = {
      ok: true,
      ts: new Date().toISOString(),
      nodes,
      vibe_engine: vibeSlots,
      spawn_log: spawnLog,
      llm_endpoints: llmEndpoints,
    };
    state._meshCache = { data: meshData, ts: now };
    return json(res, 200, meshData);
  });
}
