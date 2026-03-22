/**
 * rcc/api — Rocky Command Center REST API
 *
 * Single source of truth for the work queue, agent registry, and heartbeats.
 * Agents talk to this instead of maintaining local queue copies.
 *
 * Port: RCC_PORT env var (default 8789)
 * Auth: Bearer token — must be in RCC_AUTH_TOKENS (comma-separated)
 */

import { createServer } from 'http';
import { readFile, writeFile } from 'fs/promises';
import { existsSync } from 'fs';
import { Brain, createRequest } from '../brain/index.mjs';
import { Pump } from '../scout/pump.mjs';
import { learnLesson, queryLessons, formatLessonsForContext, receiveLessonFromBus, seedKnownLessons } from '../lessons/index.mjs';

// ── Config ─────────────────────────────────────────────────────────────────
const PORT            = parseInt(process.env.RCC_PORT || '8789', 10);
const QUEUE_PATH      = process.env.QUEUE_PATH    || '../../workqueue/queue.json';
const AGENTS_PATH     = process.env.AGENTS_PATH   || './agents.json';
const REPOS_PATH      = process.env.REPOS_PATH    || './repos.json';
const PROJECTS_PATH   = process.env.PROJECTS_PATH || './projects.json';
const RCC_PUBLIC_URL  = process.env.RCC_PUBLIC_URL || 'http://YOUR_PUBLIC_IP:8789';
const AUTH_TOKENS  = new Set((process.env.RCC_AUTH_TOKENS || '').split(',').map(t => t.trim()).filter(Boolean));
const START_TIME   = Date.now();

// ── Stale claim thresholds (ms) by executor type ───────────────────────────
// claude_cli: real coding agents, can run 60-90min on complex tasks
// gpu: render jobs, can run hours
// inference_key: fast LLM calls, should finish in minutes
const STALE_THRESHOLDS = {
  claude_cli:    parseInt(process.env.STALE_CLAUDE_MS    || String(120 * 60 * 1000), 10), // 2h
  gpu:           parseInt(process.env.STALE_GPU_MS       || String(6  * 60 * 60 * 1000), 10), // 6h
  inference_key: parseInt(process.env.STALE_INFERENCE_MS || String(30 * 60 * 1000), 10), // 30min
  default:       parseInt(process.env.STALE_DEFAULT_MS   || String(60 * 60 * 1000), 10), // 1h
};

// ── In-memory heartbeats ───────────────────────────────────────────────────
const heartbeats = {};

