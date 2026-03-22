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

// ── Config ─────────────────────────────────────────────────────────────────
const PORT         = parseInt(process.env.RCC_PORT || '8789', 10);
const QUEUE_PATH   = process.env.QUEUE_PATH || '../../workqueue/queue.json';
const AGENTS_PATH  = process.env.AGENTS_PATH || './agents.json';
const AUTH_TOKENS  = new Set((process.env.RCC_AUTH_TOKENS || '').split(',').map(t => t.trim()).filter(Boolean));
const START_TIME   = Date.now();

// ── In-memory heartbeats ───────────────────────────────────────────────────
const heartbeats = {};

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

      const item = {
        id: body.id || `wq-API-${Date.now()}`,
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
      };
      if (!q.items) q.items = [];
      q.items.push(item);
      await writeQueue(q);
      return json(res, 201, { ok: true, item });
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