// ── Projects I/O ─────────────────────────────────────────────────────────
async function readProjects() {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return [];
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeProjects(data) {
  const p = new URL(PROJECTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

function projectUrl(fullName) {
  return `${RCC_PUBLIC_URL}/projects/${encodeURIComponent(fullName)}`;
}

function buildProjectFromRepo(repo) {
  return {
    id:            repo.full_name,
    display_name:  repo.display_name || repo.full_name.split('/')[1],
    description:   repo.description || '',
    github_url:    `https://github.com/${repo.full_name}`,
    rcc_url:       projectUrl(repo.full_name),
    issue_tracker: repo.issue_tracker_url ? `https://${repo.issue_tracker_url}` : `https://github.com/${repo.full_name}/issues`,
    slack_channels: repo.ownership?.slack_channel
      ? [{ workspace: repo.ownership.slack_workspace || 'omgjkh', channel_id: repo.ownership.slack_channel }]
      : [],
    triaging_agent: repo.ownership?.triaging_agent || 'rocky',
    enabled:        repo.enabled !== false,
    kind:           repo.kind || 'personal',
    scouts:         repo.scouts || [],
    notes:          repo.notes || '',
    registeredAt:   repo.registeredAt || new Date().toISOString(),
    updatedAt:      repo.updatedAt || new Date().toISOString(),
  };
}

// ── Repo helpers ───────────────────────────────────────────────────────────
function repoOwnershipSummary(repo) {
  if (!repo.ownership) return { kind: repo.kind || 'personal', label: repo.full_name.split('/')[0] };
  const o = repo.ownership;
  if (o.model === 'sole') {
    return { kind: 'personal', label: o.owner || repo.full_name.split('/')[0], sole: true };
  }
  // team/org: list contributor logins
  const contributors = Array.isArray(o.contributors)
    ? o.contributors.map(c => typeof c === 'string' ? c : c.github)
    : [];
  return {
    kind: repo.kind || 'team',
    label: contributors.slice(0, 3).join(', ') + (contributors.length > 3 ? ` +${contributors.length - 3}` : ''),
    contributors,
    slack_channel: o.slack_channel || null,
  };
}

// ── Brain (lazy init) ─────────────────────────────────────────────────────
let brain = null;
async function getBrain() {
  if (!brain) {
    brain = new Brain();
    await brain.init();
    brain.start();
  }
  return brain;
}

// ── Pump (lazy init) ──────────────────────────────────────────────────────
let pump = null;
function getPump() {
  if (!pump) {
    pump = new Pump();
    pump.start();
  }
  return pump;
}

// ── Auth ───────────────────────────────────────────────────────────────────
function isAuthed(req) {
  if (AUTH_TOKENS.size === 0) return true; // no tokens configured = open (dev mode)
  const auth = req.headers['authorization'] || '';
  const token = auth.replace(/^Bearer\s+/i, '').trim();
  return AUTH_TOKENS.has(token);
}

// ── Queue I/O ──────────────────────────────────────────────────────────────
async function readQueue() {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return { items: [], completed: [] };
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeQueue(data) {
  const p = new URL(QUEUE_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── Agent registry I/O ────────────────────────────────────────────────────
async function readAgents() {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  if (!existsSync(p)) return {};
  return JSON.parse(await readFile(p, 'utf8'));
}

async function writeAgents(data) {
  const p = new URL(AGENTS_PATH, import.meta.url).pathname;
  await writeFile(p, JSON.stringify(data, null, 2));
}

// ── HTTP helpers ───────────────────────────────────────────────────────────
function json(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' });
  res.end(payload);
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', chunk => { body += chunk; if (body.length > 1e6) reject(new Error('Body too large')); });
    req.on('end', () => { try { resolve(body ? JSON.parse(body) : {}); } catch { reject(new Error('Invalid JSON')); } });
    req.on('error', reject);
  });
}

// ── Router ─────────────────────────────────────────────────────────────────
async function handleRequest(req, res) {
  const url = new URL(req.url, `http://localhost`);
  const path = url.pathname;
  const method = req.method;

  // CORS preflight
  if (method === 'OPTIONS') {
    res.writeHead(204, { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': 'Authorization, Content-Type', 'Access-Control-Allow-Methods': 'GET, POST, PATCH, DELETE, OPTIONS' });
    return res.end();
  }

  try {
    // ── Public endpoints ────────────────────────────────────────────────

    if (method === 'GET' && path === '/health') {
      const b = brain;
      const q = await readQueue();
      return json(res, 200, {
        ok: true,
        uptime: Math.floor((Date.now() - START_TIME) / 1000),
        agentCount: Object.keys(heartbeats).length,
        queueDepth: (q.items || []).filter(i => !['completed','cancelled'].includes(i.status)).length,
        lastBrainTick: b?.state?.lastTick || null,
        version: '0.1.0',
      });
    }

    if (method === 'GET' && path === '/api/queue') {
      const q = await readQueue();
      return json(res, 200, { items: q.items || [], completed: q.completed || [] });
    }

    if (method === 'GET' && path === '/api/agents') {
      const agents = await readAgents();
      const result = Object.entries(agents).map(([name, agent]) => ({
        ...agent,
        heartbeat: heartbeats[name] || null,
      }));
      return json(res, 200, result);
    }

    if (method === 'GET' && path === '/api/heartbeats') {
      return json(res, 200, heartbeats);
    }

    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── Item detail (public read) ─────────────────────────────────────────
    const itemDetailMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'GET' && itemDetailMatch) {
      const id = decodeURIComponent(itemDetailMatch[1]);
      const q = await readQueue();
      const item = [...(q.items||[]), ...(q.completed||[])].find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      return json(res, 200, item);
    }

    // ── Auth-required endpoints ───────────────────────────────────────────
    if (!isAuthed(req)) {
      return json(res, 401, { error: 'Unauthorized' });
    }

    // ── POST /api/queue — create item ─────────────────────────────────────
    if (method === 'POST' && path === '/api/queue') {
      const body = await readBody(req);
      if (!body.title) return json(res, 400, { error: 'title required' });
      const q = await readQueue();
      // Infer preferred_executor if not specified
      const inferExecutor = (b) => {
        if (b.preferred_executor) return b.preferred_executor;
        const tags = b.tags || [];
        if (tags.includes('gpu') || tags.includes('render') || tags.includes('simulation')) return 'gpu';
        if (tags.includes('reasoning') || tags.includes('code') || tags.includes('complex')) return 'claude_cli';
        if (tags.includes('heartbeat') || tags.includes('status') || tags.includes('poll')) return 'inference_key';
        // Default: claude_cli for assignee-specific tasks, inference_key for housekeeping
        return (b.assignee && b.assignee !== 'all') ? 'claude_cli' : 'inference_key';
      };

      // Prevent ID collisions — if a caller supplies an ID that already exists
      // (in either active items or completed), generate a fresh one instead.
      const allIds = new Set([...(q.items||[]), ...(q.completed||[])].map(i => i.id));
      let itemId = body.id || `wq-API-${Date.now()}`;
      if (body.id && allIds.has(body.id)) {
        itemId = `wq-API-${Date.now()}`;
        console.warn(`[rcc-api] ID collision on "${body.id}" — reassigned to "${itemId}"`);
      }

      const item = {
        id: itemId,
        itemVersion: 1,
        created: new Date().toISOString(),
        source: body.source || 'api',
        assignee: body.assignee || 'all',
        priority: body.priority || 'normal',
        status: 'pending',
        title: body.title,
        description: body.description || '',
        notes: body.notes || '',
        preferred_executor: inferExecutor(body),  // claude_cli | inference_key | gpu
        journal: [],
        choices: body.choices || [],
        choiceRecorded: null,
        votes: [],
        attempts: 0,
        maxAttempts: body.maxAttempts || 3,
        claimedBy: null,
        claimedAt: null,
        completedAt: null,
        result: null,
        tags: body.tags || [],
        // Scout dedup key — preserved for itemAlreadyExists() checks
        scout_key: body.scout_key || null,
        repo: body.repo || null,
      };
      if (!q.items) q.items = [];
      q.items.push(item);
      await writeQueue(q);
      return json(res, 201, { ok: true, item });
    }

    // ── GET /api/queue/stale — list stale claims ──────────────────────────
    if (method === 'GET' && path === '/api/queue/stale') {
      const q = await readQueue();
      const now = Date.now();
      const stale = (q.items || []).filter(item => {
        if (item.status !== 'in-progress' || !item.claimedAt) return false;
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        return (now - new Date(item.claimedAt).getTime()) > threshold;
      }).map(item => {
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        const age = now - new Date(item.claimedAt).getTime();
        return { ...item, staleMs: age, thresholdMs: threshold, staleMin: Math.round(age / 60000) };
      });
      return json(res, 200, { stale, count: stale.length, thresholds: STALE_THRESHOLDS });
    }

    // ── POST /api/queue/expire-stale — server-side stale reset ───────────
    if (method === 'POST' && path === '/api/queue/expire-stale') {
      const q = await readQueue();
      const now = Date.now();
      let reset = 0;
      for (const item of (q.items || [])) {
        if (item.status !== 'in-progress' || !item.claimedAt) continue;
        const threshold = STALE_THRESHOLDS[item.preferred_executor] || STALE_THRESHOLDS.default;
        if ((now - new Date(item.claimedAt).getTime()) > threshold) {
          const prevAgent = item.claimedBy;
          item.status = 'pending';
          item.claimedBy = null;
          item.claimedAt = null;
          item.attempts = (item.attempts || 0) + 1;
          if (!item.journal) item.journal = [];
          item.journal.push({
            ts: new Date().toISOString(),
            author: 'rcc-api',
            type: 'stale-reset',
            text: `Stale claim reset (was ${prevAgent}, threshold: ${threshold/60000}min for ${item.preferred_executor || 'default'})`,
          });
          reset++;
        }
      }
      if (reset > 0) await writeQueue(q);
      return json(res, 200, { ok: true, reset });
    }

    // ── PATCH /api/item/:id ───────────────────────────────────────────────
    const patchMatch = path.match(/^\/api\/item\/([^/]+)$/);
    if (method === 'PATCH' && patchMatch) {
      const id = decodeURIComponent(patchMatch[1]);
      const body = await readBody(req);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const allowed = ['title','description','priority','assignee','status','notes','choices','claimedBy','claimedAt','result','completedAt'];
      const now = new Date().toISOString();
      const changed = [];
      for (const field of allowed) {
        if (body[field] !== undefined && body[field] !== item[field]) {
          changed.push(`${field}: ${JSON.stringify(item[field])} → ${JSON.stringify(body[field])}`);
          item[field] = body[field];
        }
      }
      if (changed.length) {
        if (!item.journal) item.journal = [];
        item.journal.push({ ts: now, author: body._author || 'api', type: 'status-change', text: `Updated: ${changed.join('; ')}` });
        item.itemVersion = (item.itemVersion || 0) + 1;
        // Auto-archive: move completed/cancelled items from items[] to completed[]
        if (item.status === 'completed' || item.status === 'cancelled') {
          q.items = q.items.filter(i => i.id !== item.id);
          if (!q.completed) q.completed = [];
          q.completed.push(item);
        }
        await writeQueue(q);
      }
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/item/:id/comment ────────────────────────────────────────
    const commentMatch = path.match(/^\/api\/item\/([^/]+)\/comment$/);
    if (method === 'POST' && commentMatch) {
      const id = decodeURIComponent(commentMatch[1]);
      const body = await readBody(req);
      const text = (body.text || '').trim();
      if (!text) return json(res, 400, { error: 'text required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      if (!item.journal) item.journal = [];
      const entry = { ts: new Date().toISOString(), author: body.author || 'api', type: 'comment', text };
      item.journal.push(entry);
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, entry });
    }

    // ── POST /api/item/:id/choice ─────────────────────────────────────────
    const choiceMatch = path.match(/^\/api\/item\/([^/]+)\/choice$/);
    if (method === 'POST' && choiceMatch) {
      const id = decodeURIComponent(choiceMatch[1]);
      const body = await readBody(req);
      if (!body.choice) return json(res, 400, { error: 'choice required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      if (!item.journal) item.journal = [];
      const entry = { ts: now, author: body.author || 'api', type: 'choice', text: `Choice: [${body.choice}] ${body.choiceLabel || ''}` };
      item.journal.push(entry);
      item.choiceRecorded = { choice: body.choice, label: body.choiceLabel || '', ts: now };
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, entry, choiceRecorded: item.choiceRecorded });
    }

    // ── POST /api/item/:id/ai-comment ─────────────────────────────────────
    const aiMatch = path.match(/^\/api\/item\/([^/]+)\/ai-comment$/);
    if (method === 'POST' && aiMatch) {
      const id = decodeURIComponent(aiMatch[1]);
      const body = await readBody(req);
      const prompt = (body.prompt || '').trim();
      if (!prompt) return json(res, 400, { error: 'prompt required' });
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      const now = new Date().toISOString();
      if (!item.journal) item.journal = [];
      const userEntry = { ts: now, author: body.author || 'jkh', type: 'ai', text: `✨ ${prompt}` };
      item.journal.push(userEntry);

      // Queue to brain for async processing, or call inline if brain available
      let aiText = '(queued for brain processing)';
      try {
        const b = await getBrain();
        const brainReq = createRequest({
          messages: [
            { role: 'system', content: `You are Rocky, helping with work item "${item.title}". Be concise.` },
            { role: 'user', content: prompt }
          ],
          maxTokens: 500,
          priority: 'normal',
          metadata: { itemId: id },
        });
        // Await completion inline (with timeout)
        const result = await Promise.race([
          new Promise(resolve => {
            const onComplete = (r) => { if (r.id === brainReq.id) { b.off('completed', onComplete); resolve(r.result); } };
            b.on('completed', onComplete);
            b.enqueue(brainReq);
          }),
          new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 20000))
        ]);
        aiText = result;
      } catch (e) {
        aiText = `(brain error: ${e.message})`;
      }

      const aiEntry = { ts: new Date().toISOString(), author: '🐿️ Rocky', type: 'ai', text: aiText };
      item.journal.push(aiEntry);
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, userEntry, aiEntry });
    }

    // ── POST /api/agents/register ─────────────────────────────────────────
    if (method === 'POST' && path === '/api/agents/register') {
      const body = await readBody(req);
      if (!body.name) return json(res, 400, { error: 'name required' });
      const agents = await readAgents();
      const token = `rcc-agent-${body.name}-${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`;
      agents[body.name] = {
        name: body.name,
        host: body.host || 'unknown',
        type: body.type || 'full',           // full | container | local | spark
        token,
        registeredAt: new Date().toISOString(),
        lastSeen: null,
        // Worker capabilities — declared at registration, updated via PATCH /api/agents/:name
        capabilities: {
          claude_cli: body.capabilities?.claude_cli ?? false,
          claude_cli_model: body.capabilities?.claude_cli_model || null,
          inference_key: body.capabilities?.inference_key ?? true,
          inference_provider: body.capabilities?.inference_provider || 'nvidia',
          gpu: body.capabilities?.gpu ?? false,
          gpu_model: body.capabilities?.gpu_model || null,
          gpu_count: body.capabilities?.gpu_count || 0,
          gpu_vram_gb: body.capabilities?.gpu_vram_gb || 0,
        },
        billing: {
          claude_cli: body.billing?.claude_cli || 'fixed',
          inference_key: body.billing?.inference_key || 'metered',
          gpu: body.billing?.gpu || 'fixed',
        },
      };
      await writeAgents(agents);
      AUTH_TOKENS.add(token);
      return json(res, 201, { ok: true, token, agent: { ...agents[body.name], token } });
    }

    // ── PATCH /api/agents/:name — update capabilities ─────────────────────
    const agentPatchMatch = path.match(/^\/api\/agents\/([^/]+)$/);
    if (method === 'PATCH' && agentPatchMatch) {
      const name = decodeURIComponent(agentPatchMatch[1]);
      const body = await readBody(req);
      const agents = await readAgents();
      if (!agents[name]) return json(res, 404, { error: 'Agent not found' });
      if (body.capabilities) Object.assign(agents[name].capabilities, body.capabilities);
      if (body.billing) Object.assign(agents[name].billing, body.billing);
      if (body.host) agents[name].host = body.host;
      if (body.type) agents[name].type = body.type;
      await writeAgents(agents);
      return json(res, 200, { ok: true, agent: agents[name] });
    }

    // ── POST /api/heartbeat/:agent ────────────────────────────────────────
    const hbMatch = path.match(/^\/api\/heartbeat\/([^/]+)$/);
    if (method === 'POST' && hbMatch) {
      const agent = decodeURIComponent(hbMatch[1]);
      const body = await readBody(req);
      heartbeats[agent] = { agent, ts: new Date().toISOString(), status: 'online', ...body };
      // Update agent lastSeen
      const agents = await readAgents();
      if (agents[agent]) {
        agents[agent].lastSeen = heartbeats[agent].ts;
        await writeAgents(agents);
      }
      return json(res, 200, { ok: true });
    }

    // ── POST /api/complete/:id ────────────────────────────────────────────
    const completeMatch = path.match(/^\/api\/complete\/([^/]+)$/);
    if (method === 'POST' && completeMatch) {
      const id = decodeURIComponent(completeMatch[1]);
      const q = await readQueue();
      const item = q.items?.find(i => i.id === id);
      if (!item) return json(res, 404, { error: 'Item not found' });
      item.status = 'completed';
      item.completedAt = new Date().toISOString();
      item.itemVersion = (item.itemVersion || 0) + 1;
      await writeQueue(q);
      return json(res, 200, { ok: true, item });
    }

    // ── POST /api/brain/request — submit LLM request to brain ────────────
    if (method === 'POST' && path === '/api/brain/request') {
      const body = await readBody(req);
      if (!body.messages || !Array.isArray(body.messages)) return json(res, 400, { error: 'messages array required' });
      const b = await getBrain();
      const req2 = createRequest({
        messages: body.messages,
        maxTokens: body.maxTokens || 1024,
        priority: body.priority || 'normal',
        callbackUrl: body.callbackUrl || null,
        metadata: body.metadata || {},
      });
      const id = await b.enqueue(req2);
      return json(res, 202, { ok: true, requestId: id, status: 'queued' });
    }

    // ── GET /api/brain/status ─────────────────────────────────────────────
    if (method === 'GET' && path === '/api/brain/status') {
      const b = brain;
      if (!b) return json(res, 200, { ok: true, status: 'not started' });
      return json(res, 200, b.getStatus());
    }

    // ── POST /api/lessons — record a lesson ──────────────────────────────
    if (method === 'POST' && path === '/api/lessons') {
      const body = await readBody(req);
      if (!body.domain || !body.symptom || !body.fix) return json(res, 400, { error: 'domain, symptom, fix required' });
      const lesson = await learnLesson({ ...body, agent: body.agent || 'api' });
      return json(res, 201, { ok: true, lesson });
    }

    // ── GET /api/lessons?domain=X&q=keyword+keyword ───────────────────────
    if (method === 'GET' && path.startsWith('/api/lessons')) {
      const domain = url.searchParams.get('domain') || 'general';
      const q = (url.searchParams.get('q') || '').split(/\s+/).filter(Boolean);
      const limit = parseInt(url.searchParams.get('limit') || '5', 10);
      const lessons = await queryLessons({ domain, keywords: q, limit });
      const context = url.searchParams.get('format') === 'context' ? formatLessonsForContext(lessons) : null;
      return json(res, 200, { lessons, context, count: lessons.length });
    }

    // ── GET /api/repos ────────────────────────────────────────────────────
    if (method === 'GET' && path === '/api/repos') {
      const repos = await getPump().listRepos();
      // Enrich with kind/ownership summary for dashboard
      const enriched = repos.map(r => ({
        ...r,
        kind: r.kind || 'personal',
        ownership_summary: repoOwnershipSummary(r),
      }));
      return json(res, 200, enriched);
    }

    // ── GET /api/repos/:name or PATCH /api/repos/:name ───────────────────
    const repoSingleMatch = path.match(/^\/api\/repos\/([^/]+\/[^/]+)$/);
    if (repoSingleMatch) {
      const fullName = decodeURIComponent(repoSingleMatch[1]);
      if (method === 'GET') {
        const repos = await getPump().listRepos();
        const repo = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Repo not found' });
        return json(res, 200, { ...repo, ownership_summary: repoOwnershipSummary(repo) });
      }
      if (method === 'PATCH') {
        const body = await readBody(req);
        const repo = await getPump().patchRepo(fullName, body);
        return json(res, 200, { ok: true, repo });
      }
    }

    // ── POST /api/repos/register ──────────────────────────────────────────
    if (method === 'POST' && path === '/api/repos/register') {
      const body = await readBody(req);
      if (!body.full_name) return json(res, 400, { error: 'full_name required (e.g. owner/repo)' });
      const repo = await getPump().registerRepo(body);
      return json(res, 201, { ok: true, repo });
    }

    // ── GET /api/projects — list all projects (derived from repos + projects.json) ──
    if (method === 'GET' && path === '/api/projects') {
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Merge: repos.json is source of truth; projects.json holds Slack channel overrides
      const projectMap = new Map(projects.map(p => [p.id, p]));
      const result = repos
        .filter(r => r.enabled !== false)
        .map(r => {
          const base    = buildProjectFromRepo(r);
          const overlay = projectMap.get(r.full_name) || {};
          return { ...base, ...overlay };
        });
      return json(res, 200, result);
    }

    // ── GET /api/projects/:owner/:repo — single project ───────────────────
    const projectDetailMatch = path.match(/^\/api\/projects\/([^/]+\/[^/]+)$/);
    if (method === 'GET' && projectDetailMatch) {
      const fullName = decodeURIComponent(projectDetailMatch[1]);
      const repos    = await getPump().listRepos();
      const repo     = repos.find(r => r.full_name === fullName);
      if (!repo) return json(res, 404, { error: 'Project not found' });
      const projects = await readProjects();
      const overlay  = projects.find(p => p.id === fullName) || {};
      const base     = buildProjectFromRepo(repo);
      return json(res, 200, { ...base, ...overlay });
    }

    // ── POST /api/projects/:owner/:repo/channel — register a Slack channel ─
    const projectChannelMatch = path.match(/^\/api\/projects\/([^/]+\/[^/]+)\/channel$/);
    if (method === 'POST' && projectChannelMatch) {
      const fullName = decodeURIComponent(projectChannelMatch[1]);
      const body     = await readBody(req);
      if (!body.channel_id || !body.workspace) return json(res, 400, { error: 'channel_id and workspace required' });
      const projects = await readProjects();
      let project    = projects.find(p => p.id === fullName);
      if (!project) {
        const repos = await getPump().listRepos();
        const repo  = repos.find(r => r.full_name === fullName);
        if (!repo) return json(res, 404, { error: 'Project not found' });
        project = buildProjectFromRepo(repo);
        projects.push(project);
      }
      if (!project.slack_channels) project.slack_channels = [];
      // Upsert by workspace
      const existing = project.slack_channels.find(c => c.workspace === body.workspace);
      if (existing) {
        existing.channel_id = body.channel_id;
        existing.channel_name = body.channel_name || existing.channel_name;
        existing.updatedAt  = new Date().toISOString();
      } else {
        project.slack_channels.push({
          workspace:    body.workspace,
          channel_id:   body.channel_id,
          channel_name: body.channel_name || null,
          addedAt:      new Date().toISOString(),
        });
      }
      project.updatedAt = new Date().toISOString();
      await writeProjects(projects);
      // Also update repos.json for the primary workspace
      const pump = getPump();
      const repos = await pump.listRepos();
      const repo  = repos.find(r => r.full_name === fullName);
      if (repo) {
        if (!repo.ownership) repo.ownership = {};
        if (!repo.ownership.slack_channel || body.workspace === 'omgjkh') {
          repo.ownership.slack_channel   = body.channel_id;
          repo.ownership.slack_workspace = body.workspace;
          await pump.patchRepo(fullName, { ownership: repo.ownership });
        }
      }
      return json(res, 200, { ok: true, project });
    }

    // ── GET /api/context?channel=CXXXX — get project context for a Slack channel ──
    if (method === 'GET' && path === '/api/context') {
      const channelId = url.searchParams.get('channel');
      if (!channelId) return json(res, 400, { error: 'channel query param required' });
      const repos    = await getPump().listRepos();
      const projects = await readProjects();
      // Search repos.json first
      let repo = repos.find(r =>
        r.ownership?.slack_channel === channelId
      );
      // Then projects.json (may have multiple workspaces)
      if (!repo) {
        const projectEntry = projects.find(p =>
          (p.slack_channels || []).some(c => c.channel_id === channelId)
        );
        if (projectEntry) repo = repos.find(r => r.full_name === projectEntry.id);
      }
      if (!repo) return json(res, 404, { error: 'No project associated with this channel' });
      const overlay  = projects.find(p => p.id === repo.full_name) || {};
      const project  = { ...buildProjectFromRepo(repo), ...overlay };
      // Include recent queue items for this project
      const q        = await readQueue();
      const repoItems = (q.items || []).filter(i =>
        i.tags?.includes(repo.full_name) ||
        i.title?.toLowerCase().includes(repo.full_name.split('/')[1].toLowerCase())
      ).slice(-10);
      return json(res, 200, { project, recentItems: repoItems });
    }

    // ── POST /api/bus/receive — handle incoming SquirrelBus messages ──────
    if (method === 'POST' && path === '/api/bus/receive') {
      const body = await readBody(req);
      if (body.type === 'lesson') {
        await receiveLessonFromBus(body);
        return json(res, 200, { ok: true });
      }
      return json(res, 200, { ok: true, ignored: true });
    }

    // ── POST /api/repos/scan — trigger immediate scan ─────────────────────
    if (method === 'POST' && path === '/api/repos/scan') {
      const created = await getPump().scan();
      return json(res, 200, { ok: true, itemsCreated: created });
    }

    return json(res, 404, { error: 'Not found' });

  } catch (err) {
    console.error('[rcc-api] Error:', err.message);
    json(res, 500, { error: err.message });
  }
}

// ── Start server ───────────────────────────────────────────────────────────
export function startServer(port = PORT) {
  const server = createServer(handleRequest);
  server.listen(port, '0.0.0.0', () => {
    console.log(`[rcc-api] 🐿️ RCC API running on http://0.0.0.0:${port}`);
    console.log(`[rcc-api] Auth: ${AUTH_TOKENS.size > 0 ? `${AUTH_TOKENS.size} token(s) configured` : 'OPEN (no tokens set)'}`);
  });
  return server;
}

if (process.argv[1] === new URL(import.meta.url).pathname) {
  startServer();
  process.on('SIGTERM', () => process.exit(0));
  process.on('SIGINT',  () => process.exit(0));
}
